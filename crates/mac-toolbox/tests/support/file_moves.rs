//! One observable contract, exercised through the SDK on both block filesystems.
use mac_toolbox::{Cnid, Error, Point, System, Timestamp, VolumeId,
                  files::{CatalogInfo, DirectoryCursor, Fork, ROOT_ID}};

fn fork(system: &System<'_>, volume: VolumeId, id: Cnid, kind: Fork) -> Vec<u8>
{
    let mut file = system.files().open(volume, id, kind).unwrap();
    let mut bytes = vec![0; file.len().unwrap() as usize];
    for chunk in bytes.chunks_mut(64 * 1024)
    {
        assert_eq!(file.read(chunk).unwrap(), chunk.len());
    }
    bytes
}

fn same_object(before: &CatalogInfo, after: &CatalogInfo)
{
    assert_eq!(after.id, before.id);
    assert_eq!(after.volume, before.volume);
    assert_eq!(after.name(), before.name());
    assert_eq!(after.finder_info, before.finder_info);
    assert_eq!(after.flags, before.flags);
    assert_eq!((after.created, after.modified, after.backup),
               (before.created, before.modified, before.backup));
    assert_eq!((after.data_length, after.resource_length),
               (before.data_length, before.resource_length));
}

pub fn exercise(system: &System<'_>, volume: VolumeId, id: Cnid, when: Timestamp)
                -> (Cnid, CatalogInfo, Vec<u8>, Vec<u8>)
{
    let files = system.files();
    files.set_icon_location_at(volume,
                               id,
                               Some(Point { h: 191,
                                            v: 237 }),
                               when)
         .unwrap();
    files.set_comment_at(volume, id, "Position and comment travel with identity", when).unwrap();
    let before = files.catalog(volume, id).unwrap();
    let data = fork(system, volume, id, Fork::Data);
    let resources = fork(system, volume, id, Fork::Resource);
    let destination = files.new_folder_at(volume, ROOT_ID, "Destination", when).unwrap();
    let source_watch = files.watch_directory(volume, before.parent).unwrap();
    let dest_watch = files.watch_directory(volume, destination.id).unwrap();
    let mut scan = DirectoryCursor::default();
    files.scan_directory(volume, before.parent, &mut scan).unwrap();

    // Existing protection remains in force while any app has a fork open.
    let held = files.open(volume, id, Fork::Resource).unwrap();
    assert_eq!(files.move_into_at(volume, id, destination.id, when), Err(Error::ServiceBusy));
    drop(held);
    assert!(source_watch.poll().unwrap().is_none());
    assert!(dest_watch.poll().unwrap().is_none());
    let moved = files.move_into_at(volume, id, destination.id, when).unwrap();
    same_object(&before, &moved);
    assert_eq!(moved.parent, destination.id);
    assert_eq!(files.catalog(volume, id).unwrap(), moved);
    assert_eq!(files.lookup(volume, before.parent, before.name()), Err(Error::NotFound));
    assert_eq!(files.lookup(volume, destination.id, before.name()).unwrap().id, id);
    let mut comment = String::new();
    files.comment(volume, id, &mut comment).unwrap();
    assert_eq!(comment, "Position and comment travel with identity");
    assert_eq!(fork(system, volume, id, Fork::Data), data);
    assert_eq!(fork(system, volume, id, Fork::Resource), resources);
    assert!(source_watch.poll().unwrap().is_some());
    assert!(dest_watch.poll().unwrap().is_some());
    assert!(source_watch.poll().unwrap().is_none());
    assert!(dest_watch.poll().unwrap().is_none());
    assert_eq!(files.scan_directory(volume, before.parent, &mut scan), Err(Error::ScanStale));

    // Collision and cycle checks precede publication and preserve metadata.
    files.new_folder_at(volume, ROOT_ID, before.name(), when).unwrap();
    assert_eq!(files.move_into_at(volume, id, ROOT_ID, when), Err(Error::AlreadyExists));
    let child = files.new_folder_at(volume, destination.id, "Child", when).unwrap();
    assert_eq!(files.move_into_at(volume, destination.id, child.id, when), Err(Error::BadArgument));
    assert_eq!(files.move_into_at(volume, destination.id, destination.id, when),
               Err(Error::BadArgument));
    assert_eq!(files.move_into_at(volume, child.id, id, when), Err(Error::BadArgument));
    assert_eq!(files.move_into_at(volume, ROOT_ID, destination.id, when), Err(Error::BadArgument));
    assert_eq!(files.catalog(volume, id).unwrap(), moved);

    // Moving a folder keeps every child's ID and parent, and its saved position.
    files.set_icon_location_at(volume,
                               destination.id,
                               Some(Point { h: 40,
                                            v: 60 }),
                               when)
         .unwrap();
    let folder_before = files.catalog(volume, destination.id).unwrap();
    let container = files.new_folder_at(volume, ROOT_ID, "Container", when).unwrap();
    let folder_after = files.move_into_at(volume, destination.id, container.id, when).unwrap();
    same_object(&folder_before, &folder_after);
    assert_eq!(folder_after.parent, container.id);
    assert_eq!(files.catalog(volume, child.id).unwrap().parent, destination.id);
    assert_eq!(files.catalog(volume, id).unwrap(), moved);
    (destination.id, moved, data, resources)
}

pub fn persisted(system: &System<'_>, volume: VolumeId, expected: &CatalogInfo, data: &[u8],
                 resources: &[u8])
{
    let actual = system.files().catalog(volume, expected.id).unwrap();
    same_object(expected, &actual);
    assert_eq!(actual.parent, expected.parent);
    assert_eq!(fork(system, volume, expected.id, Fork::Data), data);
    assert_eq!(fork(system, volume, expected.id, Fork::Resource), resources);
    let mut comment = String::new();
    system.files().comment(volume, expected.id, &mut comment).unwrap();
    assert_eq!(comment, "Position and comment travel with identity");
}

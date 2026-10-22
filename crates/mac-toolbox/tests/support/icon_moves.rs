//! One group transaction and one placement contract for both native block FSs.
use mac_toolbox::{files::{IconMove, Placement, ROOT_ID},
                  *};
pub fn exercise(system: &System<'_>, volume: VolumeId, file: Cnid, when: Timestamp)
{
    let files = system.files();
    let original = files.catalog(volume, file).unwrap();
    let folder = files.new_folder_at(volume, ROOT_ID, "Group source", when).unwrap();
    let child = files.new_folder_at(volume, folder.id, "Child", when).unwrap();
    let folder = files.catalog(volume, folder.id).unwrap();
    let destination = files.new_folder_at(volume, ROOT_ID, "Group destination", when).unwrap();
    let source_watch = files.watch_directory(volume, original.parent).unwrap();
    let dest_watch = files.watch_directory(volume, destination.id).unwrap();
    let row = IconMove { id:              file,
                         expected_parent: original.parent,
                         parent:          destination.id,
                         placement:       Placement::At(Point { h: 97,
                                                                v: 141 }),
                         to_trash:        false };
    let cyclic = IconMove { id:              folder.id,
                            expected_parent: folder.parent,
                            parent:          child.id,
                            placement:       Placement::At(Point { h: 31,
                                                                   v: 41 }),
                            to_trash:        false };
    assert_eq!(files.move_icons_at(volume, &[row, cyclic], when), Err(Error::BadArgument));
    assert_eq!(files.catalog(volume, file).unwrap(), original);
    assert_eq!(files.catalog(volume, folder.id).unwrap(), folder);
    assert!(source_watch.poll().unwrap().is_none());
    assert!(dest_watch.poll().unwrap().is_none());
    assert_eq!(files.move_icons_at(volume, &[row, row], when), Err(Error::BadArgument));
    assert_eq!(files.move_icons_at(volume,
                                   &[IconMove { expected_parent: destination.id,
                                                ..row }],
                                   when),
               Err(Error::ScanStale));
    let second = IconMove { parent: destination.id,
                            ..cyclic };
    assert!(files.move_icons_at(volume, &[row, second], when).unwrap());
    let moved = files.catalog(volume, file).unwrap();
    assert_eq!(moved.parent, destination.id);
    assert_eq!(moved.saved_icon_location(),
               Some(Point { h: 97,
                            v: 141 }));
    assert_eq!((moved.created,
                moved.modified,
                moved.backup,
                moved.data_length,
                moved.resource_length),
               (original.created,
                original.modified,
                original.backup,
                original.data_length,
                original.resource_length));
    assert_eq!(files.catalog(volume, child.id).unwrap().parent, folder.id);
    assert!(source_watch.poll().unwrap().is_some());
    assert!(dest_watch.poll().unwrap().is_some());
    assert!(source_watch.poll().unwrap().is_none());
    assert!(dest_watch.poll().unwrap().is_none());
    let unchanged = [IconMove { expected_parent: destination.id,
                                ..row },
                     IconMove { expected_parent: destination.id,
                                ..second }];
    assert!(!files.move_icons_at(volume, &unchanged, when).unwrap());
    assert!(dest_watch.poll().unwrap().is_none());
    let trash = files.trash_directory_at(volume, true, when).unwrap();
    let to_trash = unchanged.map(|row| {
                                IconMove { parent: trash.id,
                                           placement: Placement::Clear,
                                           to_trash: true,
                                           ..row }
                            });
    assert!(files.move_icons_at(volume, &to_trash, when).unwrap());
    for id in [file, folder.id]
    {
        let item = files.catalog(volume, id).unwrap();
        assert_eq!(item.parent, trash.id);
        assert_eq!(item.put_away_parent(), destination.id);
        let restored = files.put_away_at(volume, id, when).unwrap();
        assert_eq!(restored.parent, destination.id);
    }
}

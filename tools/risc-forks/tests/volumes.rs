use std::{fs, path::PathBuf};

use mac_fork_file::{Fork, Kind, Metadata, Opened};
use risc_forks::{Volume,
                 io::{FileIo, Slice}};
struct Temp(PathBuf);
impl Temp
{
    fn new() -> Self
    {
        let mut id = [0; 8];
        getrandom::fill(&mut id).unwrap();
        let path = std::env::temp_dir().join(format!("risc-forks-{}-{:x}",
                                                     std::process::id(),
                                                     u64::from_le_bytes(id)));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp
{
    fn drop(&mut self)
    {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn volume() -> (Temp, Volume)
{
    let temp = Temp::new();
    let volume = Volume::initialize(&temp.0.join("share"), "Test Share").unwrap();
    (temp, volume)
}
#[test]
fn dual_forks_and_full_metadata_survive_native_host_bundle_roundtrip()
{
    let (temp, volume) = volume();
    let data: Vec<_> = (0..17001).map(|i| (i * 31) as u8).collect();
    let resource: Vec<_> = (0..7109).map(|i| (i * 17) as u8).collect();
    let mut metadata =
        Metadata::new(Kind::File, 16, [2; 16], "Café", 3_875_171_696_123_456).unwrap();
    metadata.modified += 91;
    metadata.backup = Some(0);
    metadata.attributes = 1;
    metadata.comment = "Complete metadata 🦀".into();
    metadata.finder_info = core::array::from_fn(|i| i as u8);
    let entry = volume.create("/",
                              &metadata.name,
                              Kind::File,
                              &mut Slice(&data),
                              &mut Slice(&resource),
                              Some(&metadata))
                      .unwrap();
    assert_eq!(volume.check().unwrap(), 2);
    let resolved = volume.resolve("/CAFE\u{301}").unwrap();
    assert_eq!(entry.metadata.catalog_id, resolved.metadata.catalog_id);
    let exported = temp.0.join("exported");
    volume.export("/café", &exported).unwrap();
    assert_eq!(fs::read(exported.join("data")).unwrap(), data);
    assert_eq!(fs::read(exported.join("resources")).unwrap(), resource);
    let other = Volume::initialize(&temp.0.join("other"), "Other Share").unwrap();
    let restored = other.import("/", &exported).unwrap();
    for value in [&entry.metadata, &restored.metadata]
    {
        assert_eq!(value.name, metadata.name);
        assert_eq!(value.created, metadata.created);
        assert_eq!(value.modified, metadata.modified);
        assert_eq!(value.backup, Some(0));
        assert_eq!(value.finder_info, metadata.finder_info);
        assert_eq!(value.attributes, metadata.attributes);
        assert_eq!(value.comment, metadata.comment);
    }
    assert_ne!(entry.metadata.volume, restored.metadata.volume);
    let again = temp.0.join("again");
    other.export("/CAFÉ", &again).unwrap();
    assert_eq!(fs::read(again.join("data")).unwrap(), data);
    assert_eq!(fs::read(again.join("resources")).unwrap(), resource);
}
#[test]
fn canonical_collisions_and_bad_requests_do_not_advance_catalog_state()
{
    let (_temp, volume) = volume();
    volume.create("/", "CAFÉ", Kind::File, &mut Slice(&[]), &mut Slice(&[]), None).unwrap();
    let before = volume.root().unwrap().metadata;
    assert!(volume.create("/", "cafe\u{301}", Kind::File, &mut Slice(&[]), &mut Slice(&[]), None)
                  .is_err());
    assert!(volume.create("/", "bad/name", Kind::File, &mut Slice(&[]), &mut Slice(&[]), None)
                  .is_err());
    assert!(volume.create("/", "Folder", Kind::Directory, &mut Slice(&[1]), &mut Slice(&[]), None)
                  .is_err());
    assert_eq!(volume.root().unwrap().metadata, before);
}
#[test]
fn nested_folders_keep_real_names_and_reserved_names_fail_before_publication()
{
    let (temp, volume) = volume();
    volume.create("/", "Documents", Kind::Directory, &mut Slice(&[]), &mut Slice(&[]), None)
          .unwrap();
    let name = "x".repeat(255);
    let entry =
        volume.create("/Documents", &name, Kind::File, &mut Slice(b"hello"), &mut Slice(&[]), None)
              .unwrap();
    let before = volume.root().unwrap().metadata;
    for invalid in
        ["CON", "AUX.txt", "LPT¹", "trailing.", "trailing ", "bad:name", ".system-risc-root"]
    {
        assert!(volume.create("/Documents",
                              invalid,
                              Kind::File,
                              &mut Slice(b"native"),
                              &mut Slice(&[]),
                              None)
                      .is_err());
        assert_eq!(volume.root().unwrap().metadata, before);
    }
    assert_eq!(volume.resolve(&format!("/documents/{name}")).unwrap().metadata.catalog_id,
               entry.metadata.catalog_id);
    assert!(temp.0.join("share/Documents").join(name).is_file());
    assert_eq!(volume.check().unwrap(), 3);
}
#[test]
fn copied_containers_are_adopted_without_aliasing_and_plain_files_stay_visible()
{
    let (temp, volume) = volume();
    let entry = volume.create("/",
                              "Original",
                              Kind::File,
                              &mut Slice(b"hello"),
                              &mut Slice(b"resource fork"),
                              None)
                      .unwrap();
    volume.create("/", "Folder", Kind::Directory, &mut Slice(&[]), &mut Slice(&[]), None).unwrap();
    let root = temp.0.join("share");
    fs::copy(root.join(&entry.path), root.join("Folder/Copy")).unwrap();
    fs::write(root.join("unwrapped.txt"), b"do not hide me").unwrap();
    let copy = volume.resolve("/Folder/Copy").unwrap();
    assert_ne!(copy.metadata.catalog_id, entry.metadata.catalog_id);
    assert_ne!(copy.metadata.identity, entry.metadata.identity);
    assert_eq!(copy.metadata.name, "Copy");
    assert_eq!(volume.check().unwrap(), 5);
    let exported = temp.0.join("copy");
    volume.export("/Folder/Copy", &exported).unwrap();
    assert_eq!(fs::read(exported.join("data")).unwrap(), b"hello");
    assert_eq!(fs::read(exported.join("resources")).unwrap(), b"resource fork");
    assert!(volume.children("/").unwrap().iter().any(|e| e.metadata.name == "unwrapped.txt"));
}

#[test]
fn a_snapshot_reports_external_in_place_edits_instead_of_mixing_revisions()
{
    let (temp, volume) = volume();
    let entry =
        volume.create("/", "File", Kind::File, &mut Slice(b"original"), &mut Slice(&[]), None)
              .unwrap();
    let path = temp.0.join("share").join(entry.path);
    let source = FileIo::snapshot(fs::File::open(&path).unwrap()).unwrap();
    let mut opened = Opened::open(source).unwrap();
    // An uncooperative external editor does not take the format's lock.
    use std::io::{Seek, SeekFrom, Write};
    let mut editor = fs::OpenOptions::new().write(true).open(path).unwrap();
    editor.seek(SeekFrom::Start(1024)).unwrap();
    editor.write_all(b"changed!").unwrap();
    editor.sync_all().unwrap();
    assert_eq!(opened.read(Fork::Data, 0, &mut [0; 8]), Err(mac_fork_file::Error::Stale));
}
#[test]
#[cfg(unix)]
fn symlinks_cannot_expose_files_outside_the_export_root()
{
    let (temp, volume) = volume();
    let outside = temp.0.join("outside");
    fs::write(&outside, b"private").unwrap();
    let name = "Private.txt";
    std::os::unix::fs::symlink(&outside, temp.0.join("share").join(name)).unwrap();
    assert!(volume.children("/").is_err());
    assert_eq!(fs::read(outside).unwrap(), b"private");
}

#[test]
fn hfs_b_and_host_containers_exchange_the_same_lossless_fork_bundles()
{
    let (temp, host) = volume();
    let mut metadata =
        Metadata::new(Kind::File, 16, [2; 16], "Preferences", 3_875_171_696_123_456).unwrap();
    metadata.comment = "Portable prefs".into();
    metadata.finder_info[..4].copy_from_slice(b"pref");
    metadata.backup = Some(0);
    host.create("/",
                &metadata.name,
                Kind::File,
                &mut Slice(b"{\"version\":1,\"value\":42}"),
                &mut Slice(b"resource fork bytes"),
                Some(&metadata))
        .unwrap();
    let first = temp.0.join("first");
    host.export("/Preferences", &first).unwrap();
    let image_path = temp.0.join("transfer.hfsb");
    let mut image = hfs_b::Image::create(&image_path, 16 * 1024 * 1024).unwrap();
    let partition = image.partition();
    mac_hfs_b::format(&mut image,
                      partition,
                      mac_hfs_b::FormatOptions { name:    "Transfer",
                                                 created:
                                                     mac_hfs_b::time::Timestamp(metadata.created),
                                                 uuid:    [3; 16] },
                      Default::default()).unwrap();
    let mut volume = mac_hfs_b::Volume::mount(&mut image, partition, Default::default()).unwrap();
    let entry = hfs_b::copy::copy_in(&mut volume, 2, &first, false).unwrap();
    assert_eq!(entry.created.0, metadata.created);
    assert_eq!(entry.backup, Some(mac_hfs_b::time::Timestamp(0)));
    let second = temp.0.join("second");
    hfs_b::copy::copy_out(&mut volume, entry.id, &second).unwrap();
    let restored = Volume::initialize(&temp.0.join("restored"), "Restored").unwrap();
    let final_entry = restored.import("/", &second).unwrap();
    assert_eq!(final_entry.metadata.created, metadata.created);
    assert_eq!(final_entry.metadata.comment, metadata.comment);
    let third = temp.0.join("third");
    restored.export("/Preferences", &third).unwrap();
    for fork in ["data", "resources"]
    {
        assert_eq!(fs::read(first.join(fork)).unwrap(), fs::read(third.join(fork)).unwrap());
    }
}

#[test]
#[cfg(unix)]
fn bundle_links_cannot_import_external_forks_or_metadata()
{
    use std::os::unix::fs::symlink;
    let (temp, source) = volume();
    source.create("/", "Test", Kind::File, &mut Slice(b"data"), &mut Slice(b"resources"), None)
          .unwrap();
    let bundle = temp.0.join("bundle");
    source.export("/Test", &bundle).unwrap();
    let destination = Volume::initialize(&temp.0.join("destination"), "Destination").unwrap();
    let before = destination.root().unwrap().metadata;
    for part in ["metadata.json", "data", "resources"]
    {
        let path = bundle.join(part);
        let outside = temp.0.join(format!("outside-{part}"));
        fs::rename(&path, &outside).unwrap();
        symlink(&outside, &path).unwrap();
        assert!(destination.import("/", &bundle).is_err(), "followed {part}");
        assert_eq!(destination.root().unwrap().metadata, before);
        fs::remove_file(&path).unwrap();
        fs::rename(outside, path).unwrap();
    }
    source.create("/", "Folder", Kind::Directory, &mut Slice(&[]), &mut Slice(&[]), None).unwrap();
    let directory = temp.0.join("directory-bundle");
    source.export("/Folder", &directory).unwrap();
    fs::remove_dir(directory.join("children")).unwrap();
    symlink(&temp.0, directory.join("children")).unwrap();
    assert!(destination.import("/", &directory).is_err());
    assert!(destination.children("/Folder").unwrap().is_empty());
}

#[test]
fn a_bulk_import_preserves_every_sibling_and_retains_collision_validation()
{
    let (temp, volume) = volume();
    let directory = temp.0.join("Many");
    fs::create_dir(&directory).unwrap();
    for index in 0u32..256
    {
        fs::write(directory.join(format!("Document-{index:03}")), index.to_le_bytes()).unwrap();
    }
    volume.import("/", &directory).unwrap();
    assert_eq!(volume.children("/Many").unwrap().len(), 256);
    assert_eq!(volume.check().unwrap(), 258);
    fs::write(directory.join("Café"), b"first normalization").unwrap();
    fs::write(directory.join("Cafe\u{301}"), b"normalization collision").unwrap();
    let other = Volume::initialize(&temp.0.join("other"), "Other").unwrap();
    assert!(other.import("/", &directory).is_err());
    other.check().unwrap();
}

#[test]
fn created_container_bindings_survive_reopen_and_host_rename_for_files_and_folders()
{
    let (temp, volume) = volume();
    let file = volume.create("/",
                             "Before",
                             Kind::File,
                             &mut Slice(b"data"),
                             &mut Slice(b"resources"),
                             None)
                     .unwrap();
    let directory = volume.create("/",
                                  "Folder Before",
                                  Kind::Directory,
                                  &mut Slice(&[]),
                                  &mut Slice(&[]),
                                  None)
                          .unwrap();
    let file_id = file.metadata.catalog_id;
    let directory_id = directory.metadata.catalog_id;
    // Windows refuses a parent rename while a descendant file is open. Close
    // the returned record snapshots before the external host rename.
    drop(file);
    drop(directory);
    fs::rename(temp.0.join("share/Before"), temp.0.join("share/After")).unwrap();
    fs::rename(temp.0.join("share/Folder Before"), temp.0.join("share/Folder After")).unwrap();
    drop(volume);
    let volume = Volume::open(&temp.0.join("share")).unwrap();
    assert_eq!(volume.resolve("/After").unwrap().metadata.catalog_id, file_id);
    assert_eq!(volume.resolve("/Folder After").unwrap().metadata.catalog_id, directory_id);
    // Copying the metadata marker is an import, not a transfer of identity.
    fs::create_dir(temp.0.join("share/Copied Folder")).unwrap();
    fs::copy(temp.0.join("share/Folder After").join(mac_fork_file::DIRECTORY_RECORD),
             temp.0.join("share/Copied Folder").join(mac_fork_file::DIRECTORY_RECORD)).unwrap();
    let copy = volume.resolve("/Copied Folder").unwrap();
    assert_ne!(copy.metadata.catalog_id, directory_id);
    assert_eq!(copy.metadata.name, "Copied Folder");
}

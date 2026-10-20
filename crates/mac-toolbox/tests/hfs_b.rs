//! Native disk acceptance through the public SDK and real resident services.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs,
          path::PathBuf,
          sync::atomic::{AtomicUsize, Ordering}};

use mac_hfs_b::{Budget, ForkKind, FormatOptions, Volume, time::Timestamp as DiskTime};
use mac_system::testing::{DiskActivity, DiskFaults, with_file_disk};
use mac_toolbox::{System, Timestamp,
                  files::{DataFileSpec, Filesystem, Fork, ROOT_ID, SeekFrom}};

static NEXT: AtomicUsize = AtomicUsize::new(0);
const WHEN: Timestamp = Timestamp::from_mac_microseconds(4_600_000_000_123_456);
struct Fixture
{
    dir:  PathBuf,
    disk: PathBuf,
    file: u32
}
impl Fixture
{
    fn new() -> Self
    {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../results/system7")
            .join(format!("hfs-b-sdk-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("pid"), std::process::id().to_string()).unwrap();
        let disk = dir.join("disk.hfsb");
        let mut image = hfs_b::Image::create(&disk, 32 * 1024 * 1024).unwrap();
        let partition = image.partition();
        mac_hfs_b::format(&mut image,
                          partition,
                          FormatOptions { name:    "Native Disk",
                                          created: DiskTime(WHEN.mac_microseconds().unwrap()),
                                          uuid:    [23; 16] },
                          Budget::default()).unwrap();
        let mut volume = Volume::mount(&mut image, partition, Budget::default()).unwrap();
        let file = volume.create(ROOT_ID,
                                 "cafe\u{301}",
                                 false,
                                 DiskTime(WHEN.mac_microseconds().unwrap()))
                         .unwrap()
                         .id;
        volume.write_fork(file,
                          ForkKind::Data,
                          0,
                          b"original data",
                          DiskTime(WHEN.mac_microseconds().unwrap()))
              .unwrap();
        // The resource fork exceeds the old whole-fork boot/resource ceiling.
        let chunk = vec![0x5a; 64 * 1024];
        for index in 0..81
        {
            volume.write_fork(file,
                              ForkKind::Resource,
                              index * chunk.len() as u64,
                              &chunk,
                              DiskTime(WHEN.mac_microseconds().unwrap()))
                  .unwrap();
        }
        volume.check().unwrap();
        Self { dir,
               disk,
               file }
    }

    fn run<R>(&self, readonly: bool, faults: DiskFaults, f: impl FnOnce(&System<'_>, u32) -> R)
              -> (R, DiskActivity)
    {
        with_file_disk(&self.disk, readonly, faults, |services| {
            let system = unsafe { System::from_raw(services) }.unwrap();
            let id = system.files().volume_at(0).unwrap().id;
            f(&system, id)
        }).unwrap()
    }
}
impl Drop for Fixture
{
    fn drop(&mut self)
    {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn native_names_precise_dates_and_large_forks_survive_real_remount()
{
    let fixture = Fixture::new();
    fixture.run(false, DiskFaults::default(), |system, id| {
               let files = system.files();
               let volume = files.volume(id).unwrap();
               assert_eq!(Filesystem::of(&volume).unwrap(), Filesystem::HfsB { version: 1 });
               assert_eq!(volume.created_at(), WHEN);
               assert!(volume.total_bytes > volume.free_bytes);
               assert!(files.volume(1).is_err(), "an arbitrary slot number is not a volume handle");
               let entry = files.lookup(id, ROOT_ID, "CAFÉ").unwrap();
               assert_eq!(entry.id, fixture.file);
               assert_eq!(entry.name(), "café");
               assert_eq!(entry.created_at(), WHEN);
               assert_eq!(entry.modified_at(), WHEN);
               assert!(WHEN.mac_seconds().is_none(), "this date is beyond classic HFS's range");
               let mut resource = files.open(id, entry.id, Fork::Resource).unwrap();
               assert_eq!(resource.len().unwrap(), 81 * 65536);
               resource.seek(SeekFrom::Start(4 * 1024 * 1024 + 123)).unwrap();
               let mut bytes = [0; 513];
               assert_eq!(resource.read(&mut bytes).unwrap(), bytes.len());
               assert_eq!(bytes, [0x5a; 513]);
               drop(resource);
               let later =
                   Timestamp::from_mac_microseconds(WHEN.mac_microseconds().unwrap() + 1_000_001);
               files.rename(id, entry.id, "Résumé 😀", later).unwrap();
               files.set_comment(id, entry.id, "metadata", later).unwrap();
               files.set_stationery(id, entry.id, true, later).unwrap();
               files.set_locked(id, entry.id, true, later).unwrap();
               assert_eq!(files.catalog(id, entry.id).unwrap().modified_at(), WHEN);
               let mut path = String::new();
               files.path(id, entry.id, &mut path).unwrap();
               assert!(path.ends_with("Résumé 😀"));
               let long = format!("{}abc", "😀".repeat(63));
               let boundary = files.new_folder(id, ROOT_ID, &long, WHEN).unwrap();
               assert_eq!(files.rename(id, boundary.id, &"😀".repeat(64), WHEN),
                          Err(mac_toolbox::Error(mac_toolbox::BAD_ARGUMENT)));
               assert_eq!(files.catalog(id, boundary.id).unwrap().name(), long);
               // Input exceeds 255 bytes; NFC storage is exactly 255 and valid.
               let decomposed = format!("{}e\u{301}", "a".repeat(253));
               let renamed = files.rename(id, boundary.id, &decomposed, WHEN).unwrap();
               assert_eq!(renamed.name(), format!("{}é", "a".repeat(253)));
               let cjk = format!("{}ab", "文".repeat(66));
               assert_eq!(cjk.len(), 200);
               files.new_folder(id, ROOT_ID, &cjk, WHEN).unwrap();
               let latin_i = files.new_folder(id, ROOT_ID, "I", WHEN).unwrap();
               assert_eq!(files.lookup(id, ROOT_ID, "i").unwrap().id, latin_i.id);
               let dotless = files.new_folder(id, ROOT_ID, "ı", WHEN).unwrap();
               assert_ne!(latin_i.id, dotless.id);
               let dotted = files.new_folder(id, ROOT_ID, "İ", WHEN).unwrap();
               assert_eq!(files.lookup(id, ROOT_ID, "i\u{307}").unwrap().id, dotted.id);
               let marks = files.new_folder(id, ROOT_ID, "a\u{315}\u{300}", WHEN).unwrap();
               assert_eq!(marks.name(), "à\u{315}");
               assert_eq!(files.lookup(id, ROOT_ID, "A\u{300}\u{315}").unwrap().id, marks.id);
               assert_eq!(files.new_folder(id, ROOT_ID, &"😀".repeat(64), WHEN),
                          Err(mac_toolbox::Error(mac_toolbox::BAD_ARGUMENT)));
               for (stored, lookup) in [("Straße", "STRASSE"), ("ﬁ", "fi"), ("cafe\u{301}", "CAFÉ")]
               {
                   let folder = files.new_folder(id, ROOT_ID, stored, WHEN).unwrap();
                   assert_eq!(files.lookup(id, ROOT_ID, lookup).unwrap().id, folder.id);
                   assert_eq!(files.new_folder(id, ROOT_ID, lookup, WHEN),
                              Err(mac_toolbox::Error(mac_toolbox::ALREADY_EXISTS)));
               }
               files.new_folder(id, ROOT_ID, "①", WHEN).unwrap();
               files.new_folder(id, ROOT_ID, "1", WHEN).unwrap();
           });
    fixture.run(true, DiskFaults::default(), |system, id| {
               let e = system.files().lookup(id, ROOT_ID, "RÉSUMÉ 😀").unwrap();
               assert_eq!(e.id, fixture.file);
               assert_eq!(e.created_at(), WHEN);
               assert_eq!(e.modified_at(), WHEN);
           });
}

fn save(system: &System<'_>, volume: u32, expected: Option<u32>, data: &[u8])
        -> mac_toolbox::files::CatalogInfo
{
    let mut writer = system.files()
                           .create_data(DataFileSpec { volume,
                                                       parent: ROOT_ID,
                                                       name: "Export 😀.json",
                                                       replacing: expected,
                                                       file_type: *b"TEXT",
                                                       creator: *b"RSED",
                                                       modified: WHEN },
                                        data.len())
                           .unwrap();
    writer.write_all(data).unwrap();
    writer.finish().unwrap()
}
#[test]
fn save_replace_trash_restore_and_empty_are_durable()
{
    let fixture = Fixture::new();
    let (cnid, _) =
        fixture.run(false, DiskFaults::default(), |system, id| {
                   let entry = save(system, id, None, b"{\"first\":true}");
                   let next = save(system, id, Some(entry.id), b"{\"second\":true}");
                   assert_eq!(entry.id, next.id);
                   assert_eq!(next.created_at(), WHEN);
                   system.files().set_comment(id, entry.id, "A Unicode comment 😀", WHEN).unwrap();
                   let mut text = String::new();
                   system.files().comment(id, entry.id, &mut text).unwrap();
                   assert_eq!(text, "A Unicode comment 😀");
                   system.files().move_to_trash(id, entry.id, WHEN).unwrap();
                   assert_eq!(system.files().trash_usage(id).unwrap().items, 1);
                   entry.id
               });
    fixture.run(false, DiskFaults::default(), |system, id| {
               let restored = system.files().put_away(id, cnid, WHEN).unwrap();
               assert_eq!(restored.parent, ROOT_ID);
               let mut file = system.files().open(id, cnid, Fork::Data).unwrap();
               let mut bytes = [0; 15];
               assert_eq!(file.read(&mut bytes).unwrap(), bytes.len());
               assert_eq!(&bytes, b"{\"second\":true}");
               drop(file);
               system.files().move_to_trash(id, cnid, WHEN).unwrap();
               assert_eq!(system.files().empty_trash(id, WHEN).unwrap(), 1);
           });
    fixture.run(true, DiskFaults::default(), |system, id| {
               assert_eq!(system.files().catalog(id, cnid),
                          Err(mac_toolbox::Error(mac_toolbox::NOT_FOUND)));
               assert_eq!(system.files().trash_usage(id).unwrap().items, 0);
               assert_eq!(system.files().new_folder(id, ROOT_ID, "denied", WHEN),
                          Err(mac_toolbox::Error(mac_toolbox::READ_ONLY)));
           });
}

#[test]
fn standard_save_as_accepts_native_unicode_and_normalizes_its_result()
{
    use mac_system::{testing::standard_file_action, toolbox::standard_file::Action};
    use mac_toolbox::standard_file::{Location, Progress};
    let fixture = Fixture::new();
    fixture.run(false, DiskFaults::default(), |system, volume| {
               let name = format!("{}cafe\u{301}.json", "😀".repeat(50));
               let dialog = system.standard_file()
                                  .begin_put(Location { volume,
                                                        directory: ROOT_ID },
                                             "Save As:",
                                             &name)
                                  .unwrap();
               standard_file_action(Action::Accept, 0).unwrap();
               let Progress::Selected(selection) = dialog.poll().unwrap()
               else
               {
                   panic!("Save As did not accept native name");
               };
               assert_eq!(selection.name(), format!("{}café.json", "😀".repeat(50)));
               assert_eq!(selection.entry.volume, volume);
               assert_eq!(selection.entry.id, 0);
               assert!(!selection.replacing);
               assert!(system.files().lookup(volume, ROOT_ID, selection.name()).is_err(),
                       "choosing a destination must not create it");
           });
}

#[test]
fn native_partial_writes_and_flush_failures_do_not_publish_catalog_changes()
{
    let fixture = Fixture::new();
    let before = fs::read(&fixture.disk).unwrap();
    for fault in [DiskFaults { write_at: Some(3),
                               partial_write: true,
                               ..Default::default() },
                  DiskFaults { flush_at: Some(1),
                               ..Default::default() }]
    {
        fixture.run(false, fault, |system, id| {
                   assert_eq!(system.files().new_folder(id, ROOT_ID, "Unpublished 😀", WHEN),
                              Err(mac_toolbox::Error(mac_toolbox::IO_ERROR)));
               });
        assert_eq!(fs::read(&fixture.disk).unwrap(),
                   before,
                   "reported failure must restore partially written catalog sectors: {fault:?}");
        fixture.run(true, DiskFaults::default(), |system, id| {
                   assert_eq!(system.files().lookup(id, ROOT_ID, "Unpublished 😀"),
                              Err(mac_toolbox::Error(mac_toolbox::NOT_FOUND)));
                   assert_eq!(system.files().lookup(id, ROOT_ID, "CAFÉ").unwrap().id, fixture.file);
               });
        let mut disk = hfs_b::Image::open(&fixture.disk, false).unwrap();
        let p = disk.partition();
        Volume::mount(&mut disk, p, Default::default()).unwrap().check().unwrap();
    }
}

#[test]
fn mixed_native_and_classic_mounts_preserve_format_and_startup_identity()
{
    let fixture = Fixture::new();
    let native = fs::read(&fixture.disk).unwrap();
    let classic = include_bytes!("data/files/dates-2026.hfs");
    for order in [false, true]
    {
        let disks: [&[u8]; 2] = if order { [classic, &native] } else { [&native, classic] };
        mac_system::testing::with_disks(&disks, u32::from(order), |services| {
            let system = unsafe { System::from_raw(services) }.unwrap();
            let files = system.files();
            assert_eq!(files.volume_count().unwrap(), 2);
            let a = files.volume_at(0).unwrap();
            let b = files.volume_at(1).unwrap();
            assert_ne!(a.id, b.id);
            for volume in [a, b]
            {
                if Filesystem::of(&volume).unwrap() == (Filesystem::HfsB { version: 1 })
                {
                    assert_eq!(volume.startup, 1);
                    assert_eq!(files.lookup(volume.id, ROOT_ID, "CAFÉ").unwrap().id, fixture.file);
                }
                else
                {
                    assert_eq!(Filesystem::of(&volume).unwrap(), Filesystem::Hfs);
                    assert_eq!(volume.startup, 0);
                    assert_eq!(files.lookup(volume.id, ROOT_ID, "Folder/Café")
                                    .unwrap()
                                    .created_at()
                                    .unix_seconds(),
                               1_767_225_600);
                }
            }
        });
    }
}


#[path = "../../mac-hfs-b/tests/support/large.rs"]
mod large;
#[test]
#[ignore = "streams over 4 GiB of allocated writes; run with --release --ignored"]
fn allocated_native_fork_crosses_two_and_four_gib_through_public_sdk()
{
    use std::io::{Seek, SeekFrom as HostSeek, Write};
    let (disk, id) = large::allocated_fork();
    assert!(disk.written > 1u64 << 32);
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../results/system7")
                                                       .join(format!("hfs-b-large-sdk-{}",
                                                                     std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("pid"), std::process::id().to_string()).unwrap();
    let fixture = Fixture { disk: dir.join("disk.hfsb"),
                            dir,
                            file: id };
    let mut image = fs::File::create(&fixture.disk).unwrap();
    image.set_len(disk.sectors * 512).unwrap();
    // Persist the adapter's equivalent sparse host image, not a fabricated
    // length-only catalog. Every logical data block was allocated and written.
    for (sector, data) in disk.data
    {
        image.seek(HostSeek::Start(sector * 512)).unwrap();
        image.write_all(&data).unwrap();
    }
    image.sync_all().unwrap();
    drop(image);
    fixture.run(true, DiskFaults::default(), |system, volume| {
               let files = system.files();
               assert!(files.volume(volume).unwrap().total_bytes > 1u64 << 32);
               assert_eq!(files.catalog(volume, id).unwrap().data_length, (1u64 << 32) + 4096);
               let mut fork = files.open(volume, id, Fork::Data).unwrap();
               assert_eq!(fork.len().unwrap(), (1u64 << 32) + 4096);
               for boundary in [1u64 << 31, 1u64 << 32]
               {
                   fork.seek(SeekFrom::Start(boundary - 8)).unwrap();
                   let mut bytes = [0; 16];
                   assert_eq!(fork.read(&mut bytes).unwrap(), bytes.len());
                   assert_eq!(&bytes, b"boundary-crossed");
                   assert_eq!(fork.position().unwrap(), boundary + 8);
               }
           });
}


#[test]
fn repeated_catalog_queries_do_not_repeat_physical_metadata_reads()
{
    let fixture = Fixture::new();
    let (_, single) = fixture.run(true, DiskFaults::default(), |system, id| {
                                 assert_eq!(system.files().catalog(id, fixture.file).unwrap().id,
                                            fixture.file);
                             });
    let (_, repeated) =
        fixture.run(true, DiskFaults::default(), |system, id| {
                   for _ in 0..128
                   {
                       assert_eq!(system.files().catalog(id, fixture.file).unwrap().id,
                                  fixture.file);
                   }
               });
    assert!(single.reads > 0);
    assert_eq!(repeated.reads, single.reads, "unchanged catalog data must stay cached");
    assert_eq!((repeated.writes, repeated.flushes), (0, 0));
}

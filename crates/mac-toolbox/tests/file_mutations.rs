//! Real durable HFS mutations through aggregate SDK and native File Manager gate.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs,
          path::PathBuf,
          process::Command,
          sync::atomic::{AtomicUsize, Ordering}};

use mac_system::testing::{DiskFaults, with_file_disk};
use mac_toolbox::{System, Timestamp,
                  files::{Fork, ROOT_ID}};
static NEXT: AtomicUsize = AtomicUsize::new(0);
const WHEN: Timestamp = Timestamp::from_unix_seconds(1_792_326_896);
mod file_write_cases;
struct Fixture
{
    root: PathBuf,
    dir:  PathBuf,
    disk: PathBuf
}
impl Fixture
{
    fn new() -> Self
    {
        Self::with_size(8 * 1024 * 1024)
    }

    fn with_size(size: u64) -> Self
    {
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
        let dir = root.join("results/system7").join(format!("file-writes-{}-{}",
                                                            std::process::id(),
                                                            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(dir.join("hfs-state")).unwrap();
        let disk = dir.join("disk.hfs");
        fs::File::create(&disk).unwrap().set_len(size).unwrap();
        let result = Self { root,
                            dir,
                            disk };
        result.hfs("hformat", &["-l", "Write Test", result.disk.to_str().unwrap()]);
        result.hfs("hmount", &[result.disk.to_str().unwrap()]);
        result.hfs("hmkdir", &[":Source"]);
        result.hfs("hmkdir", &[":Other"]);
        let data: Vec<_> = (0..1700).map(|n| (n * 17) as u8).collect();
        let resources: Vec<_> = (0..900).map(|n| (n * 31) as u8).collect();
        let payload=mac_formats::macbinary::encode(&mac_formats::macbinary::MacFile{name:b"Sample".to_vec(),file_type:*b"TEXT",creator:*b"TEST",flags:0,created:WHEN.mac_seconds().unwrap(),modified:WHEN.mac_seconds().unwrap(),data,resources}).unwrap();
        let file = result.dir.join("sample.bin");
        fs::write(&file, payload).unwrap();
        result.hfs("hcopy", &["-m", file.to_str().unwrap(), ":Source:"]);
        result.hfs("humount", &[]);
        result
    }

    fn hfs(&self, name: &str, args: &[&str]) -> String
    {
        let output=Command::new(self.root.join(".toolchains/hfsutils/rootfs/usr/bin").join(name)).args(args).env("HOME",self.dir.join("hfs-state")).env("TZ","UTC").output().unwrap();
        assert!(output.status.success(), "{name}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn run<R>(&self, readonly: bool, faults: DiskFaults, run: impl FnOnce(&System<'_>) -> R)
              -> (R, mac_system::testing::DiskActivity)
    {
        with_file_disk(&self.disk, readonly, faults, |services| {
            let system = unsafe { System::from_raw(services) }.unwrap();
            run(&system)
        }).unwrap()
    }

    fn verify_hfs(&self, path: &str) -> String
    {
        self.hfs("hmount", &[self.disk.to_str().unwrap()]);
        let listing = self.hfs("hls", &["-la", path]);
        self.hfs("humount", &[]);
        listing
    }
}
fn fork_bytes(system: &System<'_>, id: u32, fork: Fork) -> Vec<u8>
{
    let mut file = system.files().open(1, id, fork).unwrap();
    let mut bytes = vec![0; file.len().unwrap() as usize];
    assert_eq!(file.read(&mut bytes).unwrap(), bytes.len());
    bytes
}
#[test]
fn folders_trash_putaway_and_recursive_empty_persist_across_remounts()
{
    let fixture = Fixture::new();
    let ((source, sample, before, original_data, original_resources), _) =
        fixture.run(false, DiskFaults::default(), |system| {
                   let files = system.files();
                   assert_eq!(files.volume(1).unwrap().read_only, 0);
                   let source = files.lookup(1, ROOT_ID, "Source").unwrap();
                   let sample = files.lookup(1, source.id, "Sample").unwrap();
                   assert!(matches!(files.trash_directory(1, false),
                                    Err(mac_toolbox::Error(mac_toolbox::NOT_FOUND))));
                   let data = fork_bytes(system, sample.id, Fork::Data);
                   let resources = fork_bytes(system, sample.id, Fork::Resource);
                   let free = files.volume(1).unwrap().free_bytes;
                   let folder = files.new_folder(1, source.id, "Café", WHEN).unwrap();
                   assert_eq!(folder.created_at(), WHEN);
                   assert_eq!(folder.modified_at(), WHEN);
                   assert_eq!(files.catalog(1, source.id).unwrap().child_count, 2);
                   let moved = files.move_to_trash(1, sample.id, WHEN).unwrap();
                   let trash = files.trash_directory(1, false).unwrap();
                   assert_eq!(trash.finder_flags() & 0x1000, 0x1000);
                   assert_eq!(moved.id, sample.id);
                   assert_eq!(moved.parent, trash.id);
                   assert_eq!(moved.put_away_parent(), source.id);
                   assert_eq!(files.volume(1).unwrap().free_bytes, free);
                   let usage = files.trash_usage(1).unwrap();
                   assert_eq!(usage.items, 1);
                   assert!(usage.allocated_bytes >= 2600);
                   (source.id, sample.id, free, data, resources)
               });
    fixture.verify_hfs(":Trash:");
    fixture.run(false, DiskFaults::default(), |system| {
               let files = system.files();
               let moved = files.catalog(1, sample).unwrap();
               assert_eq!(moved.put_away_parent(), source);
               assert_eq!(fork_bytes(system, sample, Fork::Data), original_data);
               assert_eq!(fork_bytes(system, sample, Fork::Resource), original_resources);
               let restored = files.put_away(1, sample, WHEN).unwrap();
               assert_eq!(restored.id, sample);
               assert_eq!(restored.parent, source);
               assert_eq!(restored.put_away_parent(), source);
               assert_eq!(files.empty_trash(1, WHEN).unwrap(), 0);
               files.move_to_trash(1, source, WHEN).unwrap();
               assert_eq!(files.empty_trash(1, WHEN).unwrap(), 3);
               assert!(matches!(files.catalog(1, sample),
                                Err(mac_toolbox::Error(mac_toolbox::NOT_FOUND))));
               assert!(files.volume(1).unwrap().free_bytes > before);
           });
    fixture.run(true, DiskFaults::default(), |system| {
               let files = system.files();
               assert!(files.lookup(1, ROOT_ID, "Source").is_err());
               assert_eq!(files.trash_directory(1, false).unwrap().child_count, 0);
               assert!(files.lookup(1, ROOT_ID, "Other").unwrap().is_directory());
           });
    fixture.verify_hfs(":");
}
#[test]
fn readonly_busy_conflict_and_invalid_names_never_modify_disk()
{
    let fixture = Fixture::new();
    let before = fs::read(&fixture.disk).unwrap();
    let (_, activity) = fixture.run(true, DiskFaults::default(), |system| {
                                   assert_eq!(system.files().new_folder(1, ROOT_ID, "No", WHEN),
                                              Err(mac_toolbox::Error(mac_toolbox::READ_ONLY)));
                               });
    assert_eq!(activity.writes, 0);
    assert_eq!(fs::read(&fixture.disk).unwrap(), before);
    fixture.run(false, DiskFaults::default(), |system| {
               let files = system.files();
               assert_eq!(files.new_folder(1, ROOT_ID, "Source", WHEN),
                          Err(mac_toolbox::Error(mac_toolbox::ALREADY_EXISTS)));
               assert_eq!(files.new_folder(1, ROOT_ID, "bad:name", WHEN),
                          Err(mac_toolbox::Error(mac_toolbox::BAD_ARGUMENT)));
               assert_eq!(files.move_to_trash(1, ROOT_ID, WHEN),
                          Err(mac_toolbox::Error(mac_toolbox::ACCESS_DENIED)));
               let item = files.lookup(1, ROOT_ID, "Source/Sample").unwrap();
               let open = files.open(1, item.id, Fork::Data).unwrap();
               assert_eq!(files.move_to_trash(1, item.id, WHEN),
                          Err(mac_toolbox::Error(mac_toolbox::SERVICE_BUSY)));
               drop(open);
           });
    assert_eq!(fs::read(&fixture.disk).unwrap(), before);
}

#[test]
fn exiting_apps_release_their_open_forks_without_removing_other_apps_protection()
{
    use mac_system::testing::files::{close_owner, with_owner};
    let fixture = Fixture::new();
    fixture.run(false, DiskFaults::default(), |system| {
               let files = system.files();
               let item = files.lookup(1, ROOT_ID, "Source/Sample").unwrap();
               let first = with_owner(11, || files.open(1, item.id, Fork::Data)).unwrap();
               let second = with_owner(22, || files.open(1, item.id, Fork::Resource)).unwrap();
               assert_eq!(with_owner(33, || files.move_to_trash(1, item.id, WHEN)),
                          Err(mac_toolbox::Error(mac_toolbox::SERVICE_BUSY)));
               assert_eq!(close_owner(11), 1);
               assert_eq!(with_owner(11, || first.position()),
                          Err(mac_toolbox::Error(mac_toolbox::NOT_FOUND)));
               assert_eq!(with_owner(33, || files.move_to_trash(1, item.id, WHEN)),
                          Err(mac_toolbox::Error(mac_toolbox::SERVICE_BUSY)));
               assert_eq!(with_owner(22, || second.position()), Ok(0));
               assert_eq!(close_owner(22), 1);
               let moved = with_owner(33, || files.move_to_trash(1, item.id, WHEN)).unwrap();
               assert_eq!(moved.id, item.id);
               assert_ne!(moved.parent, item.parent);
               assert_eq!(with_owner(33, || files.put_away(1, item.id, WHEN)).unwrap().parent,
                          item.parent);
           });
    fixture.verify_hfs(":Source:");
}
#[test]
fn partial_write_and_flush_failures_restore_original_sectors()
{
    for fault in [DiskFaults { write_at: Some(3),
                               partial_write: true,
                               ..Default::default() },
                  DiskFaults { flush_at: Some(1),
                               ..Default::default() }]
    {
        let fixture = Fixture::new();
        let before = fs::read(&fixture.disk).unwrap();
        fixture.run(false, fault, |system| {
                   assert_eq!(system.files().new_folder(1, ROOT_ID, "Will roll back", WHEN),
                              Err(mac_toolbox::Error(mac_toolbox::IO_ERROR)));
                   assert!(system.files().lookup(1, ROOT_ID, "Will roll back").is_err());
                   assert!(system.files().lookup(1, ROOT_ID, "Source/Sample").is_ok());
               });
        assert_eq!(fs::read(&fixture.disk).unwrap(), before);
        fixture.verify_hfs(":Source:");
    }
}
#[test]
fn failed_rollback_disables_further_mutations()
{
    let fixture = Fixture::new();
    fixture.run(false,
                DiskFaults { write_at: Some(3),
                             partial_write: true,
                             persistent: true,
                             ..Default::default() },
                |system| {
                    assert_eq!(system.files().new_folder(1, ROOT_ID, "Broken", WHEN),
                               Err(mac_toolbox::Error(mac_toolbox::ROLLBACK_FAILED)));
                    assert_eq!(system.files().new_folder(1, ROOT_ID, "Denied", WHEN),
                               Err(mac_toolbox::Error(mac_toolbox::ROLLBACK_FAILED)));
                    assert_eq!(system.files().volume(1).unwrap().read_only, 1);
                });
}

#[test]
fn default_500mb_preallocated_catalog_and_map_nodes_are_supported()
{
    let fixture = Fixture::with_size(500 * 1024 * 1024);
    fixture.run(false, DiskFaults::default(), |system| {
               let files = system.files();
               let folder = files.new_folder(1, ROOT_ID, "Large disk folder", WHEN).unwrap();
               assert_eq!(folder.name(), "Large disk folder");
               let sample = files.lookup(1, ROOT_ID, "Source/Sample").unwrap();
               files.move_to_trash(1, sample.id, WHEN).unwrap();
               let usage = files.trash_usage(1).unwrap();
               assert_eq!(usage.items, 1);
               assert_eq!(usage.allocated_bytes, 16 * 1024);
               assert_eq!(files.empty_trash(1, WHEN).unwrap(), 1);
           });
    fixture.verify_hfs(":");
    fixture.run(true, DiskFaults::default(), |system| {
               assert!(system.files().lookup(1, ROOT_ID, "Large disk folder").is_ok());
               assert_eq!(system.files().trash_usage(1).unwrap().items, 0);
           });
}

#[test]
fn trash_and_restore_name_conflicts_preserve_both_items_and_forks()
{
    let fixture = Fixture::new();
    fixture.hfs("hmount", &[fixture.disk.to_str().unwrap()]);
    fixture.hfs("hcopy", &["-m", fixture.dir.join("sample.bin").to_str().unwrap(), ":Other:"]);
    fixture.hfs("humount", &[]);
    let (first, second, trash) =
        fixture.run(false, DiskFaults::default(), |system| {
                   let files = system.files();
                   let first = files.lookup(1, ROOT_ID, "Source/Sample").unwrap();
                   let second = files.lookup(1, ROOT_ID, "Other/Sample").unwrap();
                   files.move_to_trash(1, first.id, WHEN).unwrap();
                   (first.id, second.id, files.trash_directory(1, false).unwrap().id)
               })
               .0;
    let before = fs::read(&fixture.disk).unwrap();
    let (_, activity) = fixture.run(false, DiskFaults::default(), |system| {
                                   let files = system.files();
                                   assert_eq!(files.move_to_trash(1, second, WHEN),
                                              Err(mac_toolbox::Error(mac_toolbox::ALREADY_EXISTS)));
                                   assert_eq!(files.catalog(1, first).unwrap().parent, trash);
                                   assert_eq!(files.catalog(1, second).unwrap().name(), "Sample");
                                   assert_eq!(fork_bytes(system, first, Fork::Data),
                                              fork_bytes(system, second, Fork::Data));
                                   assert_eq!(fork_bytes(system, first, Fork::Resource),
                                              fork_bytes(system, second, Fork::Resource));
                               });
    assert_eq!(activity.writes, 0);
    assert_eq!(fs::read(&fixture.disk).unwrap(), before);
    // Create a conflicting replacement in the saved parent using independent HFS tooling.
    fixture.hfs("hmount", &[fixture.disk.to_str().unwrap()]);
    fixture.hfs("hcopy", &["-m", fixture.dir.join("sample.bin").to_str().unwrap(), ":Source:"]);
    fixture.hfs("humount", &[]);
    let before = fs::read(&fixture.disk).unwrap();
    fixture.run(false, DiskFaults::default(), |system| {
               assert_eq!(system.files().put_away(1, first, WHEN),
                          Err(mac_toolbox::Error(mac_toolbox::ALREADY_EXISTS)));
               assert_eq!(system.files().catalog(1, first).unwrap().parent, trash);
           });
    assert_eq!(fs::read(&fixture.disk).unwrap(), before);
    fixture.verify_hfs(":Trash:");
}

#[test]
fn blessed_system_locked_items_and_trash_itself_are_protected()
{
    let fixture = Fixture::new();
    fixture.hfs("hmount", &[fixture.disk.to_str().unwrap()]);
    fixture.hfs("hmkdir", &[":System Folder"]);
    fixture.hfs("hcopy",
                &["-m", fixture.dir.join("sample.bin").to_str().unwrap(), ":System Folder:System"]);
    fixture.hfs("hcopy",
                &["-m", fixture.dir.join("sample.bin").to_str().unwrap(), ":System Folder:Finder"]);
    fixture.hfs("hattrib", &["-b", ":System Folder"]);
    fixture.hfs("hattrib", &["+l", ":Source:Sample"]);
    fixture.hfs("humount", &[]);
    let before = fs::read(&fixture.disk).unwrap();
    let (_, activity) = fixture.run(false, DiskFaults::default(), |system| {
                                   let files = system.files();
                                   for path in ["System Folder", "System Folder/System",
                                                "System Folder/Finder", "Source/Sample"]
                                   {
                                       let item = files.lookup(1, ROOT_ID, path).unwrap();
                                       assert_eq!(files.move_to_trash(1, item.id, WHEN),
                                           Err(mac_toolbox::Error(mac_toolbox::ACCESS_DENIED)),
                                           "{path}");
                                   }
                               });
    assert_eq!(activity.writes, 0);
    assert_eq!(fs::read(&fixture.disk).unwrap(), before);
    fixture.run(false, DiskFaults::default(), |system| {
               let trash = system.files().trash_directory_at(1, true, WHEN).unwrap();
               assert_eq!(system.files().move_to_trash(1, trash.id, WHEN),
                          Err(mac_toolbox::Error(mac_toolbox::ACCESS_DENIED)));
           });
    fixture.verify_hfs(":");
}

#[test]
fn shared_three_boot_public_sdk_case_passes_with_real_disk_remounts()
{
    let fixture = Fixture::new();
    fixture.hfs("hmount", &[fixture.disk.to_str().unwrap()]);
    fixture.hfs("hcopy",
                &["-m", fixture.dir.join("sample.bin").to_str().unwrap(), ":Write Sample"]);
    fixture.hfs("humount", &[]);
    for expected in 1..=3
    {
        fixture.run(false, DiskFaults::default(), |system| {
                   assert_eq!(file_write_cases::stage(system).unwrap_or_else(|e| panic!("{e}")),
                              expected);
               });
        fixture.verify_hfs(":");
    }
}

#[test]
fn empty_trash_busy_locked_and_bitmap_flush_failures_preserve_payloads()
{
    let fixture = Fixture::new();
    let id = fixture.run(false, DiskFaults::default(), |system| {
                        let files = system.files();
                        let item = files.lookup(1, ROOT_ID, "Source/Sample").unwrap();
                        files.move_to_trash(1, item.id, WHEN).unwrap();
                        item.id
                    })
                    .0;
    let before = fs::read(&fixture.disk).unwrap();
    let (_, activity) = fixture.run(false, DiskFaults::default(), |system| {
                                   let file = system.files().open(1, id, Fork::Resource).unwrap();
                                   assert_eq!(system.files().empty_trash(1, WHEN),
                                              Err(mac_toolbox::Error(mac_toolbox::SERVICE_BUSY)));
                                   drop(file);
                               });
    assert_eq!(activity.writes, 0);
    assert_eq!(fs::read(&fixture.disk).unwrap(), before);
    fixture.run(false,
                DiskFaults { flush_at: Some(3),
                             ..Default::default() },
                |system| {
                    assert_eq!(system.files().empty_trash(1, WHEN),
                               Err(mac_toolbox::Error(mac_toolbox::IO_ERROR)));
                    assert_eq!(system.files().catalog(1, id).unwrap().name(), "Sample");
                    assert_eq!(fork_bytes(system, id, Fork::Data).len(), 1700);
                    assert_eq!(fork_bytes(system, id, Fork::Resource).len(), 900);
                });
    assert_eq!(fs::read(&fixture.disk).unwrap(), before);
    fixture.verify_hfs(":Trash:");
    fixture.hfs("hmount", &[fixture.disk.to_str().unwrap()]);
    fixture.hfs("hattrib", &["+l", ":Trash:Sample"]);
    fixture.hfs("humount", &[]);
    let locked = fs::read(&fixture.disk).unwrap();
    let (_, activity) = fixture.run(false, DiskFaults::default(), |system| {
                                   assert_eq!(system.files().empty_trash(1, WHEN),
                                              Err(mac_toolbox::Error(mac_toolbox::ACCESS_DENIED)));
                               });
    assert_eq!(activity.writes, 0);
    assert_eq!(fs::read(&fixture.disk).unwrap(), locked);
}

#[test]
fn a_file_named_trash_never_masquerades_as_a_trash_directory()
{
    let fixture = Fixture::new();
    fixture.hfs("hmount", &[fixture.disk.to_str().unwrap()]);
    fixture.hfs("hcopy", &["-m", fixture.dir.join("sample.bin").to_str().unwrap(), ":Trash"]);
    fixture.hfs("humount", &[]);
    let before = fs::read(&fixture.disk).unwrap();
    let (_, activity) = fixture.run(false, DiskFaults::default(), |system| {
                                   let files = system.files();
                                   assert_eq!(files.trash_directory(1, false),
                                              Err(mac_toolbox::Error(mac_toolbox::ALREADY_EXISTS)));
                                   assert_eq!(files.trash_usage(1),
                                              Err(mac_toolbox::Error(mac_toolbox::ALREADY_EXISTS)));
                                   assert_eq!(files.empty_trash(1, WHEN),
                                              Err(mac_toolbox::Error(mac_toolbox::ALREADY_EXISTS)));
                               });
    assert_eq!(activity.writes, 0);
    assert_eq!(fs::read(&fixture.disk).unwrap(), before);
}

#[test]
fn on_disk_volume_locks_and_dirty_volume_state_are_respected()
{
    use std::io::{Read, Seek, SeekFrom, Write};
    for (set, clear, expected, readonly) in
        [(0x8000u16, 0u16, mac_toolbox::READ_ONLY, 1), (0, 0x0100, mac_toolbox::NOT_SUPPORTED, 0)]
    {
        let fixture = Fixture::new();
        let mut file = fs::OpenOptions::new().read(true).write(true).open(&fixture.disk).unwrap();
        for sector in [2, file.metadata().unwrap().len() / 512 - 2]
        {
            file.seek(SeekFrom::Start(sector * 512 + 10)).unwrap();
            let mut bytes = [0; 2];
            file.read_exact(&mut bytes).unwrap();
            let attributes = (u16::from_be_bytes(bytes) | set) & !clear;
            file.seek(SeekFrom::Start(sector * 512 + 10)).unwrap();
            file.write_all(&attributes.to_be_bytes()).unwrap();
        }
        file.sync_all().unwrap();
        drop(file);
        let before = fs::read(&fixture.disk).unwrap();
        let (_, activity) =
            fixture.run(false, DiskFaults::default(), |system| {
                       assert_eq!(system.files().volume(1).unwrap().read_only, readonly);
                       assert!(system.files().lookup(1, ROOT_ID, "Source/Sample").is_ok());
                       assert_eq!(system.files().new_folder(1, ROOT_ID, "Rejected", WHEN),
                                  Err(mac_toolbox::Error(expected)));
                   });
        assert_eq!(activity.writes, 0);
        assert_eq!(fs::read(&fixture.disk).unwrap(), before);
    }
}

#[test]
fn legacy_pascal_new_folder_uses_the_same_durable_native_file_manager()
{
    use mac_abi::{files::{FileCall, NEW_FOLDER},
                  traps::PSTRING_FILE};
    let fixture = Fixture::new();
    with_file_disk(&fixture.disk, false, DiskFaults::default(), |services| {
        let name = b"\x04Caf\x8e";
        let mut call = FileCall { operation: NEW_FOLDER,
                                  volume: 1,
                                  cnid: ROOT_ID,
                                  name: name.as_ptr(),
                                  name_length: name.len(),
                                  position: WHEN.mac_seconds().unwrap() as u64,
                                  ..Default::default() };
        let status = unsafe {
            (services.trap)(PSTRING_FILE,
                            core::ptr::from_mut(&mut call).cast(),
                            size_of::<FileCall>())
        };
        assert_eq!(status, 0);
        assert_eq!(&call.entry.name[..call.entry.name_length as usize], name);
        let system = unsafe { System::from_raw(services) }.unwrap();
        assert_eq!(system.files().lookup(1, ROOT_ID, "Café").unwrap().id, call.entry.id);
    }).unwrap();
    fixture.run(true, DiskFaults::default(), |system| {
               assert_eq!(system.files().lookup(1, ROOT_ID, "Café").unwrap().created_at(), WHEN);
           });
    fixture.verify_hfs(":");
}

#[test]
fn get_info_metadata_edits_preserve_identity_forks_and_survive_remount()
{
    let fixture = Fixture::new();
    let ((original, allocation, data, resources, source), _) =
        fixture.run(false, DiskFaults::default(), |system| {
                   let files = system.files();
                   let original = files.lookup(1, ROOT_ID, "Source/Sample").unwrap();
                   let source = files.catalog(1, original.parent).unwrap();
                   let allocation = files.allocation_info(1, original.id).unwrap();
                   assert_eq!((allocation.data_bytes, allocation.resource_bytes), (2048, 1024));
                   assert_eq!(files.allocation_info(1, source.id).unwrap(), Default::default());
                   let data = fork_bytes(system, original.id, Fork::Data);
                   let resources = fork_bytes(system, original.id, Fork::Resource);
                   // Case-only changes keep their own identity, rather than colliding.
                   assert_eq!(files.rename(1, original.id, "sample", WHEN).unwrap().id,
                              original.id);
                   let renamed = files.rename(1, original.id, "Café", WHEN).unwrap();
                   assert_eq!(renamed.name(), "Café");
                   assert_eq!((renamed.id, renamed.parent), (original.id, original.parent));
                   assert_eq!((renamed.data_length, renamed.resource_length),
                              (original.data_length, original.resource_length));
                   assert_eq!((renamed.created, renamed.modified, renamed.backup),
                              (original.created, original.modified, original.backup));
                   assert_eq!((renamed.file_type, renamed.creator, renamed.finder_info),
                              (original.file_type, original.creator, original.finder_info));
                   files.rename(1, source.id, "Renamed Source", WHEN).unwrap();
                   assert_eq!(files.lookup(1, ROOT_ID, "Renamed Source/Café").unwrap().id,
                              original.id);
                   assert!(files.lookup(1, ROOT_ID, "Source").is_err());
                   assert_ne!(files.set_stationery(1, original.id, true, WHEN)
                                   .unwrap()
                                   .finder_flags()
                              & 0x0800,
                              0);
                   assert_ne!(files.set_locked(1, original.id, true, WHEN).unwrap().flags & 1, 0);
                   assert_eq!(files.rename(1, original.id, "Rejected", WHEN),
                              Err(mac_toolbox::Error(mac_toolbox::ACCESS_DENIED)));
                   assert_eq!(files.set_stationery(1, original.id, false, WHEN),
                              Err(mac_toolbox::Error(mac_toolbox::ACCESS_DENIED)));
                   (original, allocation, data, resources, source.id)
               });
    fixture.verify_hfs(":Renamed Source:");
    fixture.run(false, DiskFaults::default(), |system| {
               let files = system.files();
               let renamed = files.lookup(1, ROOT_ID, "Renamed Source/Café").unwrap();
               assert_eq!((renamed.id, renamed.parent), (original.id, source));
               assert_eq!(renamed.flags & !1, original.flags & !1);
               assert_eq!(renamed.flags & 1, 1);
               let mut expected_finder = original.finder_info;
               let flags = u16::from_be_bytes([expected_finder[8], expected_finder[9]]) | 0x0800;
               expected_finder[8..10].copy_from_slice(&flags.to_be_bytes());
               assert_eq!(renamed.finder_info, expected_finder);
               assert_eq!(files.allocation_info(1, original.id).unwrap(), allocation);
               assert_eq!(fork_bytes(system, original.id, Fork::Data), data);
               assert_eq!(fork_bytes(system, original.id, Fork::Resource), resources);
               files.set_locked(1, original.id, false, WHEN).unwrap();
               assert_eq!(files.set_stationery(1, original.id, false, WHEN).unwrap().finder_info,
                          original.finder_info);
               // The limit counts MacRoman bytes rather than Rust UTF8 bytes.
               let long = "é".repeat(31);
               assert_eq!(files.rename(1, original.id, &long, WHEN).unwrap().name(), long);
               files.rename(1, original.id, "Sample", WHEN).unwrap();
               assert_eq!(files.catalog(1, original.id).unwrap().finder_info, original.finder_info);
           });
    fixture.verify_hfs(":Renamed Source:");
    let before = fs::read(&fixture.disk).unwrap();
    let (_, activity) = fixture.run(false, DiskFaults::default(), |system| {
                                   let files = system.files();
                                   files.rename(1, original.id, "Sample", WHEN).unwrap();
                                   files.set_locked(1, original.id, false, WHEN).unwrap();
                                   files.set_stationery(1, original.id, false, WHEN).unwrap();
                               });
    assert_eq!(activity.writes, 0, "unchanged metadata must not write the disk");
    assert_eq!(fs::read(&fixture.disk).unwrap(), before);
}

#[test]
fn get_info_rejected_metadata_edits_never_change_the_disk()
{
    let fixture = Fixture::new();
    let before = fs::read(&fixture.disk).unwrap();
    for readonly in [true, false]
    {
        let (_, activity) =
            fixture.run(readonly, DiskFaults::default(), |system| {
                       let files = system.files();
                       let sample = files.lookup(1, ROOT_ID, "Source/Sample").unwrap();
                       if readonly
                       {
                           for result in [files.rename(1, sample.id, "New", WHEN),
                                          files.set_locked(1, sample.id, true, WHEN),
                                          files.set_stationery(1, sample.id, true, WHEN)]
                           {
                               assert_eq!(result, Err(mac_toolbox::Error(mac_toolbox::READ_ONLY)));
                           }
                       }
                       else
                       {
                           for name in
                               ["", ".", "..", "bad:name", "bad\0name", "😀", &"é".repeat(32)]
                           {
                               assert_eq!(files.rename(1, sample.id, name, WHEN),
                                          Err(mac_toolbox::Error(mac_toolbox::BAD_ARGUMENT)),
                                          "{name:?}");
                           }
                           assert_eq!(files.rename(1, sample.parent, "Other", WHEN),
                                      Err(mac_toolbox::Error(mac_toolbox::ALREADY_EXISTS)));
                           assert_eq!(files.rename(1, ROOT_ID, "Renamed disk", WHEN),
                                      Err(mac_toolbox::Error(mac_toolbox::ACCESS_DENIED)));
                           assert_eq!(files.set_locked(1, sample.parent, true, WHEN),
                                      Err(mac_toolbox::Error(mac_toolbox::BAD_ARGUMENT)));
                           assert_eq!(files.set_stationery(1, sample.parent, true, WHEN),
                                      Err(mac_toolbox::Error(mac_toolbox::BAD_ARGUMENT)));
                           let open = files.open(1, sample.id, Fork::Resource).unwrap();
                           for result in [files.rename(1, sample.id, "New", WHEN),
                                          files.rename(1, sample.parent, "New folder", WHEN),
                                          files.set_locked(1, sample.id, true, WHEN),
                                          files.set_stationery(1, sample.id, true, WHEN)]
                           {
                               assert_eq!(result,
                                          Err(mac_toolbox::Error(mac_toolbox::SERVICE_BUSY)));
                           }
                           drop(open);
                       }
                   });
        assert_eq!(activity.writes, 0);
        assert_eq!(fs::read(&fixture.disk).unwrap(), before);
    }
}

#[test]
fn get_info_mutations_rollback_partial_writes_and_poison_failed_rollback()
{
    for operation in 0..3
    {
        for persistent in [false, true]
        {
            let fixture = Fixture::new();
            let before = fs::read(&fixture.disk).unwrap();
            fixture.run(false,
                        DiskFaults { write_at: Some(3),
                                     partial_write: true,
                                     persistent,
                                     ..Default::default() },
                        |system| {
                            let files = system.files();
                            let sample = files.lookup(1, ROOT_ID, "Source/Sample").unwrap();
                            let result = match operation
                            {
                                0 => files.rename(1, sample.id, "Renamed", WHEN),
                                1 => files.set_locked(1, sample.id, true, WHEN),
                                _ => files.set_stationery(1, sample.id, true, WHEN)
                            };
                            let expected = if persistent
                            {
                                mac_toolbox::ROLLBACK_FAILED
                            }
                            else
                            {
                                mac_toolbox::IO_ERROR
                            };
                            assert_eq!(result, Err(mac_toolbox::Error(expected)));
                            if persistent
                            {
                                assert_eq!(files.volume(1).unwrap().read_only, 1);
                                assert_eq!(files.rename(1, sample.id, "Again", WHEN),
                                           Err(mac_toolbox::Error(expected)));
                            }
                            else
                            {
                                assert_eq!(files.catalog(1, sample.id).unwrap(), sample);
                            }
                        });
            if !persistent
            {
                assert_eq!(fs::read(&fixture.disk).unwrap(), before);
                fixture.verify_hfs(":Source:");
            }
        }
    }
}

#[test]
fn additive_metadata_services_keep_the_64_bit_abi6_file_layout()
{
    use core::mem::{offset_of, size_of};

    use mac_abi::files::{CatalogInfo, FileCall, VolumeInfo};
    assert_eq!(size_of::<CatalogInfo>(), 224);
    assert_eq!(size_of::<VolumeInfo>(), 192);
    assert_eq!(size_of::<FileCall>(), 512);
    assert_eq!(offset_of!(FileCall, entry), 288);
    assert_eq!(offset_of!(CatalogInfo, finder_info), 56);
    assert_eq!(offset_of!(CatalogInfo, name), 92);
}

#[test]
fn finder_name_lock_is_separate_from_catalog_lock_and_unrelated_flags_survive()
{
    let fixture = Fixture::new();
    let mut sample =
        mac_formats::macbinary::decode(&fs::read(fixture.dir.join("sample.bin")).unwrap()).unwrap();
    sample.name = b"Name locked".to_vec();
    sample.flags = 0x100e;
    let binary = fixture.dir.join("name-locked.bin");
    fs::write(&binary, mac_formats::macbinary::encode(&sample).unwrap()).unwrap();
    fixture.hfs("hmount", &[fixture.disk.to_str().unwrap()]);
    fixture.hfs("hcopy", &["-m", binary.to_str().unwrap(), ":Source:"]);
    fixture.hfs("humount", &[]);
    fixture.run(false, DiskFaults::default(), |system| {
               let files = system.files();
               let original = files.lookup(1, ROOT_ID, "Source/Name locked").unwrap();
               assert!(original.is_name_locked());
               assert!(!original.is_locked());
               assert!(!original.is_stationery());
               assert_eq!(files.rename(1, original.id, "New name", WHEN),
                          Err(mac_toolbox::Error(mac_toolbox::ACCESS_DENIED)));
               let stationery = files.set_stationery(1, original.id, true, WHEN).unwrap();
               assert!(stationery.is_stationery());
               assert_eq!(stationery.finder_flags(), 0x180e);
               let locked = files.set_locked(1, original.id, true, WHEN).unwrap();
               assert!(locked.is_locked());
               assert_eq!(locked.finder_info, stationery.finder_info);
               let unlocked = files.set_locked(1, original.id, false, WHEN).unwrap();
               assert_eq!(unlocked.flags, original.flags);
               let cleared = files.set_stationery(1, original.id, false, WHEN).unwrap();
               assert_eq!(cleared.finder_info, original.finder_info);
           });
    fixture.verify_hfs(":Source:");
}

fn comment_text(system: &System<'_>, id: u32) -> String
{
    let mut output = String::new();
    system.files().comment(1, id, &mut output).unwrap();
    output
}
fn validate_desktop_indexes(bytes: &[u8])
{
    fn descend(bytes: &[u8], node: usize, height: u8, seen: &mut std::collections::HashSet<usize>,
               leaves: &mut Vec<usize>)
               -> Vec<u8>
    {
        let total = u32::from_be_bytes(bytes[36..40].try_into().unwrap()) as usize;
        assert!(node != 0 && node < total && seen.insert(node));
        assert!((1..=8).contains(&height));
        let base = node * 512;
        assert_eq!(bytes[base + 9], height);
        let count = u16::from_be_bytes(bytes[base + 10..base + 12].try_into().unwrap()) as usize;
        assert!(count != 0);
        let mut first = None;
        for index in 0..count
        {
            let offset = |at| {
                u16::from_be_bytes(bytes[base + at..base + at + 2].try_into().unwrap()) as usize
            };
            let start = base + offset(510 - index * 2);
            let end = base + offset(508 - index * 2);
            let length = usize::from(bytes[start]);
            assert!(length != 0 && length <= 37 && start + length < end);
            let key = if height == 1
            {
                assert_eq!(bytes[base + 8], 255);
                bytes[start..start + length + 1].to_vec()
            }
            else
            {
                assert_eq!(bytes[base + 8], 0);
                let span = (length + 2) & !1;
                assert_eq!(start + span + 4, end);
                let child =
                    u32::from_be_bytes(bytes[start + span..end].try_into().unwrap()) as usize;
                let key = descend(bytes, child, height - 1, seen, leaves);
                assert_eq!(&bytes[start + 1..start + key.len()],
                           &key[1..],
                           "Desktop DB index does not identify its child's first key");
                key
            };
            if first.is_none()
            {
                first = Some(key);
            }
        }
        if height == 1
        {
            leaves.push(node);
        }
        first.unwrap()
    }
    let root = u32::from_be_bytes(bytes[16..20].try_into().unwrap()) as usize;
    if root == 0
    {
        return;
    }
    let depth = u16::from_be_bytes(bytes[14..16].try_into().unwrap()) as u8;
    let mut indexed = Vec::new();
    descend(bytes, root, depth, &mut Default::default(), &mut indexed);
    let mut linked = Vec::new();
    let mut node = u32::from_be_bytes(bytes[24..28].try_into().unwrap()) as usize;
    while node != 0
    {
        assert!(linked.len() < indexed.len());
        linked.push(node);
        node = u32::from_be_bytes(bytes[node * 512..node * 512 + 4].try_into().unwrap()) as usize;
    }
    assert_eq!(indexed, linked, "index traversal and linked leaf order disagree");
}
fn desktop_record_spans(bytes: &[u8]) -> Vec<(usize, usize)>
{
    validate_desktop_indexes(bytes);
    let word = |at| u16::from_be_bytes(bytes[at..at + 2].try_into().unwrap()) as usize;
    let long = |at| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    assert_eq!(word(32), 512);
    let mut node = long(24);
    let mut previous = 0;
    let mut visited = std::collections::HashSet::new();
    let mut records = Vec::new();
    while node != 0
    {
        assert!(visited.insert(node) && node < long(36), "invalid Desktop DB leaf chain");
        let base = node * 512;
        assert_eq!(bytes[base + 8], 255);
        assert_eq!(long(base + 4), previous);
        for index in 0..word(base + 10)
        {
            let start = word(base + 510 - index * 2);
            let end = word(base + 508 - index * 2);
            assert!(14 <= start && start < end && end <= 512);
            records.push((base + start, base + end));
        }
        previous = node;
        node = long(base);
    }
    assert_eq!(previous, long(28));
    assert_eq!(records.len(), long(20));
    records
}
fn desktop_unknown_records(bytes: &[u8]) -> Vec<Vec<u8>>
{
    desktop_record_spans(bytes).into_iter()
                               .map(|(start, end)| &bytes[start..end])
                               .filter(|record| !(record[0] == 5 && record[1] == 3))
                               .map(|record| record.to_vec())
                               .collect()
}
#[test]
fn comments_create_real_desktop_db_and_remount_update_delete_without_touching_target_dates()
{
    let fixture = Fixture::new();
    let (original, _) =
        fixture.run(false, DiskFaults::default(), |system| {
                   let files = system.files();
                   let sample = files.lookup(1, ROOT_ID, "Source/Sample").unwrap();
                   assert_eq!(comment_text(system, sample.id), "");
                   files.set_comment(1, sample.id, "Résumé\rSecond line", WHEN).unwrap();
                   files.set_comment(1, sample.parent, "Folder comment", WHEN).unwrap();
                   let db = files.lookup(1, ROOT_ID, "Desktop DB").unwrap();
                   assert_eq!((db.file_type, db.creator), (*b"BTFL", *b"DMGR"));
                   assert_eq!(db.finder_flags() & 0x4000, 0x4000);
                   assert_eq!(db.data_length, 64 * 1024);
                   assert_eq!(db.resource_length, 0);
                   assert_eq!(files.catalog(1, sample.id).unwrap(), sample);
                   assert_eq!(comment_text(system, sample.id), "Résumé\rSecond line");
                   sample
               });
    fixture.verify_hfs(":");
    fixture.run(false, DiskFaults::default(), |system| {
               let files = system.files();
               assert_eq!(comment_text(system, original.id), "Résumé\rSecond line");
               assert_eq!(comment_text(system, original.parent), "Folder comment");
               let longest = "é".repeat(255);
               files.set_comment(1, original.id, &longest, WHEN).unwrap();
               assert_eq!(comment_text(system, original.id), longest);
               files.set_comment(1, original.id, "", WHEN).unwrap();
               assert_eq!(comment_text(system, original.id), "");
               assert_eq!(comment_text(system, original.parent), "Folder comment");
               assert_eq!(files.catalog(1, original.id).unwrap(), original);
               assert_eq!(fork_bytes(system, original.id, Fork::Data),
                          (0..1700).map(|n| (n * 17) as u8).collect::<Vec<_>>());
           });
    fixture.run(true, DiskFaults::default(), |system| {
               assert_eq!(comment_text(system, original.id), "");
               assert_eq!(comment_text(system, original.parent), "Folder comment");
               assert_eq!(system.files().catalog(1, original.id).unwrap(), original);
           });
}
#[test]
fn captured_original_desktop_comments_preserve_every_unknown_record_on_native_edits()
{
    let fixture = Fixture::new();
    let ((folder, sample, other), _) =
        fixture.run(true, DiskFaults::default(), |system| {
                   let files = system.files();
                   (files.lookup(1, ROOT_ID, "Source").unwrap().id,
                    files.lookup(1, ROOT_ID, "Source/Sample").unwrap().id,
                    files.lookup(1, ROOT_ID, "Other").unwrap().id)
               });
    let original = include_bytes!("data/desktop-comments/original-desktop-db.bin");
    let mut captured = original.to_vec();
    let spans = desktop_record_spans(original);
    assert_eq!(spans.len(), 2318);
    // Fixture transport only: remap the two observed type3 keys to real catalog
    // identities in this disposable disk. Every unrelated key/value is unchanged.
    let mut remapped = 0;
    for (start, _) in spans
    {
        if captured[start] == 5 && captured[start + 1] == 3
        {
            let old = u32::from_be_bytes(captured[start + 2..start + 6].try_into().unwrap());
            let new = match old
            {
                460 => folder,
                463 => sample,
                _ => panic!("unexpected original comment CNID")
            };
            captured[start + 2..start + 6].copy_from_slice(&new.to_be_bytes());
            remapped += 1;
        }
    }
    assert_eq!(remapped, 2);
    let unknown = desktop_unknown_records(original);
    assert_eq!(unknown.len(), 2316);
    let db = mac_formats::macbinary::MacFile { name:      b"Desktop DB".to_vec(),
                                               file_type: *b"BTFL",
                                               creator:   *b"DMGR",
                                               flags:     0x4000,
                                               created:   WHEN.mac_seconds().unwrap(),
                                               modified:  WHEN.mac_seconds().unwrap(),
                                               data:      captured,
                                               resources: Vec::new() };
    let binary = fixture.dir.join("captured-desktop.bin");
    fs::write(&binary, mac_formats::macbinary::encode(&db).unwrap()).unwrap();
    fixture.hfs("hmount", &[fixture.disk.to_str().unwrap()]);
    fixture.hfs("hcopy", &["-m", binary.to_str().unwrap(), ":"]);
    fixture.hfs("humount", &[]);
    fixture.run(false, DiskFaults::default(), |system| {
               let files = system.files();
               assert_eq!(comment_text(system, folder), "Folder comment\rSecond line");
               assert_eq!(comment_text(system, sample), "Renamed File");
               files.set_comment(1, sample, "Native updated comment", WHEN).unwrap();
               files.set_comment(1, other, "Inserted between existing comments", WHEN).unwrap();
               files.set_comment(1, folder, "", WHEN).unwrap();
               let db = files.lookup(1, ROOT_ID, "Desktop DB").unwrap();
               let bytes = fork_bytes(system, db.id, Fork::Data);
               assert_eq!(desktop_unknown_records(&bytes), unknown);
               assert_eq!(&bytes[120..248],
                          &original[120..248],
                          "Desktop header user data changed");
           });
    fixture.run(true, DiskFaults::default(), |system| {
               assert_eq!(comment_text(system, sample), "Native updated comment");
               assert_eq!(comment_text(system, other), "Inserted between existing comments");
               assert_eq!(comment_text(system, folder), "");
           });
    fixture.verify_hfs(":");
}
#[test]
fn comments_reject_locked_readonly_overlong_and_unrepresentable_updates()
{
    let fixture = Fixture::new();
    let (id, _) = fixture.run(false, DiskFaults::default(), |system| {
                             let files = system.files();
                             let sample = files.lookup(1, ROOT_ID, "Source/Sample").unwrap();
                             files.set_comment(1, sample.id, "Original comment", WHEN).unwrap();
                             files.set_locked(1, sample.id, true, WHEN).unwrap();
                             sample.id
                         });
    let before = fs::read(&fixture.disk).unwrap();
    fixture.run(false, DiskFaults::default(), |system| {
               assert_eq!(system.files().set_comment(1, id, "Changed", WHEN),
                          Err(mac_toolbox::Error(mac_toolbox::ACCESS_DENIED)));
               assert_eq!(comment_text(system, id), "Original comment");
           });
    fixture.run(true, DiskFaults::default(), |system| {
               assert_eq!(system.files().set_comment(1, id, "Changed", WHEN),
                          Err(mac_toolbox::Error(mac_toolbox::READ_ONLY)));
               assert_eq!(comment_text(system, id), "Original comment");
           });
    assert_eq!(fs::read(&fixture.disk).unwrap(), before);
    fixture.run(false, DiskFaults::default(), |system| {
               system.files().set_locked(1, id, false, WHEN).unwrap();
           });
    let before = fs::read(&fixture.disk).unwrap();
    let (_, activity) =
        fixture.run(false, DiskFaults::default(), |system| {
                   for text in [&"é".repeat(256), "😀"]
                   {
                       assert_eq!(system.files().set_comment(1, id, text, WHEN),
                                  Err(mac_toolbox::Error(mac_toolbox::BAD_ARGUMENT)));
                   }
                   system.files().set_comment(1, id, "Original comment", WHEN).unwrap();
               });
    assert_eq!(activity.writes, 0);
    assert_eq!(fs::read(&fixture.disk).unwrap(), before);
}
#[test]
fn desktop_db_creation_and_update_rollback_the_complete_disk_after_reported_failures()
{
    for existing in [false, true]
    {
        for persistent in [false, true]
        {
            let fixture = Fixture::new();
            let (id, _) =
                fixture.run(false, DiskFaults::default(), |system| {
                           let sample = system.files().lookup(1, ROOT_ID, "Source/Sample").unwrap();
                           if existing
                           {
                               system.files().set_comment(1, sample.id, "Existing", WHEN).unwrap();
                           }
                           sample.id
                       });
            let before = fs::read(&fixture.disk).unwrap();
            fixture.run(false,
                        DiskFaults { write_at: Some(3),
                                     partial_write: true,
                                     persistent,
                                     ..Default::default() },
                        |system| {
                            let expected = if persistent
                            {
                                mac_toolbox::ROLLBACK_FAILED
                            }
                            else
                            {
                                mac_toolbox::IO_ERROR
                            };
                            assert_eq!(system.files().set_comment(1, id, "A new comment", WHEN),
                                       Err(mac_toolbox::Error(expected)));
                            if persistent
                            {
                                assert_eq!(system.files().volume(1).unwrap().read_only, 1);
                            }
                            else
                            {
                                assert_eq!(comment_text(system, id),
                                           if existing { "Existing" } else { "" });
                            }
                        });
            if !persistent
            {
                assert_eq!(fs::read(&fixture.disk).unwrap(), before);
                fixture.verify_hfs(":Source:");
            }
        }
    }
}

#[test]
fn a_full_desktop_db_reports_capacity_before_any_sector_changes()
{
    let fixture = Fixture::new();
    let (target, _) = fixture.run(true, DiskFaults::default(), |system| {
                                 system.files().lookup(1, ROOT_ID, "Source/Sample").unwrap().id
                             });
    let captured = include_bytes!("data/desktop-comments/original-desktop-db.bin");
    let mut bytes = vec![0; 1024];
    bytes[..512].copy_from_slice(&captured[..512]);
    let put16 = |bytes: &mut [u8], at: usize, value: u16| {
        bytes[at..at + 2].copy_from_slice(&value.to_be_bytes())
    };
    let put32 = |bytes: &mut [u8], at: usize, value: u32| {
        bytes[at..at + 4].copy_from_slice(&value.to_be_bytes())
    };
    put16(&mut bytes, 14, 1);
    for at in [16, 20, 24, 28]
    {
        put32(&mut bytes, at, 1);
    }
    put32(&mut bytes, 36, 2);
    put32(&mut bytes, 40, 0);
    bytes[248..504].fill(0);
    bytes[248] = 0xc0;
    bytes[520] = 255;
    bytes[521] = 1;
    put16(&mut bytes, 522, 1);
    let (start, end) = desktop_record_spans(captured)[0];
    let mut opaque = captured[start..end].to_vec();
    opaque.resize(400, 0xa5);
    bytes[526..926].copy_from_slice(&opaque);
    put16(&mut bytes, 1022, 14);
    put16(&mut bytes, 1020, 414);
    let db = mac_formats::macbinary::MacFile { name:      b"Desktop DB".to_vec(),
                                               file_type: *b"BTFL",
                                               creator:   *b"DMGR",
                                               flags:     0x4000,
                                               created:   WHEN.mac_seconds().unwrap(),
                                               modified:  WHEN.mac_seconds().unwrap(),
                                               data:      bytes,
                                               resources: Vec::new() };
    let binary = fixture.dir.join("full-desktop.bin");
    fs::write(&binary, mac_formats::macbinary::encode(&db).unwrap()).unwrap();
    fixture.hfs("hmount", &[fixture.disk.to_str().unwrap()]);
    fixture.hfs("hcopy", &["-m", binary.to_str().unwrap(), ":"]);
    fixture.hfs("humount", &[]);
    let before = fs::read(&fixture.disk).unwrap();
    let (_, activity) =
        fixture.run(false, DiskFaults::default(), |system| {
                   assert_eq!(system.files().set_comment(1, target, &"x".repeat(255), WHEN),
                              Err(mac_toolbox::Error(mac_toolbox::DISK_FULL)));
                   assert_eq!(comment_text(system, target), "");
               });
    assert_eq!(activity.writes, 0);
    assert_eq!(fs::read(&fixture.disk).unwrap(), before);
}
#[test]
fn desktop_db_flush_failure_restores_payload_bitmap_and_both_volume_headers()
{
    let fixture = Fixture::new();
    let before = fs::read(&fixture.disk).unwrap();
    fixture.run(false,
                DiskFaults { flush_at: Some(2),
                             ..Default::default() },
                |system| {
                    let sample = system.files().lookup(1, ROOT_ID, "Source/Sample").unwrap();
                    assert_eq!(system.files().set_comment(1, sample.id,
                                                          "Rollback after payload flush", WHEN),
                               Err(mac_toolbox::Error(mac_toolbox::IO_ERROR)));
                    assert_eq!(comment_text(system, sample.id), "");
                    assert!(system.files().lookup(1, ROOT_ID, "Desktop DB").is_err());
                });
    assert_eq!(fs::read(&fixture.disk).unwrap(), before);
    fixture.verify_hfs(":Source:");
}

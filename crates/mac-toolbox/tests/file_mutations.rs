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

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
#[path = "support/file_moves.rs"]
mod file_moves;
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
    let mut file = system.files()
                         .open(system.files().volume_at(0).unwrap().id, mac_toolbox::Cnid(id), fork)
                         .unwrap();
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
                   assert!(!files.volume(system.files().volume_at(0).unwrap().id)
                                 .unwrap()
                                 .read_only);
                   let source =
                       files.lookup(system.files().volume_at(0).unwrap().id, ROOT_ID, "Source")
                            .unwrap();
                   let sample =
                       files.lookup(system.files().volume_at(0).unwrap().id, source.id, "Sample")
                            .unwrap();
                   assert!(matches!(files.trash_directory(system.files()
                                                                .volume_at(0)
                                                                .unwrap()
                                                                .id,
                                                          false),
                                    Err(mac_toolbox::Error::NotFound)));
                   let data = fork_bytes(system, (sample.id).0, Fork::Data);
                   let resources = fork_bytes(system, (sample.id).0, Fork::Resource);
                   let free = files.volume(system.files().volume_at(0).unwrap().id)
                                   .unwrap()
                                   .capacity
                                   .unwrap()
                                   .free_bytes;
                   let folder = files.new_folder_at(system.files().volume_at(0).unwrap().id,
                                                    source.id,
                                                    "Café",
                                                    WHEN)
                                     .unwrap();
                   assert_eq!(folder.created_at(), WHEN);
                   assert_eq!(folder.modified_at(), WHEN);
                   assert_eq!(files.catalog(system.files().volume_at(0).unwrap().id, source.id)
                                   .unwrap()
                                   .child_count,
                              Some(2));
                   let moved = files.move_to_trash_at(system.files().volume_at(0).unwrap().id,
                                                      sample.id,
                                                      WHEN)
                                    .unwrap();
                   let trash = files.trash_directory(system.files().volume_at(0).unwrap().id,
                                                     false)
                                    .unwrap();
                   assert_eq!(trash.finder_flags().bits() & 0x1000, 0x1000);
                   assert_eq!(moved.id, sample.id);
                   assert_eq!(moved.parent, trash.id);
                   assert_eq!(moved.put_away_parent(), source.id);
                   assert_eq!(files.volume(system.files().volume_at(0).unwrap().id)
                                   .unwrap()
                                   .capacity
                                   .unwrap()
                                   .free_bytes,
                              free);
                   let usage = files.trash_usage(system.files().volume_at(0).unwrap().id).unwrap();
                   assert_eq!(usage.items, 1);
                   assert!(usage.allocated_bytes.is_some_and(|n| n >= 2600));
                   (source.id, sample.id, free, data, resources)
               });
    fixture.verify_hfs(":Trash:");
    fixture.run(false, DiskFaults::default(), |system| {
               let files = system.files();
               let moved = files.catalog(system.files().volume_at(0).unwrap().id, sample).unwrap();
               assert_eq!(moved.put_away_parent(), source);
               assert_eq!(fork_bytes(system, (sample).0, Fork::Data), original_data);
               assert_eq!(fork_bytes(system, (sample).0, Fork::Resource), original_resources);
               let restored =
                   files.put_away_at(system.files().volume_at(0).unwrap().id, sample, WHEN)
                        .unwrap();
               assert_eq!(restored.id, sample);
               assert_eq!(restored.parent, source);
               assert_eq!(restored.put_away_parent(), source);
               assert_eq!(files.empty_trash_at(system.files().volume_at(0).unwrap().id, WHEN)
                               .unwrap(),
                          0);
               files.move_to_trash_at(system.files().volume_at(0).unwrap().id, source, WHEN)
                    .unwrap();
               assert_eq!(files.empty_trash_at(system.files().volume_at(0).unwrap().id, WHEN)
                               .unwrap(),
                          3);
               assert!(matches!(files.catalog(system.files().volume_at(0).unwrap().id, sample),
                                Err(mac_toolbox::Error::NotFound)));
               assert!(files.volume(system.files().volume_at(0).unwrap().id)
                            .unwrap()
                            .capacity
                            .unwrap()
                            .free_bytes
                       > before);
           });
    fixture.run(true, DiskFaults::default(), |system| {
               let files = system.files();
               assert!(files.lookup(system.files().volume_at(0).unwrap().id, ROOT_ID, "Source")
                            .is_err());
               assert_eq!(files.trash_directory(system.files().volume_at(0).unwrap().id, false)
                               .unwrap()
                               .child_count,
                          Some(0));
               assert!(files.lookup(system.files().volume_at(0).unwrap().id, ROOT_ID, "Other")
                            .unwrap()
                            .is_directory());
           });
    fixture.verify_hfs(":");
}
#[test]
fn readonly_busy_conflict_and_invalid_names_never_modify_disk()
{
    let fixture = Fixture::new();
    let before = fs::read(&fixture.disk).unwrap();
    let (_, activity) = fixture.run(true, DiskFaults::default(), |system| {
                                   assert_eq!(system.files().new_folder_at(system.files()
                                                                                 .volume_at(0)
                                                                                 .unwrap()
                                                                                 .id,
                                                                           ROOT_ID,
                                                                           "No",
                                                                           WHEN),
                                              Err(mac_toolbox::Error::ReadOnly));
                               });
    assert_eq!(activity.writes, 0);
    assert_eq!(fs::read(&fixture.disk).unwrap(), before);
    fixture.run(false, DiskFaults::default(), |system| {
               let files = system.files();
               assert_eq!(files.new_folder_at(system.files().volume_at(0).unwrap().id,
                                              ROOT_ID,
                                              "Source",
                                              WHEN),
                          Err(mac_toolbox::Error::AlreadyExists));
               assert_eq!(files.new_folder_at(system.files().volume_at(0).unwrap().id,
                                              ROOT_ID,
                                              "bad:name",
                                              WHEN),
                          Err(mac_toolbox::Error::BadArgument));
               assert_eq!(files.move_to_trash_at(system.files().volume_at(0).unwrap().id,
                                                 ROOT_ID,
                                                 WHEN),
                          Err(mac_toolbox::Error::AccessDenied));
               let item =
                   files.lookup(system.files().volume_at(0).unwrap().id, ROOT_ID, "Source/Sample")
                        .unwrap();
               let open = files.open(system.files().volume_at(0).unwrap().id, item.id, Fork::Data)
                               .unwrap();
               assert_eq!(files.move_to_trash_at(system.files().volume_at(0).unwrap().id,
                                                 item.id,
                                                 WHEN),
                          Err(mac_toolbox::Error::ServiceBusy));
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
               let item =
                   files.lookup(system.files().volume_at(0).unwrap().id, ROOT_ID, "Source/Sample")
                        .unwrap();
               let first =
                   with_owner(11, || {
                       files.open(system.files().volume_at(0).unwrap().id, item.id, Fork::Data)
                   }).unwrap();
               let second =
                   with_owner(22, || {
                       files.open(system.files().volume_at(0).unwrap().id, item.id, Fork::Resource)
                   }).unwrap();
               assert_eq!(with_owner(33, || {
                              files.move_to_trash_at(system.files().volume_at(0).unwrap().id,
                                                     item.id,
                                                     WHEN)
                          }),
                          Err(mac_toolbox::Error::ServiceBusy));
               assert_eq!(close_owner(11), 1);
               assert_eq!(with_owner(11, || first.position()), Err(mac_toolbox::Error::NotFound));
               assert_eq!(with_owner(33, || {
                              files.move_to_trash_at(system.files().volume_at(0).unwrap().id,
                                                     item.id,
                                                     WHEN)
                          }),
                          Err(mac_toolbox::Error::ServiceBusy));
               assert_eq!(with_owner(22, || second.position()), Ok(0));
               assert_eq!(close_owner(22), 1);
               let moved = with_owner(33, || {
                               files.move_to_trash_at(system.files().volume_at(0).unwrap().id,
                                                      item.id,
                                                      WHEN)
                           }).unwrap();
               assert_eq!(moved.id, item.id);
               assert_ne!(moved.parent, item.parent);
               assert_eq!(with_owner(33, || {
                              files.put_away_at(system.files().volume_at(0).unwrap().id,
                                                item.id,
                                                WHEN)
                          }).unwrap()
                            .parent,
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
                   assert_eq!(system.files().new_folder_at(system.files()
                                                                 .volume_at(0)
                                                                 .unwrap()
                                                                 .id,
                                                           ROOT_ID,
                                                           "Will roll back",
                                                           WHEN),
                              Err(mac_toolbox::Error::Io));
                   assert!(system.files()
                                 .lookup(system.files().volume_at(0).unwrap().id,
                                         ROOT_ID,
                                         "Will roll back")
                                 .is_err());
                   assert!(system.files()
                                 .lookup(system.files().volume_at(0).unwrap().id,
                                         ROOT_ID,
                                         "Source/Sample")
                                 .is_ok());
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
                    assert_eq!(system.files().new_folder_at(system.files()
                                                                  .volume_at(0)
                                                                  .unwrap()
                                                                  .id,
                                                            ROOT_ID,
                                                            "Broken",
                                                            WHEN),
                               Err(mac_toolbox::Error::RollbackFailed));
                    assert_eq!(system.files().new_folder_at(system.files()
                                                                  .volume_at(0)
                                                                  .unwrap()
                                                                  .id,
                                                            ROOT_ID,
                                                            "Denied",
                                                            WHEN),
                               Err(mac_toolbox::Error::RollbackFailed));
                    assert!(system.files()
                                  .volume(system.files().volume_at(0).unwrap().id)
                                  .unwrap()
                                  .read_only);
                });
}

#[test]
fn default_500mb_preallocated_catalog_and_map_nodes_are_supported()
{
    let fixture = Fixture::with_size(500 * 1024 * 1024);
    fixture.run(false, DiskFaults::default(), |system| {
               let files = system.files();
               let folder = files.new_folder_at(system.files().volume_at(0).unwrap().id,
                                                ROOT_ID,
                                                "Large disk folder",
                                                WHEN)
                                 .unwrap();
               assert_eq!(folder.name(), "Large disk folder");
               let sample =
                   files.lookup(system.files().volume_at(0).unwrap().id, ROOT_ID, "Source/Sample")
                        .unwrap();
               files.move_to_trash_at(system.files().volume_at(0).unwrap().id, sample.id, WHEN)
                    .unwrap();
               let usage = files.trash_usage(system.files().volume_at(0).unwrap().id).unwrap();
               assert_eq!(usage.items, 1);
               assert_eq!(usage.allocated_bytes, Some(16 * 1024));
               assert_eq!(files.empty_trash_at(system.files().volume_at(0).unwrap().id, WHEN)
                               .unwrap(),
                          1);
           });
    fixture.verify_hfs(":");
    fixture.run(true, DiskFaults::default(), |system| {
               assert!(system.files()
                             .lookup(system.files().volume_at(0).unwrap().id,
                                     ROOT_ID,
                                     "Large disk folder")
                             .is_ok());
               assert_eq!(system.files()
                                .trash_usage(system.files().volume_at(0).unwrap().id)
                                .unwrap()
                                .items,
                          0);
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
                   let first = files.lookup(system.files().volume_at(0).unwrap().id,
                                            ROOT_ID,
                                            "Source/Sample")
                                    .unwrap();
                   let second = files.lookup(system.files().volume_at(0).unwrap().id,
                                             ROOT_ID,
                                             "Other/Sample")
                                     .unwrap();
                   files.move_to_trash_at(system.files().volume_at(0).unwrap().id, first.id, WHEN)
                        .unwrap();
                   (first.id,
                    second.id,
                    files.trash_directory(system.files().volume_at(0).unwrap().id, false)
                         .unwrap()
                         .id)
               })
               .0;
    let before = fs::read(&fixture.disk).unwrap();
    let (_, activity) =
        fixture.run(false, DiskFaults::default(), |system| {
                   let files = system.files();
                   assert_eq!(files.move_to_trash_at(system.files().volume_at(0).unwrap().id,
                                                     second,
                                                     WHEN),
                              Err(mac_toolbox::Error::AlreadyExists));
                   assert_eq!(files.catalog(system.files().volume_at(0).unwrap().id, first)
                                   .unwrap()
                                   .parent,
                              trash);
                   assert_eq!(files.catalog(system.files().volume_at(0).unwrap().id, second)
                                   .unwrap()
                                   .name(),
                              "Sample");
                   assert_eq!(fork_bytes(system, (first).0, Fork::Data),
                              fork_bytes(system, (second).0, Fork::Data));
                   assert_eq!(fork_bytes(system, (first).0, Fork::Resource),
                              fork_bytes(system, (second).0, Fork::Resource));
               });
    assert_eq!(activity.writes, 0);
    assert_eq!(fs::read(&fixture.disk).unwrap(), before);
    // Create a conflicting replacement in the saved parent using independent HFS tooling.
    fixture.hfs("hmount", &[fixture.disk.to_str().unwrap()]);
    fixture.hfs("hcopy", &["-m", fixture.dir.join("sample.bin").to_str().unwrap(), ":Source:"]);
    fixture.hfs("humount", &[]);
    let before = fs::read(&fixture.disk).unwrap();
    fixture.run(false, DiskFaults::default(), |system| {
               assert_eq!(system.files()
                                .put_away_at(system.files().volume_at(0).unwrap().id, first, WHEN),
                          Err(mac_toolbox::Error::AlreadyExists));
               assert_eq!(system.files()
                                .catalog(system.files().volume_at(0).unwrap().id, first)
                                .unwrap()
                                .parent,
                          trash);
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
    let (_, activity) =
        fixture.run(false, DiskFaults::default(), |system| {
                   let files = system.files();
                   for path in ["System Folder", "System Folder/System", "System Folder/Finder",
                                "Source/Sample"]
                   {
                       let item =
                           files.lookup(system.files().volume_at(0).unwrap().id, ROOT_ID, path)
                                .unwrap();
                       assert_eq!(files.move_to_trash_at(system.files().volume_at(0).unwrap().id,
                                                         item.id,
                                                         WHEN),
                                  Err(mac_toolbox::Error::AccessDenied),
                                  "{path}");
                   }
               });
    assert_eq!(activity.writes, 0);
    assert_eq!(fs::read(&fixture.disk).unwrap(), before);
    fixture.run(false, DiskFaults::default(), |system| {
               let trash =
                   system.files()
                         .trash_directory_at(system.files().volume_at(0).unwrap().id, true, WHEN)
                         .unwrap();
               assert_eq!(system.files().move_to_trash_at(system.files().volume_at(0).unwrap().id,
                                                          trash.id,
                                                          WHEN),
                          Err(mac_toolbox::Error::AccessDenied));
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
    let id =
        fixture.run(false, DiskFaults::default(), |system| {
                   let files = system.files();
                   let item = files.lookup(system.files().volume_at(0).unwrap().id,
                                           ROOT_ID,
                                           "Source/Sample")
                                   .unwrap();
                   files.move_to_trash_at(system.files().volume_at(0).unwrap().id, item.id, WHEN)
                        .unwrap();
                   item.id
               })
               .0;
    let before = fs::read(&fixture.disk).unwrap();
    let (_, activity) = fixture.run(false, DiskFaults::default(), |system| {
                                   let file = system.files()
                                                    .open(system.files().volume_at(0).unwrap().id,
                                                          id,
                                                          Fork::Resource)
                                                    .unwrap();
                                   assert_eq!(system.files().empty_trash_at(system.files()
                                                                                  .volume_at(0)
                                                                                  .unwrap()
                                                                                  .id,
                                                                            WHEN),
                                              Err(mac_toolbox::Error::ServiceBusy));
                                   drop(file);
                               });
    assert_eq!(activity.writes, 0);
    assert_eq!(fs::read(&fixture.disk).unwrap(), before);
    fixture.run(false,
                DiskFaults { flush_at: Some(3),
                             ..Default::default() },
                |system| {
                    assert_eq!(system.files().empty_trash_at(system.files()
                                                                   .volume_at(0)
                                                                   .unwrap()
                                                                   .id,
                                                             WHEN),
                               Err(mac_toolbox::Error::Io));
                    assert_eq!(system.files()
                                     .catalog(system.files().volume_at(0).unwrap().id, id)
                                     .unwrap()
                                     .name(),
                               "Sample");
                    assert_eq!(fork_bytes(system, (id).0, Fork::Data).len(), 1700);
                    assert_eq!(fork_bytes(system, (id).0, Fork::Resource).len(), 900);
                });
    assert_eq!(fs::read(&fixture.disk).unwrap(), before);
    fixture.verify_hfs(":Trash:");
    fixture.hfs("hmount", &[fixture.disk.to_str().unwrap()]);
    fixture.hfs("hattrib", &["+l", ":Trash:Sample"]);
    fixture.hfs("humount", &[]);
    let locked = fs::read(&fixture.disk).unwrap();
    let (_, activity) = fixture.run(false, DiskFaults::default(), |system| {
                                   assert_eq!(system.files().empty_trash_at(system.files()
                                                                                  .volume_at(0)
                                                                                  .unwrap()
                                                                                  .id,
                                                                            WHEN),
                                              Err(mac_toolbox::Error::AccessDenied));
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
    let (_, activity) =
        fixture.run(false, DiskFaults::default(), |system| {
                   let files = system.files();
                   assert_eq!(files.trash_directory(system.files().volume_at(0).unwrap().id,
                                                    false),
                              Err(mac_toolbox::Error::AlreadyExists));
                   assert_eq!(files.trash_usage(system.files().volume_at(0).unwrap().id),
                              Err(mac_toolbox::Error::AlreadyExists));
                   assert_eq!(files.empty_trash_at(system.files().volume_at(0).unwrap().id, WHEN),
                              Err(mac_toolbox::Error::AlreadyExists));
               });
    assert_eq!(activity.writes, 0);
    assert_eq!(fs::read(&fixture.disk).unwrap(), before);
}

#[test]
fn on_disk_volume_locks_and_dirty_volume_state_are_respected()
{
    use std::io::{Read, Seek, SeekFrom, Write};
    for (set, clear, expected, readonly) in [(0x8000u16, 0u16, mac_toolbox::abi::READ_ONLY, 1),
                                             (0, 0x0100, mac_toolbox::abi::NOT_SUPPORTED, 0)]
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
                       assert_eq!(system.files()
                                        .volume(system.files().volume_at(0).unwrap().id)
                                        .unwrap()
                                        .read_only,
                                  readonly != 0);
                       assert!(system.files()
                                     .lookup(system.files().volume_at(0).unwrap().id,
                                             ROOT_ID,
                                             "Source/Sample")
                                     .is_ok());
                       assert_eq!(system.files().new_folder_at(system.files()
                                                                     .volume_at(0)
                                                                     .unwrap()
                                                                     .id,
                                                               ROOT_ID,
                                                               "Rejected",
                                                               WHEN),
                                  Err(mac_toolbox::Error::from_status(expected)));
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
        let system = unsafe { System::from_raw(services) }.unwrap();
        let name = b"\x04Caf\x8e";
        let mut call = FileCall { operation: NEW_FOLDER,
                                  volume: (system.files().volume_at(0).unwrap().id).0,
                                  cnid: (ROOT_ID).0,
                                  name: name.as_ptr(),
                                  name_length: name.len(),
                                  position: WHEN.mac_microseconds().unwrap(),
                                  ..Default::default() };
        let status = unsafe {
            (services.trap)(PSTRING_FILE,
                            core::ptr::from_mut(&mut call).cast(),
                            size_of::<FileCall>())
        };
        assert_eq!(status, 0);
        assert_eq!(&call.entry.name[..call.entry.name_length as usize], name);
        assert_eq!(system.files()
                         .lookup(system.files().volume_at(0).unwrap().id, ROOT_ID, "Café")
                         .unwrap()
                         .id,
                   mac_toolbox::Cnid(call.entry.id));
    }).unwrap();
    fixture.run(true, DiskFaults::default(), |system| {
               assert_eq!(system.files()
                                .lookup(system.files().volume_at(0).unwrap().id, ROOT_ID, "Café")
                                .unwrap()
                                .created_at(),
                          WHEN);
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
                   let original = files.lookup(system.files().volume_at(0).unwrap().id,
                                               ROOT_ID,
                                               "Source/Sample")
                                       .unwrap();
                   let source = files.catalog(system.files().volume_at(0).unwrap().id,
                                              original.parent)
                                     .unwrap();
                   let allocation = files.allocation_info(system.files().volume_at(0).unwrap().id,
                                                          original.id)
                                         .unwrap();
                   assert_eq!((allocation.data_bytes, allocation.resource_bytes),
                              (Some(2048), Some(1024)));
                   assert_eq!(files.allocation_info(system.files().volume_at(0).unwrap().id,
                                                    source.id)
                                   .unwrap(),
                              mac_toolbox::files::AllocationInfo { data_bytes:     Some(0),
                                                                   resource_bytes: Some(0) });
                   let data = fork_bytes(system, (original.id).0, Fork::Data);
                   let resources = fork_bytes(system, (original.id).0, Fork::Resource);
                   // Case-only changes keep their own identity, rather than colliding.
                   assert_eq!(files.rename_at(system.files().volume_at(0).unwrap().id,
                                              original.id,
                                              "sample",
                                              WHEN)
                                   .unwrap()
                                   .id,
                              original.id);
                   let renamed = files.rename_at(system.files().volume_at(0).unwrap().id,
                                                 original.id,
                                                 "Café",
                                                 WHEN)
                                      .unwrap();
                   assert_eq!(renamed.name(), "Café");
                   assert_eq!((renamed.id, renamed.parent), (original.id, original.parent));
                   assert_eq!((renamed.data_length, renamed.resource_length),
                              (original.data_length, original.resource_length));
                   assert_eq!((renamed.created, renamed.modified, renamed.backup),
                              (original.created, original.modified, original.backup));
                   assert_eq!((renamed.file_type, renamed.creator, renamed.finder_info),
                              (original.file_type, original.creator, original.finder_info));
                   files.rename_at(system.files().volume_at(0).unwrap().id,
                                   source.id,
                                   "Renamed Source",
                                   WHEN)
                        .unwrap();
                   assert_eq!(files.lookup(system.files().volume_at(0).unwrap().id,
                                           ROOT_ID,
                                           "Renamed Source/Café")
                                   .unwrap()
                                   .id,
                              original.id);
                   assert!(files.lookup(system.files().volume_at(0).unwrap().id,
                                        ROOT_ID,
                                        "Source")
                                .is_err());
                   assert_ne!(files.set_stationery_at(system.files().volume_at(0).unwrap().id,
                                                      original.id,
                                                      true,
                                                      WHEN)
                                   .unwrap()
                                   .finder_flags()
                                   .bits()
                              & 0x0800,
                              0);
                   assert_ne!(files.set_locked_at(system.files().volume_at(0).unwrap().id,
                                                  original.id,
                                                  true,
                                                  WHEN)
                                   .unwrap()
                                   .flags
                              & mac_toolbox::files::CatalogFlags::LOCKED,
                              mac_toolbox::files::CatalogFlags::NONE);
                   assert_eq!(files.rename_at(system.files().volume_at(0).unwrap().id,
                                              original.id,
                                              "Rejected",
                                              WHEN),
                              Err(mac_toolbox::Error::AccessDenied));
                   assert_eq!(files.set_stationery_at(system.files().volume_at(0).unwrap().id,
                                                      original.id,
                                                      false,
                                                      WHEN),
                              Err(mac_toolbox::Error::AccessDenied));
                   (original, allocation, data, resources, source.id)
               });
    fixture.verify_hfs(":Renamed Source:");
    fixture.run(false, DiskFaults::default(), |system| {
               let files = system.files();
               let renamed = files.lookup(system.files().volume_at(0).unwrap().id,
                                          ROOT_ID,
                                          "Renamed Source/Café")
                                  .unwrap();
               assert_eq!((renamed.id, renamed.parent), (original.id, source));
               assert_eq!(renamed.flags & mac_toolbox::files::CatalogFlags::from_bits_retain(!1),
                          original.flags & mac_toolbox::files::CatalogFlags::from_bits_retain(!1));
               assert_eq!(renamed.flags & mac_toolbox::files::CatalogFlags::from_bits_retain(1),
                          mac_toolbox::files::CatalogFlags::from_bits_retain(1));
               let mut expected_finder = original.finder_info;
               let flags = u16::from_be_bytes([expected_finder[8], expected_finder[9]]) | 0x0800;
               expected_finder[8..10].copy_from_slice(&flags.to_be_bytes());
               assert_eq!(renamed.finder_info, expected_finder);
               assert_eq!(files.allocation_info(system.files().volume_at(0).unwrap().id,
                                                original.id)
                               .unwrap(),
                          allocation);
               assert_eq!(fork_bytes(system, (original.id).0, Fork::Data), data);
               assert_eq!(fork_bytes(system, (original.id).0, Fork::Resource), resources);
               files.set_locked_at(system.files().volume_at(0).unwrap().id,
                                   original.id,
                                   false,
                                   WHEN)
                    .unwrap();
               assert_eq!(files.set_stationery_at(system.files().volume_at(0).unwrap().id,
                                                  original.id,
                                                  false,
                                                  WHEN)
                               .unwrap()
                               .finder_info,
                          original.finder_info);
               // The limit counts MacRoman bytes rather than Rust UTF8 bytes.
               let long = "é".repeat(31);
               assert_eq!(files.rename_at(system.files().volume_at(0).unwrap().id,
                                          original.id,
                                          &long,
                                          WHEN)
                               .unwrap()
                               .name(),
                          long);
               files.rename_at(system.files().volume_at(0).unwrap().id,
                               original.id,
                               "Sample",
                               WHEN)
                    .unwrap();
               assert_eq!(files.catalog(system.files().volume_at(0).unwrap().id, original.id)
                               .unwrap()
                               .finder_info,
                          original.finder_info);
           });
    fixture.verify_hfs(":Renamed Source:");
    let before = fs::read(&fixture.disk).unwrap();
    let (_, activity) = fixture.run(false, DiskFaults::default(), |system| {
                                   let files = system.files();
                                   files.rename_at(system.files().volume_at(0).unwrap().id,
                                                   original.id,
                                                   "Sample",
                                                   WHEN)
                                        .unwrap();
                                   files.set_locked_at(system.files().volume_at(0).unwrap().id,
                                                       original.id,
                                                       false,
                                                       WHEN)
                                        .unwrap();
                                   files.set_stationery_at(system.files().volume_at(0).unwrap().id,
                                                           original.id,
                                                           false,
                                                           WHEN)
                                        .unwrap();
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
                       let sample = files.lookup(system.files().volume_at(0).unwrap().id,
                                                 ROOT_ID,
                                                 "Source/Sample")
                                         .unwrap();
                       if readonly
                       {
                           for result in
                               [files.rename_at(system.files().volume_at(0).unwrap().id,
                                                sample.id,
                                                "New",
                                                WHEN),
                                files.set_locked_at(system.files().volume_at(0).unwrap().id,
                                                    sample.id,
                                                    true,
                                                    WHEN),
                                files.set_stationery_at(system.files().volume_at(0).unwrap().id,
                                                        sample.id,
                                                        true,
                                                        WHEN)]
                           {
                               assert_eq!(result, Err(mac_toolbox::Error::ReadOnly));
                           }
                       }
                       else
                       {
                           for name in
                               ["", ".", "..", "bad:name", "bad\0name", "😀", &"é".repeat(32)]
                           {
                               assert_eq!(files.rename_at(system.files().volume_at(0).unwrap().id,
                                                          sample.id,
                                                          name,
                                                          WHEN),
                                          Err(mac_toolbox::Error::BadArgument),
                                          "{name:?}");
                           }
                           assert_eq!(files.rename_at(system.files().volume_at(0).unwrap().id,
                                                      sample.parent,
                                                      "Other",
                                                      WHEN),
                                      Err(mac_toolbox::Error::AlreadyExists));
                           assert_eq!(files.rename_at(system.files().volume_at(0).unwrap().id,
                                                      ROOT_ID,
                                                      "Renamed disk",
                                                      WHEN),
                                      Err(mac_toolbox::Error::AccessDenied));
                           assert_eq!(files.set_locked_at(system.files().volume_at(0).unwrap().id,
                                                          sample.parent,
                                                          true,
                                                          WHEN),
                                      Err(mac_toolbox::Error::BadArgument));
                           assert_eq!(files.set_stationery_at(system.files()
                                                                    .volume_at(0)
                                                                    .unwrap()
                                                                    .id,
                                                              sample.parent,
                                                              true,
                                                              WHEN),
                                      Err(mac_toolbox::Error::BadArgument));
                           let open = files.open(system.files().volume_at(0).unwrap().id,
                                                 sample.id,
                                                 Fork::Resource)
                                           .unwrap();
                           for result in
                               [files.rename_at(system.files().volume_at(0).unwrap().id,
                                                sample.id,
                                                "New",
                                                WHEN),
                                files.rename_at(system.files().volume_at(0).unwrap().id,
                                                sample.parent,
                                                "New folder",
                                                WHEN),
                                files.set_locked_at(system.files().volume_at(0).unwrap().id,
                                                    sample.id,
                                                    true,
                                                    WHEN),
                                files.set_stationery_at(system.files().volume_at(0).unwrap().id,
                                                        sample.id,
                                                        true,
                                                        WHEN)]
                           {
                               assert_eq!(result, Err(mac_toolbox::Error::ServiceBusy));
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
                            let sample = files.lookup(system.files().volume_at(0).unwrap().id,
                                                      ROOT_ID,
                                                      "Source/Sample")
                                              .unwrap();
                            let result = match operation
                            {
                                0 =>
                                {
                                    files.rename_at(system.files().volume_at(0).unwrap().id,
                                                    sample.id,
                                                    "Renamed",
                                                    WHEN)
                                }
                                1 =>
                                {
                                    files.set_locked_at(system.files().volume_at(0).unwrap().id,
                                                        sample.id,
                                                        true,
                                                        WHEN)
                                }
                                _ =>
                                {
                                    files.set_stationery_at(system.files().volume_at(0).unwrap().id,
                                                            sample.id,
                                                            true,
                                                            WHEN)
                                }
                            };
                            let expected = if persistent
                            {
                                mac_toolbox::abi::ROLLBACK_FAILED
                            }
                            else
                            {
                                mac_toolbox::abi::IO_ERROR
                            };
                            assert_eq!(result, Err(mac_toolbox::Error::from_status(expected)));
                            if persistent
                            {
                                assert!(files.volume(system.files().volume_at(0).unwrap().id)
                                             .unwrap()
                                             .read_only);
                                assert_eq!(files.rename_at(system.files()
                                                                 .volume_at(0)
                                                                 .unwrap()
                                                                 .id,
                                                           sample.id,
                                                           "Again",
                                                           WHEN),
                                           Err(mac_toolbox::Error::from_status(expected)));
                            }
                            else
                            {
                                assert_eq!(files.catalog(system.files().volume_at(0).unwrap().id,
                                                         sample.id)
                                                .unwrap(),
                                           sample);
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
fn native_file_layout_carries_full_names_and_precise_dates()
{
    use core::mem::{offset_of, size_of};

    use mac_abi::files::{CatalogInfo, FileCall, VolumeInfo};
    assert_eq!(size_of::<CatalogInfo>(), 368);
    assert_eq!(size_of::<VolumeInfo>(), 368);
    assert_eq!(size_of::<FileCall>(), 832);
    assert_eq!(offset_of!(FileCall, entry), 464);
    assert_eq!(offset_of!(CatalogInfo, finder_info), 76);
    assert_eq!(offset_of!(CatalogInfo, name), 112);
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
               let original = files.lookup(system.files().volume_at(0).unwrap().id,
                                           ROOT_ID,
                                           "Source/Name locked")
                                   .unwrap();
               assert!(original.is_name_locked());
               assert!(!original.is_locked());
               assert!(!original.is_stationery());
               assert_eq!(files.rename_at(system.files().volume_at(0).unwrap().id,
                                          original.id,
                                          "New name",
                                          WHEN),
                          Err(mac_toolbox::Error::AccessDenied));
               let stationery = files.set_stationery_at(system.files().volume_at(0).unwrap().id,
                                                        original.id,
                                                        true,
                                                        WHEN)
                                     .unwrap();
               assert!(stationery.is_stationery());
               assert_eq!(stationery.finder_flags().bits(), 0x180e);
               let locked = files.set_locked_at(system.files().volume_at(0).unwrap().id,
                                                original.id,
                                                true,
                                                WHEN)
                                 .unwrap();
               assert!(locked.is_locked());
               assert_eq!(locked.finder_info, stationery.finder_info);
               let unlocked = files.set_locked_at(system.files().volume_at(0).unwrap().id,
                                                  original.id,
                                                  false,
                                                  WHEN)
                                   .unwrap();
               assert_eq!(unlocked.flags, original.flags);
               let cleared = files.set_stationery_at(system.files().volume_at(0).unwrap().id,
                                                     original.id,
                                                     false,
                                                     WHEN)
                                  .unwrap();
               assert_eq!(cleared.finder_info, original.finder_info);
           });
    fixture.verify_hfs(":Source:");
}

fn comment_text(system: &System<'_>, id: u32) -> String
{
    let mut output = String::new();
    system.files()
          .comment(system.files().volume_at(0).unwrap().id, mac_toolbox::Cnid(id), &mut output)
          .unwrap();
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
                   let sample = files.lookup(system.files().volume_at(0).unwrap().id,
                                             ROOT_ID,
                                             "Source/Sample")
                                     .unwrap();
                   assert_eq!(comment_text(system, (sample.id).0), "");
                   files.set_comment_at(system.files().volume_at(0).unwrap().id,
                                        sample.id,
                                        "Résumé\rSecond line",
                                        WHEN)
                        .unwrap();
                   files.set_comment_at(system.files().volume_at(0).unwrap().id,
                                        sample.parent,
                                        "Folder comment",
                                        WHEN)
                        .unwrap();
                   let db =
                       files.lookup(system.files().volume_at(0).unwrap().id, ROOT_ID, "Desktop DB")
                            .unwrap();
                   assert_eq!((db.file_type.bytes(), db.creator.bytes()), (*b"BTFL", *b"DMGR"));
                   assert_eq!(db.finder_flags().bits() & 0x4000, 0x4000);
                   assert_eq!(db.data_length, 64 * 1024);
                   assert_eq!(db.resource_length, 0);
                   assert_eq!(files.catalog(system.files().volume_at(0).unwrap().id, sample.id)
                                   .unwrap(),
                              sample);
                   assert_eq!(comment_text(system, (sample.id).0), "Résumé\rSecond line");
                   sample
               });
    fixture.verify_hfs(":");
    fixture.run(false, DiskFaults::default(), |system| {
               let files = system.files();
               assert_eq!(comment_text(system, (original.id).0), "Résumé\rSecond line");
               assert_eq!(comment_text(system, (original.parent).0), "Folder comment");
               let longest = "é".repeat(255);
               files.set_comment_at(system.files().volume_at(0).unwrap().id,
                                    original.id,
                                    &longest,
                                    WHEN)
                    .unwrap();
               assert_eq!(comment_text(system, (original.id).0), longest);
               files.set_comment_at(system.files().volume_at(0).unwrap().id, original.id, "", WHEN)
                    .unwrap();
               assert_eq!(comment_text(system, (original.id).0), "");
               assert_eq!(comment_text(system, (original.parent).0), "Folder comment");
               assert_eq!(files.catalog(system.files().volume_at(0).unwrap().id, original.id)
                               .unwrap(),
                          mac_toolbox::files::CatalogInfo { volume: system.files()
                                                                          .volume_at(0)
                                                                          .unwrap()
                                                                          .id,
                                                            ..original.clone() });
               assert_eq!(fork_bytes(system, (original.id).0, Fork::Data),
                          (0..1700).map(|n| (n * 17) as u8).collect::<Vec<_>>());
           });
    fixture.run(true, DiskFaults::default(), |system| {
               assert_eq!(comment_text(system, (original.id).0), "");
               assert_eq!(comment_text(system, (original.parent).0), "Folder comment");
               assert_eq!(system.files()
                                .catalog(system.files().volume_at(0).unwrap().id, original.id)
                                .unwrap(),
                          mac_toolbox::files::CatalogInfo { volume: system.files()
                                                                          .volume_at(0)
                                                                          .unwrap()
                                                                          .id,
                                                            ..original.clone() });
           });
}
#[test]
fn captured_original_desktop_comments_preserve_every_unknown_record_on_native_edits()
{
    let fixture = Fixture::new();
    let ((folder, sample, other), _) =
        fixture.run(true, DiskFaults::default(), |system| {
                   let files = system.files();
                   (files.lookup(system.files().volume_at(0).unwrap().id, ROOT_ID, "Source")
                         .unwrap()
                         .id,
                    files.lookup(system.files().volume_at(0).unwrap().id, ROOT_ID, "Source/Sample")
                         .unwrap()
                         .id,
                    files.lookup(system.files().volume_at(0).unwrap().id, ROOT_ID, "Other")
                         .unwrap()
                         .id)
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
            captured[start + 2..start + 6].copy_from_slice(&new.0.to_be_bytes());
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
               assert_eq!(comment_text(system, (folder).0), "Folder comment\rSecond line");
               assert_eq!(comment_text(system, (sample).0), "Renamed File");
               files.set_comment_at(system.files().volume_at(0).unwrap().id,
                                    sample,
                                    "Native updated comment",
                                    WHEN)
                    .unwrap();
               files.set_comment_at(system.files().volume_at(0).unwrap().id,
                                    other,
                                    "Inserted between existing comments",
                                    WHEN)
                    .unwrap();
               files.set_comment_at(system.files().volume_at(0).unwrap().id, folder, "", WHEN)
                    .unwrap();
               let db =
                   files.lookup(system.files().volume_at(0).unwrap().id, ROOT_ID, "Desktop DB")
                        .unwrap();
               let bytes = fork_bytes(system, (db.id).0, Fork::Data);
               assert_eq!(desktop_unknown_records(&bytes), unknown);
               assert_eq!(&bytes[120..248],
                          &original[120..248],
                          "Desktop header user data changed");
           });
    fixture.run(true, DiskFaults::default(), |system| {
               assert_eq!(comment_text(system, (sample).0), "Native updated comment");
               assert_eq!(comment_text(system, (other).0), "Inserted between existing comments");
               assert_eq!(comment_text(system, (folder).0), "");
           });
    fixture.verify_hfs(":");
}
#[test]
fn comments_reject_locked_readonly_overlong_and_unrepresentable_updates()
{
    let fixture = Fixture::new();
    let (id, _) = fixture.run(false, DiskFaults::default(), |system| {
                             let files = system.files();
                             let sample = files.lookup(system.files().volume_at(0).unwrap().id,
                                                       ROOT_ID,
                                                       "Source/Sample")
                                               .unwrap();
                             files.set_comment_at(system.files().volume_at(0).unwrap().id,
                                                  sample.id,
                                                  "Original comment",
                                                  WHEN)
                                  .unwrap();
                             files.set_locked_at(system.files().volume_at(0).unwrap().id,
                                                 sample.id,
                                                 true,
                                                 WHEN)
                                  .unwrap();
                             sample.id
                         });
    let before = fs::read(&fixture.disk).unwrap();
    fixture.run(false, DiskFaults::default(), |system| {
               assert_eq!(system.files().set_comment_at(system.files().volume_at(0).unwrap().id,
                                                        id,
                                                        "Changed",
                                                        WHEN),
                          Err(mac_toolbox::Error::AccessDenied));
               assert_eq!(comment_text(system, (id).0), "Original comment");
           });
    fixture.run(true, DiskFaults::default(), |system| {
               assert_eq!(system.files().set_comment_at(system.files().volume_at(0).unwrap().id,
                                                        id,
                                                        "Changed",
                                                        WHEN),
                          Err(mac_toolbox::Error::ReadOnly));
               assert_eq!(comment_text(system, (id).0), "Original comment");
           });
    assert_eq!(fs::read(&fixture.disk).unwrap(), before);
    fixture.run(false, DiskFaults::default(), |system| {
               system.files()
                     .set_locked_at(system.files().volume_at(0).unwrap().id, id, false, WHEN)
                     .unwrap();
           });
    let before = fs::read(&fixture.disk).unwrap();
    let (_, activity) = fixture.run(false, DiskFaults::default(), |system| {
                                   for text in [&"é".repeat(256), "😀"]
                                   {
                                       assert_eq!(system.files().set_comment_at(system.files()
                                                                                      .volume_at(0)
                                                                                      .unwrap()
                                                                                      .id,
                                                                                id,
                                                                                text,
                                                                                WHEN),
                                                  Err(mac_toolbox::Error::BadArgument));
                                   }
                                   system.files()
                                         .set_comment_at(system.files().volume_at(0).unwrap().id,
                                                         id,
                                                         "Original comment",
                                                         WHEN)
                                         .unwrap();
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
                           let sample = system.files()
                                              .lookup(system.files().volume_at(0).unwrap().id,
                                                      ROOT_ID,
                                                      "Source/Sample")
                                              .unwrap();
                           if existing
                           {
                               system.files()
                                     .set_comment_at(system.files().volume_at(0).unwrap().id,
                                                     sample.id,
                                                     "Existing",
                                                     WHEN)
                                     .unwrap();
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
                                mac_toolbox::abi::ROLLBACK_FAILED
                            }
                            else
                            {
                                mac_toolbox::abi::IO_ERROR
                            };
                            assert_eq!(system.files().set_comment_at(system.files()
                                                                           .volume_at(0)
                                                                           .unwrap()
                                                                           .id,
                                                                     id,
                                                                     "A new comment",
                                                                     WHEN),
                                       Err(mac_toolbox::Error::from_status(expected)));
                            if persistent
                            {
                                assert!(system.files()
                                              .volume(system.files().volume_at(0).unwrap().id)
                                              .unwrap()
                                              .read_only);
                            }
                            else
                            {
                                assert_eq!(comment_text(system, (id).0),
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
    let (target, _) =
        fixture.run(true, DiskFaults::default(), |system| {
                   system.files()
                         .lookup(system.files().volume_at(0).unwrap().id, ROOT_ID, "Source/Sample")
                         .unwrap()
                         .id
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
    let (_, activity) = fixture.run(false, DiskFaults::default(), |system| {
                                   assert_eq!(system.files().set_comment_at(system.files()
                                                                                  .volume_at(0)
                                                                                  .unwrap()
                                                                                  .id,
                                                                            target,
                                                                            &"x".repeat(255),
                                                                            WHEN),
                                              Err(mac_toolbox::Error::DiskFull));
                                   assert_eq!(comment_text(system, (target).0), "");
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
                    let sample =
                        system.files()
                              .lookup(system.files().volume_at(0).unwrap().id,
                                      ROOT_ID,
                                      "Source/Sample")
                              .unwrap();
                    assert_eq!(system.files().set_comment_at(system.files()
                                                                   .volume_at(0)
                                                                   .unwrap()
                                                                   .id,
                                                             sample.id,
                                                             "Rollback after payload flush",
                                                             WHEN),
                               Err(mac_toolbox::Error::Io));
                    assert_eq!(comment_text(system, (sample.id).0), "");
                    assert!(system.files()
                                  .lookup(system.files().volume_at(0).unwrap().id,
                                          ROOT_ID,
                                          "Desktop DB")
                                  .is_err());
                });
    assert_eq!(fs::read(&fixture.disk).unwrap(), before);
    fixture.verify_hfs(":Source:");
}

#[test]
fn native_data_save_is_atomic_owned_and_preserves_resource_forks()
{
    use mac_toolbox::files::DataFileSpec;
    let fixture = Fixture::new();
    let payload: Vec<_> = (0..20_003).map(|i| (i * 23) as u8).collect();
    let ((id, source, sample, resource, free), activity) =
        fixture.run(false, DiskFaults::default(), |system| {
                   let files = system.files();
                   let source =
                       files.lookup(system.files().volume_at(0).unwrap().id, ROOT_ID, "Source")
                            .unwrap();
                   let sample =
                       files.lookup(system.files().volume_at(0).unwrap().id, source.id, "Sample")
                            .unwrap();
                   let resource = fork_bytes(system, (sample.id).0, Fork::Resource);
                   let free = files.volume(system.files().volume_at(0).unwrap().id)
                                   .unwrap()
                                   .capacity
                                   .unwrap()
                                   .free_bytes;
                   let spec = DataFileSpec { volume:    system.files().volume_at(0).unwrap().id,
                                             parent:    source.id,
                                             name:      "Café.json",
                                             replacing: None,
                                             file_type: mac_toolbox::TypeCode::new(*b"TEXT"),
                                             creator:   mac_toolbox::TypeCode::new(*b"ttxt") };
                   {
                       let mut abandoned = files.create_data_at(spec, 20, WHEN).unwrap();
                       abandoned.write_all(b"unfinished").unwrap();
                       assert_eq!(abandoned.finish(), Err(mac_toolbox::Error::BadArgument));
                   }
                   assert!(files.lookup(system.files().volume_at(0).unwrap().id,
                                        source.id,
                                        "Café.json")
                                .is_err());
                   let mut writer = files.create_data_at(spec, payload.len(), WHEN).unwrap();
                   writer.write_all(&payload[..123]).unwrap();
                   writer.write_all(&payload[123..]).unwrap();
                   assert!(files.lookup(system.files().volume_at(0).unwrap().id,
                                        source.id,
                                        "Café.json")
                                .is_err());
                   let entry = writer.finish().unwrap();
                   assert_eq!(entry.created_at(), WHEN);
                   assert_eq!(entry.modified_at(), WHEN);
                   assert_eq!(entry.file_type, mac_toolbox::TypeCode::new(*b"TEXT"));
                   assert_eq!(fork_bytes(system, (entry.id).0, Fork::Data), payload);
                   let replacement = DataFileSpec { name: "Sample",
                                                    replacing: Some(sample.id),
                                                    ..spec };
                   let mut writer = files.create_data_at(replacement, 3, WHEN).unwrap();
                   writer.write_all(b"new").unwrap();
                   assert_eq!(writer.finish().unwrap().id, sample.id);
                   assert_eq!(fork_bytes(system, (sample.id).0, Fork::Data), b"new");
                   assert_eq!(fork_bytes(system, (sample.id).0, Fork::Resource), resource);
                   let mut duplicate = files.create_data_at(spec, 0, WHEN).unwrap();
                   duplicate.write_all(&[]).unwrap();
                   assert_eq!(duplicate.finish(), Err(mac_toolbox::Error::AlreadyExists));
                   let mut empty = files.create_data_at(DataFileSpec { name: "Empty",
                                                                       ..spec },
                                                        0,
                                                        WHEN)
                                        .unwrap();
                   empty.write_all(&[]).unwrap();
                   assert_eq!(empty.finish().unwrap().data_length, 0);
                   (entry.id, source.id, sample.id, resource, free)
               });
    assert!(activity.writes > 0 && activity.flushes > 0);
    fixture.run(false, DiskFaults::default(), |system| {
               assert_eq!(fork_bytes(system, (id).0, Fork::Data), payload);
               assert_eq!(fork_bytes(system, (sample).0, Fork::Resource), resource);
               assert_eq!(system.files()
                                .catalog(system.files().volume_at(0).unwrap().id, source)
                                .unwrap()
                                .child_count,
                          Some(3));
               assert!(system.files()
                             .volume(system.files().volume_at(0).unwrap().id)
                             .unwrap()
                             .capacity
                             .unwrap()
                             .free_bytes
                       < free);
           });
}

#[test]
fn native_data_write_failures_preserve_live_files_and_abort_unpublished_creations()
{
    use mac_toolbox::files::DataFileSpec;
    let fixture = Fixture::new();
    let original = fs::read(&fixture.disk).unwrap();
    let ((volume, parent, id, data, resource), _) =
        fixture.run(false, DiskFaults::default(), |system| {
                   let e =
                       system.files()
                             .lookup(system.files().volume_at(0).unwrap().id,
                                     ROOT_ID,
                                     "Source/Sample")
                             .unwrap();
                   (system.files().volume_at(0).unwrap().id,
                    e.parent,
                    e.id,
                    fork_bytes(system, (e.id).0, Fork::Data),
                    fork_bytes(system, (e.id).0, Fork::Resource))
               });
    let spec = DataFileSpec { volume,
                              parent,
                              name: "Sample",
                              replacing: Some(id),
                              file_type: mac_toolbox::TypeCode::new(*b"TEXT"),
                              creator: mac_toolbox::TypeCode::new(*b"TEST") };
    // Sweep every write and flush in the successful operation, including payload
    // staging, journal publication and rollback of partially written sectors.
    let (_, activity) =
        fixture.run(false, DiskFaults::default(), |system| {
                   let mut w = system.files()
                                     .create_data_at(DataFileSpec { volume: system.files()
                                                                                  .volume_at(0)
                                                                                  .unwrap()
                                                                                  .id,
                                                                    ..spec },
                                                     1800,
                                                     WHEN)
                                     .unwrap();
                   w.write_all(&[42; 1800]).unwrap();
                   w.finish().unwrap();
               });
    for fault in (0..activity.writes).map(|n| {
                                         DiskFaults { write_at: Some(n),
                                                      partial_write: true,
                                                      ..Default::default() }
                                     })
                                     .chain((0..activity.flushes).map(|n| {
                                                                     DiskFaults { flush_at: Some(n),
                                                             ..Default::default() }
                                                                 }))
    {
        fs::write(&fixture.disk, &original).unwrap();
        fixture.run(false, fault, |system| {
                   let mut w = system.files()
                                     .create_data_at(DataFileSpec { volume: system.files()
                                                                                  .volume_at(0)
                                                                                  .unwrap()
                                                                                  .id,
                                                                    ..spec },
                                                     1800,
                                                     WHEN)
                                     .unwrap();
                   w.write_all(&[42; 1800]).unwrap();
                   assert!(w.finish().is_err(), "fault {fault:?}, activity {activity:?}");
               });
        fixture.run(false, DiskFaults::default(), |system| {
                   assert_eq!(fork_bytes(system, (id).0, Fork::Data), data);
                   assert_eq!(fork_bytes(system, (id).0, Fork::Resource), resource);
               });
    }
    fixture.run(true, DiskFaults::default(), |system| {
               assert!(matches!(system.files().create_data_at(DataFileSpec { volume:
                                                                                 system.files()
                                                                                       .volume_at(0)
                                                                                       .unwrap()
                                                                                       .id,
                                                                             ..spec },
                                                              0,
                                                              WHEN),
                                Err(mac_toolbox::Error::ReadOnly)))
           });
}

#[test]
fn standard_file_filters_folders_save_as_replace_and_cancel_through_sdk()
{
    use mac_system::{testing::{standard_file_action as action, standard_file_entries as entries},
                     toolbox::standard_file::Action as A};
    use mac_toolbox::standard_file::{Location, Progress, TypeFilter};
    let fixture = Fixture::new();
    fixture.run(false, DiskFaults::default(), |system| {
               let files = system.files();
               let source =
                   files.lookup(system.files().volume_at(0).unwrap().id, ROOT_ID, "Source")
                        .unwrap();
               let sample =
                   files.lookup(system.files().volume_at(0).unwrap().id, source.id, "Sample")
                        .unwrap();
               let loc = Location { volume:    system.files().volume_at(0).unwrap().id,
                                    directory: source.id };
               let dialog =
                   system.standard_file()
                         .begin_get(loc, TypeFilter::Types(&[mac_toolbox::TypeCode::new(*b"APPL")]))
                         .unwrap();
               assert!(entries().is_empty());
               action(A::Accept, 0).unwrap();
               assert!(matches!(dialog.poll().unwrap(), Progress::Pending));
               action(A::Parent, 0).unwrap();
               assert!(entries().iter().all(|e| e.is_directory()));
               action(A::Cancel, 0).unwrap();
               assert!(matches!(dialog.poll().unwrap(), Progress::Cancelled));
               drop(dialog);
               let dialog =
                   system.standard_file()
                         .begin_get(loc, TypeFilter::Types(&[mac_toolbox::TypeCode::new(*b"TEXT")]))
                         .unwrap();
               assert_eq!(entries().len(), 1);
               action(A::Accept, 0).unwrap();
               match dialog.poll().unwrap()
               {
                   Progress::Selected(s) => assert_eq!(s.entry.id, sample.id),
                   _ => panic!("missing selection")
               };
               drop(dialog);
               let dialog = system.standard_file()
                                  .begin_put(loc, "Save JSON as:", "resources.json")
                                  .unwrap();
               action(A::Accept, 0).unwrap();
               match dialog.poll().unwrap()
               {
                   Progress::Selected(s) =>
                   {
                       assert!(!s.replacing);
                       assert_eq!(s.name(), "resources.json");
                       assert_eq!(s.entry.id, mac_toolbox::Cnid(0));
                       assert_eq!(s.entry.parent, source.id);
                   }
                   _ => panic!("missing save selection")
               };
               drop(dialog);
               assert!(files.lookup(system.files().volume_at(0).unwrap().id,
                                    source.id,
                                    "resources.json")
                            .is_err());
               let dialog = system.standard_file().begin_put(loc, "Save as:", "Sample").unwrap();
               action(A::Accept, 0).unwrap();
               assert!(matches!(dialog.poll().unwrap(), Progress::Pending));
               action(A::Cancel, 0).unwrap();
               assert!(matches!(dialog.poll().unwrap(), Progress::Pending));
               action(A::Accept, 0).unwrap();
               action(A::Confirm, 0).unwrap();
               match dialog.poll().unwrap()
               {
                   Progress::Selected(s) =>
                   {
                       assert!(s.replacing);
                       assert_eq!(s.entry.id, sample.id);
                   }
                   _ => panic!("missing replacement")
               };
               drop(dialog);
               let dialog = system.standard_file().begin_put(loc, "Save as:", "x").unwrap();
               action(A::NewFolder, 0).unwrap();
               action(A::SelectName, 0).unwrap();
               for c in "Exports".chars()
               {
                   action(A::Edit(c), 0).unwrap();
               }
               action(A::Confirm, WHEN.mac_seconds().unwrap()).unwrap();
               let folder =
                   files.lookup(system.files().volume_at(0).unwrap().id, source.id, "Exports")
                        .unwrap();
               assert!(folder.is_directory());
               action(A::Cancel, 0).unwrap();
               drop(dialog);
           });
}

#[test]
fn shared_file_dialog_and_writer_ids_reject_reused_and_foreign_slots()
{
    use mac_abi::{files::*, standard_file as sf};
    use mac_system::testing::files::with_owner;
    use mac_toolbox::Error;
    let fixture = Fixture::new();
    fixture.run(false, DiskFaults::default(), |system| {
               let owner = mac_system::testing::inspection::add("Dialog owner", 0);
               let foreign_owner = mac_system::testing::inspection::add("Foreign dialog owner", 0);
               let client = system.client();
               let parent = system.files()
                                  .lookup(system.files().volume_at(0).unwrap().id,
                                          mac_toolbox::Cnid(ROOT_ID),
                                          "Source")
                                  .unwrap()
                                  .id;
               let mut write = DataWriteCall { operation: BEGIN_DATA_WRITE,
                                               volume:
                                                   (system.files().volume_at(0).unwrap().id).0,
                                               parent: parent.0,
                                               length: 1,
                                               ..Default::default() };
               with_owner(owner, || client.data_write(&mut write, "JSON", &[])).unwrap();
               let old = write.handle;
               write.operation = APPEND_DATA_WRITE;
               assert_eq!(with_owner(foreign_owner, || client.data_write(&mut write, "", b"x")),
                          Err(Error::AccessDenied));
               write.operation = ABORT_DATA_WRITE;
               with_owner(owner, || client.data_write(&mut write, "", &[])).unwrap();
               write = DataWriteCall { operation: BEGIN_DATA_WRITE,
                                       volume: (system.files().volume_at(0).unwrap().id).0,
                                       parent: parent.0,
                                       length: 1,
                                       ..Default::default() };
               with_owner(owner, || client.data_write(&mut write, "JSON", &[])).unwrap();
               assert_eq!(old & 0xffff, write.handle & 0xffff);
               assert_ne!(old, write.handle);
               let mut stale = DataWriteCall { operation: APPEND_DATA_WRITE,
                                               handle: old,
                                               ..Default::default() };
               assert_eq!(with_owner(owner, || client.data_write(&mut stale, "", b"x")),
                          Err(Error::NotFound));

               let mut call =
                   sf::StandardFileCall { volume: (system.files().volume_at(0).unwrap().id).0,
                                          directory: parent.0,
                                          ..Default::default() };
               with_owner(owner, || client.standard_file(&mut call)).unwrap();
               let first = call.handle;
               call.operation = sf::POLL;
               assert_eq!(with_owner(foreign_owner, || client.standard_file(&mut call)),
                          Err(Error::AccessDenied));
               call.operation = sf::DISPOSE;
               with_owner(owner, || client.standard_file(&mut call)).unwrap();
               call = sf::StandardFileCall { volume: (system.files().volume_at(0).unwrap().id).0,
                                             directory: parent.0,
                                             ..Default::default() };
               with_owner(owner, || client.standard_file(&mut call)).unwrap();
               let pools = with_owner(owner, || {
                   system.inspection()
                         .snapshot(mac_toolbox::inspection::Scope::Own)
                         .unwrap()
                         .ledger(0)
                         .unwrap()
               });
               let pool = with_owner(owner, || {
                   (0..pools.len()).map(|index| pools.at(index).unwrap())
                                   .find(|pool| pool.kind == *b"SFDL")
                                   .unwrap()
               });
               assert_eq!((pool.live, pool.slots),
                          (1, 1),
                          "replacement must reuse the sole dialog slot");
               assert!(first > u64::from(u32::MAX) && call.handle > u64::from(u32::MAX));
               assert_ne!(first, call.handle);
               let mut stale = sf::StandardFileCall { operation: sf::POLL,
                                                      handle: first,
                                                      ..Default::default() };
               assert_eq!(with_owner(owner, || client.standard_file(&mut stale)),
                          Err(Error::NotFound));
               stale.handle = u64::from(write.handle);
               assert_eq!(with_owner(owner, || client.standard_file(&mut stale)),
                          Err(Error::NotFound));
               let mut foreign = FileCall { operation: TELL,
                                            handle: write.handle,
                                            ..Default::default() };
               assert_eq!(with_owner(owner, || client.file(&mut foreign, "", &mut [])),
                          Err(Error::NotFound));
               write.operation = ABORT_DATA_WRITE;
               with_owner(owner, || client.data_write(&mut write, "", &[])).unwrap();
               call.operation = sf::DISPOSE;
               with_owner(owner, || client.standard_file(&mut call)).unwrap();
           });
}

#[test]
fn completed_dialogs_do_not_block_or_dispose_a_new_active_dialog_and_state_walks()
{
    use mac_system::{testing::{files::with_owner, inspection as inspect,
                               standard_file_action as action},
                     toolbox::standard_file::Action};
    use mac_toolbox::standard_file::{Location, Progress, TypeFilter};
    let fixture = Fixture::new();
    fixture.run(false, DiskFaults::default(), |system| {
               let owner = inspect::add("Retained dialogs", 0);
               let other = inspect::add("Independent dialog", 0);
               let location = Location { volume:    system.files().volume_at(0).unwrap().id,
                                         directory: mac_toolbox::Cnid::ROOT };
               with_owner(owner, || {
                   let mut completed = Vec::new();
                   for tick in 0..12
                   {
                       let dialog =
                           system.standard_file().begin_get(location, TypeFilter::All).unwrap();
                       action(Action::Cancel, tick).unwrap();
                       assert!(matches!(dialog.poll().unwrap(), Progress::Cancelled));
                       completed.push(dialog);
                   }
                   let active =
                       system.standard_file().begin_get(location, TypeFilter::All).unwrap();
                   with_owner(other, || {
                       let independent =
                           system.standard_file().begin_get(location, TypeFilter::All).unwrap();
                       assert!(matches!(independent.poll().unwrap(), Progress::Pending));
                       drop(independent);
                   });
                   let error =
                       system.standard_file().begin_get(location, TypeFilter::All).err().unwrap();
                   assert_eq!(error, mac_toolbox::Error::ServiceBusy);
                   assert!(error.to_string().contains("busy"));
                   drop(completed);
                   assert!(matches!(active.poll().unwrap(), Progress::Pending));
                   action(Action::Cancel, 20).unwrap();
                   assert!(matches!(active.poll().unwrap(), Progress::Cancelled),
                           "disposing old dialogs must not clear the new active-dialog index");
                   drop(active);
                   let members =
                       system.inspection().snapshot(mac_toolbox::inspection::Scope::Own).unwrap();
                   let pools = members.ledger(0).unwrap();
                   let dialogs = (0..pools.len()).map(|index| pools.at(index).unwrap())
                                                 .find(|pool| pool.kind == *b"SFDL")
                                                 .unwrap();
                   assert_eq!((dialogs.live, dialogs.slots),
                              (0, 16),
                              "dialog capacity grows per owner beyond the old eight-entry table");
                   let failed = inspect::walk_registered(owner, true, false);
                   assert_eq!((failed.report.invalidated, failed.report.quarantined), (1, 1));
                   assert!(failed.failures[0].contains("SFST")
                           && failed.failures[0].contains("no registered cleanup provider"));
                   let retry = inspect::walk_registered(owner, false, true);
                   assert_eq!((retry.report.released, retry.report.quarantined), (1, 0));
                   let final_state = members.at(0).unwrap();
                   assert_eq!((final_state.live, final_state.pending, final_state.quarantined),
                              (0, 0, 0));
               });
           });
}

#[test]
fn legacy_standard_file_pascal_names_and_writer_share_native_validation()
{
    use mac_abi::{files::*, standard_file as sf, traps};
    use mac_system::{testing::{files::with_owner, standard_file_action as action},
                     toolbox::standard_file::Action as A};
    let fixture = Fixture::new();
    with_file_disk(&fixture.disk, false, DiskFaults::default(), |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let owner = mac_system::testing::inspection::add("Legacy dialog owner", 0);
        with_owner(owner, || {
            let name = b"\x09Caf\x8e.json";
            let mut dialog =
                sf::StandardFileCall { mode: sf::PUT,
                                       volume: (system.files().volume_at(0).unwrap().id).0,
                                       directory: ROOT_ID,
                                       name_length: name.len() as u32,
                                       ..Default::default() };
            dialog.name[..name.len()].copy_from_slice(name);
            let invoke = |c: &mut sf::StandardFileCall| unsafe {
                (services.trap)(traps::PSTRING_STANDARD_FILE,
                                core::ptr::from_mut(c).cast(),
                                size_of::<sf::StandardFileCall>())
            };
            assert_eq!(invoke(&mut dialog), 0);
            action(A::Accept, 0).unwrap();
            dialog.operation = sf::POLL;
            assert_eq!(invoke(&mut dialog), 0);
            assert_eq!(dialog.state, sf::ACCEPTED);
            assert_eq!(&dialog.entry.name[..dialog.entry.name_length as usize], name);
            dialog.operation = sf::DISPOSE;
            assert_eq!(invoke(&mut dialog), 0);
            let stale = dialog.handle;
            let mut next =
                sf::StandardFileCall { volume: (system.files().volume_at(0).unwrap().id).0,
                                       directory: ROOT_ID,
                                       ..Default::default() };
            system.client().standard_file(&mut next).unwrap();
            let pools = system.inspection()
                              .snapshot(mac_toolbox::inspection::Scope::Own)
                              .unwrap()
                              .ledger(0)
                              .unwrap();
            let pool = (0..pools.len()).map(|index| pools.at(index).unwrap())
                                       .find(|pool| pool.kind == *b"SFDL")
                                       .unwrap();
            assert_eq!((pool.live, pool.slots), (1, 1));
            assert_ne!(next.handle, stale);
            dialog.operation = sf::POLL;
            assert_eq!(invoke(&mut dialog), i64::from(mac_toolbox::abi::NOT_FOUND));
            next.operation = sf::DISPOSE;
            system.client().standard_file(&mut next).unwrap();
            let mut w = DataWriteCall { operation: BEGIN_DATA_WRITE,
                                        volume: (system.files().volume_at(0).unwrap().id).0,
                                        parent: ROOT_ID,
                                        name: name.as_ptr(),
                                        name_length: name.len(),
                                        length: 2,
                                        when: WHEN.mac_microseconds().unwrap(),
                                        file_type: *b"TEXT",
                                        creator: *b"ttxt",
                                        ..Default::default() };
            let call = |c: &mut DataWriteCall| unsafe {
                (services.trap)(traps::PSTRING_FILE,
                                core::ptr::from_mut(c).cast(),
                                size_of::<DataWriteCall>())
            };
            assert_eq!(with_owner(11, || call(&mut w)), 0);
            let first = w.handle;
            w.operation = APPEND_DATA_WRITE;
            w.data = b"{}".as_ptr();
            w.data_length = 2;
            assert_eq!(with_owner(22, || call(&mut w)), i64::from(mac_toolbox::abi::ACCESS_DENIED));
            assert_eq!(with_owner(11, || call(&mut w)), 0);
            w.operation = COMMIT_DATA_WRITE;
            assert_eq!(with_owner(11, || call(&mut w)), 0);
            assert_eq!(&w.entry.name[..w.entry.name_length as usize], name);
            assert_eq!(system.files()
                             .lookup(system.files().volume_at(0).unwrap().id,
                                     mac_toolbox::Cnid(ROOT_ID),
                                     "Café.json")
                             .unwrap()
                             .id,
                       mac_toolbox::Cnid(w.entry.id));
            w.operation = BEGIN_DATA_WRITE;
            w.expected_cnid = w.entry.id;
            assert_eq!(with_owner(11, || call(&mut w)), 0);
            assert_eq!(w.handle & 0xffff, first & 0xffff);
            let mut old = DataWriteCall { operation: ABORT_DATA_WRITE,
                                          handle: first,
                                          ..Default::default() };
            assert_eq!(with_owner(11, || call(&mut old)), i64::from(mac_toolbox::abi::NOT_FOUND));
            w.operation = ABORT_DATA_WRITE;
            assert_eq!(with_owner(11, || call(&mut w)), 0);
        });
    }).unwrap();
}

#[test]
fn classic_hfs_positions_preserve_dates_and_forks_through_native_and_legacy_gates()
{
    use mac_toolbox::{Error, Point};
    let fixture = Fixture::new();
    let ((file, folder, before, first), activity) =
        fixture.run(false, DiskFaults::default(), |system| {
                   let files = system.files();
                   let volume = files.volume_at(0).unwrap().id;
                   let folder = files.lookup(volume, ROOT_ID, "Source").unwrap();
                   let file = files.lookup(volume, folder.id, "Sample").unwrap();
                   files.set_locked_at(volume, file.id, true, WHEN).unwrap();
                   let mut open = files.open(volume, file.id, Fork::Data).unwrap();
                   let first = Point { h: 120,
                                       v: -45 };
                   let placed =
                       files.set_icon_location_at(volume, file.id, Some(first), WHEN).unwrap();
                   assert_eq!(placed.saved_icon_location(), Some(first));
                   assert_eq!((placed.created, placed.modified, placed.backup),
                              (file.created, file.modified, file.backup));
                   assert_eq!((placed.data_length, placed.resource_length),
                              (file.data_length, file.resource_length));
                   assert_eq!(&placed.finder_info[..8], &file.finder_info[..8]);
                   assert_eq!(&placed.finder_info[14..], &file.finder_info[14..]);
                   assert_eq!(placed.flags.bits() & 1, 1);
                   let mut bytes = [0; 64];
                   assert_eq!(open.read(&mut bytes).unwrap(), bytes.len());
                   assert_eq!(bytes, core::array::from_fn(|n| (n * 17) as u8));
                   let dir =
                       files.set_icon_location_at(volume, folder.id, Some(first), WHEN).unwrap();
                   assert_eq!(dir.saved_icon_location(), Some(first));
                   assert_eq!(dir.folder_window_bounds(), folder.folder_window_bounds());
                   (file.id, folder.id, file, first)
               });
    assert!(activity.writes > 0);
    let (_, activity) =
        fixture.run(false, DiskFaults::default(), |system| {
                   let files = system.files();
                   let volume = files.volume_at(0).unwrap().id;
                   let watch = files.watch_directory(volume, before.parent).unwrap();
                   files.set_icon_location_at(volume, file, Some(first), WHEN).unwrap();
                   assert!(watch.poll().unwrap().is_none());
               });
    assert_eq!(activity.writes, 0);
    let second = Point { h: -73,
                         v: 81 };
    with_file_disk(&fixture.disk, false, DiskFaults::default(), |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let volume = system.files().volume_at(0).unwrap().id;
        let mut request = mac_abi::files::FileCall { operation:
                                                         mac_abi::files::SET_ICON_LOCATION,
                                                     volume: volume.0,
                                                     cnid: file.0,
                                                     fork: 1,
                                                     index: u32::from(second.h as u16)
                                                            | (u32::from(second.v as u16) << 16),
                                                     position: WHEN.mac_microseconds().unwrap(),
                                                     ..Default::default() };
        let status = unsafe {
            (services.trap)(mac_abi::traps::PSTRING_FILE,
                            core::ptr::from_mut(&mut request).cast(),
                            core::mem::size_of_val(&request))
        };
        assert_eq!(status, 0);
        assert_eq!(request.entry.id, file.0);
        assert_eq!(system.files().catalog(volume, file).unwrap().saved_icon_location(),
                   Some(second));
        request.index = u32::MAX;
        let status = unsafe {
            (services.trap)(mac_abi::traps::PSTRING_FILE,
                            core::ptr::from_mut(&mut request).cast(),
                            core::mem::size_of_val(&request))
        };
        assert_eq!(status, i64::from(mac_abi::BAD_ARGUMENT));
    }).unwrap();
    fixture.run(true, DiskFaults::default(), |system| {
               let files = system.files();
               let volume = files.volume_at(0).unwrap().id;
               let entry = files.catalog(volume, file).unwrap();
               assert_eq!(entry.saved_icon_location(), Some(second));
               assert_eq!((entry.created, entry.modified, entry.backup),
                          (before.created, before.modified, before.backup));
               assert_eq!(files.catalog(volume, folder).unwrap().saved_icon_location(),
                          Some(first));
               assert_eq!(files.set_icon_location_at(volume, file, None, WHEN),
                          Err(Error::ReadOnly));
           });
    fixture.hfs("hmount", &[fixture.disk.to_str().unwrap()]);
    let payload = fixture.dir.join("placed.bin");
    fixture.hfs("hcopy", &["-m", ":Source:Sample", payload.to_str().unwrap()]);
    fixture.hfs("humount", &[]);
    let extracted = mac_formats::macbinary::decode(&fs::read(payload).unwrap()).unwrap();
    assert_eq!(extracted.data, (0..1700).map(|n| (n * 17) as u8).collect::<Vec<_>>());
    assert_eq!(extracted.resources, (0..900).map(|n| (n * 31) as u8).collect::<Vec<_>>());
}

#[test]
fn classic_moves_preserve_identity_placement_forks_and_comments_across_remounts()
{
    let fixture = Fixture::new();
    let ((parent, entry, data, resources), _) =
        fixture.run(false, DiskFaults::default(), |system| {
                   let volume = system.files().volume_at(0).unwrap().id;
                   let sample = system.files().lookup(volume, ROOT_ID, "Source/Sample").unwrap();
                   file_moves::exercise(system, volume, sample.id, WHEN)
               });
    fixture.run(true, DiskFaults::default(), |system| {
               let volume = system.files().volume_at(0).unwrap().id;
               file_moves::persisted(system, volume, &entry, &data, &resources);
               assert_eq!(system.files().move_into_at(volume, entry.id, ROOT_ID, WHEN),
                          Err(mac_toolbox::Error::ReadOnly));
           });
    let (_, activity) = fixture.run(false, DiskFaults::default(), |system| {
                                   let volume = system.files().volume_at(0).unwrap().id;
                                   let watch =
                                       system.files().watch_directory(volume, parent).unwrap();
                                   assert_eq!(system.files()
                                                    .move_into_at(volume, entry.id, parent, WHEN)
                                                    .unwrap()
                                                    .id,
                                              entry.id);
                                   assert_eq!(watch.poll().unwrap(), None);
                               });
    assert_eq!(activity.writes, 0);
    fixture.verify_hfs(":Container:Destination:Sample");
}

#[test]
fn classic_move_write_and_flush_faults_restore_catalog_threads_and_forks()
{
    let fixture = Fixture::new();
    let ((file, parent), _) = fixture.run(false, DiskFaults::default(), |system| {
                                         let files = system.files();
                                         let volume = files.volume_at(0).unwrap().id;
                                         (files.lookup(volume, ROOT_ID, "Source/Sample").unwrap(),
                                          files.lookup(volume, ROOT_ID, "Other").unwrap().id)
                                     });
    let before = fs::read(&fixture.disk).unwrap();
    for fault in [DiskFaults { write_at: Some(3),
                               partial_write: true,
                               ..Default::default() },
                  DiskFaults { flush_at: Some(1),
                               ..Default::default() }]
    {
        fixture.run(false, fault, |system| {
                   let volume = system.files().volume_at(0).unwrap().id;
                   assert_eq!(system.files().move_into_at(volume, file.id, parent, WHEN),
                              Err(mac_toolbox::Error::Io));
               });
        assert_eq!(fs::read(&fixture.disk).unwrap(), before, "move rollback: {fault:?}");
        fixture.run(true, DiskFaults::default(), |system| {
                   let volume = system.files().volume_at(0).unwrap().id;
                   assert_eq!(system.files().catalog(volume, file.id).unwrap(),
                              mac_toolbox::files::CatalogInfo { volume,
                                                                ..file.clone() });
               });
    }
    fixture.verify_hfs(":Source:Sample");
}

#[path = "support/icon_moves.rs"]
mod icon_moves;
#[test]
fn atomic_icon_groups_validate_every_item_before_publishing_moves_and_positions()
{
    let fixture = Fixture::new();
    fixture.run(false, DiskFaults::default(), |system| {
               let volume = system.files().volume_at(0).unwrap().id;
               let file = system.files().lookup(volume, ROOT_ID, "Source/Sample").unwrap().id;
               icon_moves::exercise(system, volume, file, WHEN);
           });
}

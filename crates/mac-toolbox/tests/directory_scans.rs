//! Incremental filesystem acceptance through the aggregate userspace SDK.
#![cfg(not(target_arch = "riscv64"))]
use std::{collections::BTreeMap,
          fs,
          path::PathBuf,
          process::Command,
          sync::atomic::{AtomicUsize, Ordering}};

use mac_system::testing::{DiskFaults, with_file_disk};
use mac_toolbox::{Error, System, Timestamp,
                  files::{DirectoryCursor, ROOT_ID, ScanStep}};
static NEXT: AtomicUsize = AtomicUsize::new(0);
const WHEN: Timestamp = Timestamp::from_unix_seconds(1_792_326_896);
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
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
        let dir = root.join("results/system7").join(format!("directory-scans-{}-{}",
                                                            std::process::id(),
                                                            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(dir.join("hfs-state")).unwrap();
        let disk = dir.join("disk.hfs");
        fs::File::create(&disk).unwrap().set_len(8 * 1024 * 1024).unwrap();
        let f = Self { root,
                       dir,
                       disk };
        f.hfs("hformat", &["-l", "Scan Test", f.disk.to_str().unwrap()]);
        let file = mac_formats::macbinary::MacFile { name:      b"Payload".to_vec(),
                                                     file_type: *b"TEXT",
                                                     creator:   *b"TEST",
                                                     flags:     0,
                                                     created:   WHEN.mac_seconds().unwrap(),
                                                     modified:  WHEN.mac_seconds().unwrap(),
                                                     data:      vec![17; 1700],
                                                     resources: vec![23; 900] };
        fs::write(f.dir.join("payload.bin"),mac_formats::macbinary::encode(&file).unwrap()).unwrap();
        f.hfs("hmount", &[f.disk.to_str().unwrap()]);
        f.hfs("hmkdir", &[":Earlier"]);
        for n in 0..40
        {
            f.hfs("hcopy",
                  &["-m",
                    f.dir.join("payload.bin").to_str().unwrap(),
                    &format!(":Earlier:File {n:02}")]);
        }
        f.hfs("hmkdir", &[":Target"]);
        f.hfs("hmkdir", &[":Empty"]);
        for name in ["Alpha", "Café", "Zulu"]
        {
            f.hfs("hcopy",
                  &["-m", f.dir.join("payload.bin").to_str().unwrap(), &format!(":Target:{name}")]);
        }
        f.hfs("humount", &[]);
        f
    }

    fn hfs(&self, tool: &str, args: &[&str])
    {
        let r=Command::new(self.root.join(".toolchains/hfsutils/rootfs/usr/bin").join(tool)).args(args).env("HOME",self.dir.join("hfs-state")).env("TZ","UTC").output().unwrap();
        assert!(r.status.success(), "{tool}: {}", String::from_utf8_lossy(&r.stderr));
    }

    fn run<R>(&self, run: impl FnOnce(&System<'_>) -> R) -> (R, mac_system::testing::DiskActivity)
    {
        with_file_disk(&self.disk, false, DiskFaults::default(), |services| {
            let system = unsafe { System::from_raw(services) }.unwrap();
            run(&system)
        }).unwrap()
    }

    fn run_with_gate<R>(&self, run: impl FnOnce(&System<'_>, mac_abi::TrapGate) -> R)
                        -> (R, mac_system::testing::DiskActivity)
    {
        with_file_disk(&self.disk, false, DiskFaults::default(), |services| {
            let system = unsafe { System::from_raw(services) }.unwrap();
            run(&system, services.trap)
        }).unwrap()
    }

    fn corrupt_first_leaf(&self, corrupt: impl FnOnce(&mut [u8], usize))
    {
        let mut b = fs::read(&self.disk).unwrap();
        let mdb = &b[1024..1536];
        let allocation = u32::from_be_bytes(mdb[20..24].try_into().unwrap()) as usize;
        let start = usize::from(u16::from_be_bytes(mdb[28..30].try_into().unwrap())) * 512;
        let extent = usize::from(u16::from_be_bytes(mdb[150..152].try_into().unwrap()));
        let catalog = start + extent * allocation;
        let size =
            usize::from(u16::from_be_bytes(b[catalog + 32..catalog + 34].try_into().unwrap()));
        let leaf = u32::from_be_bytes(b[catalog + 24..catalog + 28].try_into().unwrap()) as usize;
        corrupt(&mut b[catalog + leaf * size..catalog + (leaf + 1) * size], leaf);
        fs::write(&self.disk, b).unwrap();
    }
}
impl Drop for Fixture
{
    fn drop(&mut self)
    {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
type Entries =
    BTreeMap<mac_toolbox::Cnid, (mac_toolbox::files::CatalogInfo, Option<u64>, Option<u64>)>;
fn collect(system: &System<'_>, parent: mac_toolbox::Cnid) -> (Entries, usize, usize)
{
    let mut cursor = DirectoryCursor::default();
    let mut entries = BTreeMap::new();
    let mut pending = 0;
    for steps in 1..1024
    {
        match system.files()
                    .scan_directory(system.files().volume_at(0).unwrap().id, parent, &mut cursor)
                    .unwrap()
        {
            ScanStep::Entry { catalog,
                              data_allocated,
                              resource_allocated } =>
            {
                assert!(entries.insert(catalog.id, (catalog, data_allocated, resource_allocated))
                               .is_none());
            }
            ScanStep::Pending => pending += 1,
            ScanStep::Done =>
            {
                assert_eq!(system.files().scan_directory(system.files().volume_at(0).unwrap().id,
                                                         parent,
                                                         &mut cursor),
                           Ok(ScanStep::Done));
                return (entries, steps + 1, pending);
            }
        }
    }
    panic!("bounded test catalog failed to terminate");
}
#[test]
fn incremental_directory_entries_match_catalog_and_allocation_with_one_leaf_per_step()
{
    let fixture = Fixture::new();
    let (mut expected, _) =
        fixture.run(|system| {
                   let parent =
                       system.files()
                             .lookup(system.files().volume_at(0).unwrap().id, ROOT_ID, "Target")
                             .unwrap();
                   let mut entries = BTreeMap::new();
                   for index in 0..3
                   {
                       let catalog =
                           system.files()
                                 .child(system.files().volume_at(0).unwrap().id, parent.id, index)
                                 .unwrap();
                       let physical =
                           system.files()
                                 .allocation_info(system.files().volume_at(0).unwrap().id,
                                                  catalog.id)
                                 .unwrap();
                       entries.insert(catalog.id,
                                      (catalog, physical.data_bytes, physical.resource_bytes));
                   }
                   entries
               });
    let (_, baseline) =
        fixture.run(|system| {
                   system.files()
                         .lookup(system.files().volume_at(0).unwrap().id, ROOT_ID, "Target")
                         .unwrap()
               });
    let ((actual, steps, pending), activity) =
        fixture.run(|system| {
                   let parent =
                       system.files()
                             .lookup(system.files().volume_at(0).unwrap().id, ROOT_ID, "Target")
                             .unwrap();
                   collect(system, parent.id)
               });
    // Catalog contents persist, but mounted-volume handles belong to this
    // service incarnation and must not survive reconstruction.
    let current_volume = actual.values().next().unwrap().0.volume;
    for (catalog, _, _) in expected.values_mut()
    {
        assert_ne!(catalog.volume, current_volume);
        catalog.volume = current_volume;
    }
    assert_eq!(actual, expected);
    assert!(pending > 0, "unrelated leaves must yield progress");
    assert!(activity.reads - baseline.reads <= steps,
            "a step must read at most one512-byte fixture leaf: {} reads in {steps} steps",
            activity.reads - baseline.reads);
    assert_eq!(activity.writes, 0);
    assert_eq!(activity.flushes, 0);
    fixture.run(|system| {
               let empty = system.files()
                                 .lookup(system.files().volume_at(0).unwrap().id, ROOT_ID, "Empty")
                                 .unwrap();
               assert!(collect(system, empty.id).0.is_empty());
           });
}
#[test]
fn directory_scan_mutation_remount_and_binding_guards_prevent_mixed_totals()
{
    let fixture = Fixture::new();
    let ((saved, parent, old_volume), _) =
        fixture.run(|system| {
                   let files = system.files();
                   let parent =
                       files.lookup(system.files().volume_at(0).unwrap().id, ROOT_ID, "Target")
                            .unwrap()
                            .id;
                   let mut cursor = DirectoryCursor::default();
                   let _ = files.scan_directory(system.files().volume_at(0).unwrap().id,
                                                parent,
                                                &mut cursor)
                                .unwrap();
                   let unchanged = cursor;
                   assert_eq!(files.scan_directory(system.files().volume_at(0).unwrap().id,
                                                   ROOT_ID,
                                                   &mut cursor),
                              Err(Error::BadArgument));
                   assert_eq!(cursor, unchanged);
                   let _ = files.catalog(system.files().volume_at(0).unwrap().id, parent).unwrap(); // ordinary reads do not invalidate
                   files.scan_directory(system.files().volume_at(0).unwrap().id,
                                        parent,
                                        &mut cursor)
                        .unwrap();
                   files.new_folder_at(system.files().volume_at(0).unwrap().id,
                                       parent,
                                       "Added",
                                       WHEN)
                        .unwrap();
                   let before = cursor;
                   assert_eq!(files.scan_directory(system.files().volume_at(0).unwrap().id,
                                                   parent,
                                                   &mut cursor),
                              Err(Error::ScanStale));
                   assert_eq!(cursor, before);
                   assert_eq!(collect(system, parent).0.len(), 4);
                   let mut fresh = DirectoryCursor::default();
                   files.scan_directory(system.files().volume_at(0).unwrap().id,
                                        parent,
                                        &mut fresh)
                        .unwrap();
                   (fresh, parent, files.volume_at(0).unwrap().id)
               });
    fixture.run(|system| {
               let files = system.files();
               let new_volume = files.volume_at(0).unwrap().id;
               assert_ne!(new_volume, old_volume);
               let mut cursor = saved;
               assert_eq!(files.scan_directory(old_volume, parent, &mut cursor),
                          Err(Error::NotFound));
               assert_eq!(cursor, saved, "stale mount refusal changed the cursor");
               assert_eq!(files.scan_directory(new_volume, parent, &mut cursor),
                          Err(Error::BadArgument));
               assert_eq!(cursor, saved, "foreign cursor binding refusal changed the cursor");
               assert_eq!(collect(system, parent).0.len(), 4, "a fresh scan still works");
           });
}
#[test]
fn forged_scan_cursor_fields_are_rejected_through_public_native_client()
{
    let fixture = Fixture::new();
    fixture.run(|system| {
               use mac_abi::files::{FileScanCall, SCAN_DIRECTORY};
               let mut call = FileScanCall { operation: SCAN_DIRECTORY,
                                             volume: (system.files().volume_at(0).unwrap().id).0,
                                             directory: (ROOT_ID).0,
                                             ..Default::default() };
               system.client().file_scan(&mut call).unwrap();
               let good = call;
               for field in 0..6
               {
                   let mut bad = good;
                   match field
                   {
                       0 => bad.cursor.node = u32::MAX,
                       1 => bad.cursor.index = u32::MAX,
                       2 => bad.cursor.traversed = u32::MAX,
                       3 => bad.cursor.state = 3,
                       4 => bad.cursor.reserved = 1,
                       _ => bad.cursor.revision = 0
                   }
                   assert_eq!(system.client().file_scan(&mut bad),
                              Err(Error::BadArgument),
                              "field {field}");
               }
           });
}
#[test]
fn corrupted_catalog_cycles_and_offset_tables_fail_without_walking_the_chain()
{
    for cycle in [true, false]
    {
        let fixture = Fixture::new();
        fixture.corrupt_first_leaf(|leaf, index| {
                   if cycle
                   {
                       leaf[..4].copy_from_slice(&(index as u32).to_be_bytes());
                   }
                   else
                   {
                       leaf[10..12].copy_from_slice(&u16::MAX.to_be_bytes());
                   }
               });
        let (_, baseline) = fixture.run(|_| ());
        let (result, activity) =
            fixture.run(|system| {
                       system.files().scan_directory(system.files().volume_at(0).unwrap().id,
                                                     ROOT_ID,
                                                     &mut DirectoryCursor::default())
                   });
        assert_eq!(result, Err(Error::Io));
        assert!(activity.reads - baseline.reads <= 1);
    }
}

#[test]
fn malformed_physical_lengths_cannot_be_reported_as_completed_folder_totals()
{
    let fixture = Fixture::new();
    let mut bytes = fs::read(&fixture.disk).unwrap();
    let word = |bytes: &[u8], at: usize| {
        u16::from_be_bytes(bytes[at..at + 2].try_into().unwrap()) as usize
    };
    let long = |bytes: &[u8], at: usize| {
        u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize
    };
    let catalog =
        word(&bytes, 1024 + 28) * 512 + word(&bytes, 1024 + 150) * long(&bytes, 1024 + 20);
    let size = word(&bytes, catalog + 32);
    let mut node = long(&bytes, catalog + 24);
    let mut parent = 0;
    while node != 0 && parent == 0
    {
        let at = catalog + node * size;
        for index in 0..word(&bytes, at + 10)
        {
            let start = at + word(&bytes, at + size - (index + 1) * 2);
            let data = start + (usize::from(bytes[start]) + 1).next_multiple_of(2);
            if bytes[data] == 2
            {
                parent = long(&bytes, start + 2) as u32;
                // Logical data length is1700. One allocated byte is both too
                // small and not an allocation-block multiple.
                bytes[data + 30..data + 34].copy_from_slice(&1u32.to_be_bytes());
                break;
            }
        }
        node = long(&bytes, at);
    }
    assert_ne!(parent, 0);
    fs::write(&fixture.disk, bytes).unwrap();
    fixture.run(|system| {
               let mut cursor = DirectoryCursor::default();
               for _ in 0..256
               {
                   match system.files().scan_directory(system.files().volume_at(0).unwrap().id,
                                                       mac_toolbox::Cnid(parent),
                                                       &mut cursor)
                   {
                       Err(Error::Io) => return,
                       Ok(ScanStep::Pending) =>
                       {}
                       other => panic!("malformed allocation returned usable totals: {other:?}")
                   }
               }
               panic!("bounded scan failed to report malformed allocation");
           });
}

#[test]
fn native_and_legacy_incremental_entries_share_cursor_and_physical_metadata()
{
    let fixture = Fixture::new();
    fixture.run_with_gate(|system, gate| {
               use mac_abi::files::{FileScanCall, SCAN_DIRECTORY, SCAN_DONE, SCAN_ENTRY};
               let parent =
                   system.files()
                         .lookup(system.files().volume_at(0).unwrap().id, ROOT_ID, "Target")
                         .unwrap()
                         .id;
               let item = system.files()
                                .lookup(system.files().volume_at(0).unwrap().id, parent, "Alpha")
                                .unwrap();
               system.files()
                     .rename_at(system.files().volume_at(0).unwrap().id, item.id, "Café Ω", WHEN)
                     .unwrap();
               let mut native = FileScanCall { operation: SCAN_DIRECTORY,
                                               volume:
                                                   (system.files().volume_at(0).unwrap().id).0,
                                               directory: (parent).0,
                                               ..Default::default() };
               let mut legacy = native;
               let mut unicode_seen = false;
               let mut done = false;
               for _ in 0..1024
               {
                   system.client().file_scan(&mut native).unwrap();
                   let status = unsafe {
                       gate(mac_abi::traps::PSTRING_FILE,
                            core::ptr::from_mut(&mut legacy).cast(),
                            size_of::<FileScanCall>())
                   };
                   assert_eq!(status, 0);
                   assert_eq!(native.cursor, legacy.cursor);
                   assert_eq!(native.step, legacy.step);
                   assert_eq!(native.data_allocated, legacy.data_allocated);
                   assert_eq!(native.resource_allocated, legacy.resource_allocated);
                   if native.step == SCAN_ENTRY
                   {
                       let encoded = mac_formats::text::encode(native.entry.name()).unwrap();
                       assert_eq!(legacy.entry.name_length as usize, encoded.len() + 1);
                       assert_eq!(legacy.entry.name[0] as usize, encoded.len());
                       assert_eq!(&legacy.entry.name[1..encoded.len() + 1], encoded);
                       assert_eq!(native.entry.id, legacy.entry.id);
                       assert_eq!(native.entry.data_length, legacy.entry.data_length);
                       assert_eq!(native.entry.resource_length, legacy.entry.resource_length);
                       unicode_seen |= native.entry.name() == "Café Ω";
                   }
                   if native.step == SCAN_DONE
                   {
                       done = true;
                       break;
                   }
               }
               assert!(done);
               assert!(unicode_seen);
           });
}

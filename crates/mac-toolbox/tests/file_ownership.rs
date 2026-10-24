//! Native application ownership through the public SDK and real disk service.
//! Context selection/exit are trusted host scheduler fixtures, not ABI fields.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs,
          path::PathBuf,
          process::Command,
          sync::atomic::{AtomicUsize, Ordering}};

use mac_abi::files::*;
use mac_system::testing::{files::{close_owner, with_owner},
                          with_disk};
use mac_toolbox::{Error, System, abi::ACCESS_DENIED};
static NEXT: AtomicUsize = AtomicUsize::new(0);

fn watch_pool(system: &System<'_>, kind: [u8; 4]) -> mac_toolbox::inspection::Pool
{
    let snapshot = system.inspection().snapshot(mac_toolbox::inspection::Scope::Own).unwrap();
    let ledger = snapshot.ledger(0).unwrap();
    (0..ledger.len()).map(|index| ledger.at(index).unwrap())
                     .find(|pool| pool.kind == kind)
                     .expect("registered directory watch pool")
}

fn writable_disk(run: impl FnOnce(&System<'_>))
{
    writable_disk_with_gate(|system, _| run(system));
}

fn writable_disk_with_gate(run: impl FnOnce(&System<'_>, mac_abi::TrapGate))
{
    struct Temporary(PathBuf);
    impl Drop for Temporary
    {
        fn drop(&mut self)
        {
            let _ = fs::remove_file(&self.0);
        }
    }
    let path = Temporary(std::env::temp_dir().join(format!("watch-ledger-{}-{}.hfs",
                                                           std::process::id(),
                                                           NEXT.fetch_add(1, Ordering::Relaxed))));
    fs::write(&path.0, disk()).unwrap();
    mac_system::testing::with_file_disk(&path.0, false, Default::default(), |services| {
        run(&unsafe { System::from_raw(services) }.unwrap(), services.trap);
    }).unwrap();
}

#[test]
fn pending_watch_disposal_invalidates_immediately_but_reserves_its_slot_until_ack()
{
    use mac_system::testing::files::with_pending_watch_probe;
    writable_disk(|system| {
        let files = system.files();
        let volume = files.volume_at(0).unwrap().id;
        let watches: Vec<_> =
            (0..64).map(|_| files.watch_directory(volume, mac_toolbox::Cnid(ROOT_ID)).unwrap())
                   .collect();
        let mut iter = watches.into_iter();
        let held = iter.next().unwrap();
        let others: Vec<_> = iter.collect();
        let stale = held.id();
        with_pending_watch_probe(stale, || {
            held.close().unwrap();
            let mut call = FileCall { operation: POLL_WATCH,
                                      handle: stale,
                                      ..Default::default() };
            assert_eq!(system.client().file(&mut call, "", &mut []), Err(Error::NotFound));
            let pool = watch_pool(system, *b"FWCH");
            assert_eq!((pool.live, pool.pending), (63, 1));
            let error = files.watch_directory(volume, mac_toolbox::Cnid(ROOT_ID)).err().unwrap();
            assert_eq!(error, Error::NoMemory);
            assert!(error.to_string().contains("memory"));
            assert_eq!(watch_pool(system, *b"FWCH"),
                       pool,
                       "refused publication changed the ledger");
            for other in &others
            {
                assert_eq!(other.poll().unwrap(), None);
            }
        }).unwrap();
        assert_eq!(watch_pool(system, *b"FWCH").pending, 0);
        let replacement = files.watch_directory(volume, mac_toolbox::Cnid(ROOT_ID)).unwrap();
        assert_eq!(stale & 0xffff, replacement.id() & 0xffff);
        assert_ne!(stale, replacement.id());
        let mut call = FileCall { operation: POLL_WATCH,
                                  handle: stale,
                                  ..Default::default() };
        assert_eq!(system.client().file(&mut call, "", &mut []), Err(Error::NotFound));
        assert_eq!(replacement.poll().unwrap(), None);
        drop(others);
    });
}

#[test]
fn watch_ack_after_invalidation_completes_pending_or_later_quarantined_cleanup()
{
    use mac_system::testing::{files::with_pending_watch_probe, inspection as fixture};
    for unavailable in [false, true]
    {
        writable_disk(|system| {
            let files = system.files();
            let volume = files.volume_at(0).unwrap().id;
            let watch = files.watch_directory(volume, mac_toolbox::Cnid(ROOT_ID)).unwrap();
            let snapshot =
                system.inspection().snapshot(mac_toolbox::inspection::Scope::Own).unwrap();
            let owner = snapshot.at(0).unwrap().incarnation;
            with_pending_watch_probe(watch.id(), || {
                let walked = fixture::walk_registered(owner, unavailable, false);
                assert_eq!(walked.report.invalidated, 2);
                assert_eq!(walked.report.quarantined, if unavailable { 2 } else { 0 });
                if unavailable
                {
                    assert!(walked.failures[0].contains("no registered cleanup provider"));
                }
                assert_eq!(watch.poll(), Err(Error::NotFound));
                let row = snapshot.at(0).unwrap();
                assert!(!row.accepting);
                assert!(row.pending > 0);
                assert!(fixture::try_retire(owner).unwrap_err()
                                                  .to_string()
                                                  .contains("still pending"));
                let error =
                    files.watch_directory(volume, mac_toolbox::Cnid(ROOT_ID)).err().unwrap();
                assert_eq!(error, Error::ServiceBusy);
            }).unwrap();
            if unavailable
            {
                assert_eq!(snapshot.at(0).unwrap().pending,
                           2,
                           "ACK must not invent cleanup callbacks");
                let retry = fixture::walk_registered(owner, false, true);
                assert!(retry.failures.is_empty());
                assert_eq!(retry.report.released, 2);
            }
            let row = snapshot.at(0).unwrap();
            assert_eq!((row.live, row.pending, row.quarantined), (0, 0, 0));
            assert_eq!(row.deaths, row.births);
            drop(watch);
            fixture::try_retire(owner).unwrap();
        });
    }
}

#[test]
fn provider_fixture_unwind_finishes_the_real_pending_watch_guard()
{
    use mac_system::testing::files::with_pending_watch_probe;
    writable_disk(|system| {
        let files = system.files();
        let volume = files.volume_at(0).unwrap().id;
        let watch = files.watch_directory(volume, mac_toolbox::Cnid(ROOT_ID)).unwrap();
        let stale = watch.id();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_pending_watch_probe(stale, || {
                watch.close().unwrap();
                assert_eq!(watch_pool(system, *b"FWCH").pending, 1);
                panic!("provider fixture transport interrupted");
            }).unwrap();
        }));
        assert!(outcome.is_err());
        assert_eq!(watch_pool(system, *b"FWCH").pending, 0);
        let replacement = files.watch_directory(volume, mac_toolbox::Cnid(ROOT_ID)).unwrap();
        assert_eq!(stale & 0xffff, replacement.id() & 0xffff);
        assert_ne!(stale, replacement.id());
    });
}

fn disk() -> Vec<u8>
{
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap();
    let dir = root.join("results/system7").join(format!("file-ownership-{}-{}",
                                                        std::process::id(),
                                                        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(dir.join("hfs-state")).unwrap();
    let raw = dir.join("ownership.hfs");
    fs::File::create(&raw).unwrap().set_len(1024 * 1024).unwrap();
    let sample = dir.join("sample.bin");
    fs::write(&sample, mac_formats::macbinary::encode(&mac_formats::macbinary::MacFile {
        name: b"Caf\x8e".to_vec(), file_type: *b"TEXT", creator: *b"TEST", flags: 0,
        created: 0, modified: 0, data: (0..64).collect(), resources: b"RESOURCE".to_vec()
    }).unwrap()).unwrap();
    let hfs = |tool: &str, args: &[&str]| {
        let output = Command::new(root.join(".toolchains/hfsutils/rootfs/usr/bin").join(tool))
            .args(args).env("HOME", dir.join("hfs-state")).env("TZ", "UTC").output().unwrap();
        assert!(output.status.success(), "{tool}: {}", String::from_utf8_lossy(&output.stderr));
    };
    hfs("hformat", &["-l", "Ownership", raw.to_str().unwrap()]);
    hfs("hmount", &[raw.to_str().unwrap()]);
    hfs("hmkdir", &[":Folder"]);
    hfs("hcopy", &["-m", sample.to_str().unwrap(), ":Folder:"]);
    hfs("humount", &[]);
    fs::read(raw).unwrap()
}

fn with_system(run: impl FnOnce(&System<'_>))
{
    with_disk(&disk(), |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        run(&system);
    });
}

fn open(system: &System<'_>, owner: u64) -> u32
{
    with_owner(owner, || {
        let entry = system.files()
                          .lookup(system.files().volume_at(0).unwrap().id,
                                  mac_toolbox::Cnid(ROOT_ID),
                                  "Folder/Café")
                          .unwrap();
        let mut call = FileCall { operation: OPEN,
                                  volume: (system.files().volume_at(0).unwrap().id).0,
                                  cnid: (entry.id).0,
                                  fork: DATA_FORK,
                                  ..Default::default() };
        system.client().file(&mut call, "", &mut []).unwrap();
        call.handle
    })
}

#[test]
fn foreign_fork_handles_cannot_read_seek_tell_or_close()
{
    with_system(|system| {
        let handle = open(system, 11);
        let client = system.client();
        let mut seek = FileCall { operation: SEEK,
                                  handle,
                                  offset: 7,
                                  whence: SEEK_START,
                                  ..Default::default() };
        with_owner(11, || client.file(&mut seek, "", &mut [])).unwrap();
        for operation in [READ, SEEK, TELL, CLOSE]
        {
            let mut output = [0xa5; 16];
            let mut call = FileCall { operation,
                                      handle,
                                      offset: 2,
                                      position: 99,
                                      transferred: 99,
                                      ..Default::default() };
            assert_eq!(with_owner(22, || client.file(&mut call, "", &mut output)),
                       Err(Error::AccessDenied),
                       "operation {operation}");
            assert_eq!(output, [0xa5; 16], "foreign read must preserve the destination");
            assert_eq!(call.transferred, 0);
            assert_eq!(call.position, 99);
        }
        let mut tell = FileCall { operation: TELL,
                                  handle,
                                  ..Default::default() };
        with_owner(11, || client.file(&mut tell, "", &mut [])).unwrap();
        assert_eq!(tell.position, 7, "rejected foreign operations preserve the owner's mark");
        assert_eq!(close_owner(11), 1);
    });
}

#[test]
fn legacy_conversion_gate_enforces_the_same_trusted_owner()
{
    with_disk(&disk(), |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let handle = open(&system, 11);
        let mut call = FileCall { operation: TELL,
                                  handle,
                                  ..Default::default() };
        let invoke = |call: &mut FileCall| unsafe {
            (services.trap)(mac_abi::traps::PSTRING_FILE,
                            core::ptr::from_mut(call).cast(),
                            size_of::<FileCall>())
        };
        assert_eq!(with_owner(22, || invoke(&mut call)), i64::from(ACCESS_DENIED));
        assert_eq!(with_owner(11, || invoke(&mut call)), 0);
        assert_eq!(call.position, 0);
        assert_eq!(close_owner(11), 1);
    });
}

#[test]
fn exit_releases_only_its_handles_and_old_ids_do_not_alias_relaunch()
{
    with_system(|system| {
        let surviving = open(system, 22);
        let mut exited = Vec::new();
        for _ in 0..63
        {
            exited.push(open(system, 11));
        }
        let entry = system.files()
                          .lookup(system.files().volume_at(0).unwrap().id,
                                  mac_toolbox::Cnid(ROOT_ID),
                                  "Folder/Café")
                          .unwrap();
        let mut call = FileCall { operation: OPEN,
                                  volume: (system.files().volume_at(0).unwrap().id).0,
                                  cnid: (entry.id).0,
                                  fork: DATA_FORK,
                                  ..Default::default() };
        assert_eq!(with_owner(33, || system.client().file(&mut call, "", &mut [])),
                   Err(Error::NoMemory));
        assert_eq!(close_owner(11), 63);
        assert_eq!(close_owner(11), 0, "exit cleanup is idempotent");
        let replacement = open(system, 33);
        assert!(!exited.contains(&replacement));
        for handle in exited
        {
            let mut stale = FileCall { operation: TELL,
                                       handle,
                                       ..Default::default() };
            assert_eq!(with_owner(11, || system.client().file(&mut stale, "", &mut [])),
                       Err(Error::NotFound));
            assert_eq!(with_owner(33, || system.client().file(&mut stale, "", &mut [])),
                       Err(Error::NotFound));
        }
        let mut own = FileCall { operation: READ,
                                 handle: surviving,
                                 ..Default::default() };
        let mut output = [0; 8];
        with_owner(22, || system.client().file(&mut own, "", &mut output)).unwrap();
        assert_eq!(own.transferred, 8, "another app's fork remains open across exit");
        assert_eq!(own.position, 8);
        assert_eq!(close_owner(33), 1);
        assert_eq!(close_owner(22), 1);
    });
}

#[test]
fn separate_apps_have_independent_marks_and_foreign_sdk_values_are_rejected()
{
    with_system(|system| {
        let entry = system.files()
                          .lookup(system.files().volume_at(0).unwrap().id,
                                  mac_toolbox::Cnid(ROOT_ID),
                                  "Folder/Café")
                          .unwrap();
        let mut first = with_owner(11, || {
                            system.files().open(system.files().volume_at(0).unwrap().id,
                                                entry.id,
                                                mac_toolbox::files::Fork::Data)
                        }).unwrap();
        let mut second = with_owner(22, || {
                             system.files().open(system.files().volume_at(0).unwrap().id,
                                                 entry.id,
                                                 mac_toolbox::files::Fork::Data)
                         }).unwrap();
        let mut bytes = [0; 8];
        assert_eq!(with_owner(11, || first.read(&mut bytes)), Ok(8));
        assert_eq!(with_owner(22, || second.position()), Ok(0));
        assert_eq!(with_owner(22, || second.read(&mut bytes)), Ok(8));
        assert_eq!(with_owner(22, || first.position()), Err(Error::AccessDenied));
        assert_eq!(with_owner(11, || first.position()), Ok(8));
        assert_eq!(with_owner(22, || first.close()), Err(Error::AccessDenied));
        assert_eq!(with_owner(11, || first.position()), Ok(8), "failed close retains the owner");
        with_owner(11, || first.close()).unwrap();
        with_owner(11, || first.close()).unwrap();
        with_owner(22, || second.close()).unwrap();
        assert_eq!(close_owner(11), 0);
        assert_eq!(close_owner(22), 0);
    });
}

#[test]
fn directory_watches_are_owned_coalesced_scoped_and_reject_disposed_reused_ids()
{
    struct Temporary(PathBuf);
    impl Drop for Temporary
    {
        fn drop(&mut self)
        {
            let _ = fs::remove_file(&self.0);
        }
    }
    let path =
        Temporary(std::env::temp_dir().join(format!("watch-ownership-{}.hfs", std::process::id())));
    fs::write(&path.0, disk()).unwrap();
    mac_system::testing::with_file_disk(&path.0, false, Default::default(), |services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let files = system.files();
        let volume = files.volume_at(0).unwrap().id;
        let folder = files.lookup(volume, mac_toolbox::Cnid(ROOT_ID), "Folder").unwrap();
        let root = files.watch_directory(volume, mac_toolbox::Cnid(ROOT_ID)).unwrap();
        let child = files.watch_directory(volume, folder.id).unwrap();
        assert_eq!(root.poll().unwrap(), None);
        assert_eq!(child.poll().unwrap(), None);
        let id = root.id();
        for operation in [POLL_WATCH, DISPOSE_WATCH]
        {
            let mut call = FileCall { operation,
                                      handle: id,
                                      ..Default::default() };
            assert_eq!(with_owner(700, || system.client().file(&mut call, "", &mut [])),
                       Err(Error::AccessDenied));
        }
        files.new_folder(volume, mac_toolbox::Cnid(ROOT_ID), "First").unwrap();
        files.new_folder(volume, mac_toolbox::Cnid(ROOT_ID), "Second").unwrap();
        assert!(root.poll().unwrap().is_some());
        assert_eq!(root.poll().unwrap(), None, "changes coalesce and are consumed once");
        assert_eq!(child.poll().unwrap(), None, "unrelated folder must remain clean");
        root.close().unwrap();
        let replacement = files.watch_directory(volume, mac_toolbox::Cnid(ROOT_ID)).unwrap();
        assert_eq!(id & 0xffff, replacement.id() & 0xffff, "force slot reuse");
        assert_ne!(id, replacement.id());
        for operation in [POLL_WATCH, DISPOSE_WATCH]
        {
            let mut call = FileCall { operation,
                                      handle: id,
                                      ..Default::default() };
            assert_eq!(system.client().file(&mut call, "", &mut []), Err(Error::NotFound));
        }
        assert_eq!(replacement.poll().unwrap(), None);
        drop(child);
        drop(replacement);
    }).unwrap();
}

#[test]
fn reconstructed_file_manager_rejects_old_volume_fork_and_write_ids_through_both_gates()
{
    // Both service sessions use the same trusted caller. Rejection is identity
    // validation, not a different application's ownership check.
    let mut old = None;
    for generation in 0..2
    {
        writable_disk_with_gate(|system, gate| {
            let volume = system.files().volume_at(0).unwrap().id.0;
            let handle = open(system, 1);
            let mut write = DataWriteCall { operation: BEGIN_DATA_WRITE,
                                            volume,
                                            parent: ROOT_ID,
                                            length: 2,
                                            file_type: *b"TEXT",
                                            creator: *b"TEST",
                                            ..Default::default() };
            system.client().data_write(&mut write, "Staged", &[]).unwrap();
            if generation == 0
            {
                old = Some((volume, handle, write.handle));
            }
            else
            {
                let (old_volume, old_fork, old_write) = old.unwrap();
                for (stale, fresh) in
                    [(old_volume, volume), (old_fork, handle), (old_write, write.handle)]
                {
                    assert_eq!(stale & 0xffff, fresh & 0xffff, "same treaty slot reused");
                    assert_ne!(stale, fresh, "reconstruction must preserve generation history");
                }
                for operation in [VOLUME_INFO, TELL, CLOSE]
                {
                    let mut call = FileCall { operation,
                                              volume: old_volume,
                                              handle: old_fork,
                                              ..Default::default() };
                    let error = system.client().file(&mut call, "", &mut []).unwrap_err();
                    assert_eq!(error, Error::NotFound);
                    assert!(error.to_string().contains("found"));
                    assert_eq!(unsafe {
                                   gate(mac_abi::traps::PSTRING_FILE,
                                        core::ptr::from_mut(&mut call).cast(),
                                        size_of::<FileCall>())
                               },
                               i64::from(mac_abi::NOT_FOUND));
                }
                for operation in [APPEND_DATA_WRITE, ABORT_DATA_WRITE, COMMIT_DATA_WRITE]
                {
                    let mut call = DataWriteCall { operation,
                                                   handle: old_write,
                                                   ..Default::default() };
                    assert_eq!(system.client().data_write(&mut call, "", &[]),
                               Err(Error::NotFound));
                    assert_eq!(unsafe {
                                   gate(mac_abi::traps::PSTRING_FILE,
                                        core::ptr::from_mut(&mut call).cast(),
                                        size_of::<DataWriteCall>())
                               },
                               i64::from(mac_abi::NOT_FOUND));
                }
                let mut tell = FileCall { operation: TELL,
                                          handle,
                                          ..Default::default() };
                system.client().file(&mut tell, "", &mut []).unwrap();
                assert_eq!(tell.position, 0, "stale requests changed the current fork mark");
                assert_eq!(system.files().volume_at(0).unwrap().id.0, volume);
                write.operation = APPEND_DATA_WRITE;
                system.client().data_write(&mut write, "", b"ok").unwrap();
            }
            write.operation = ABORT_DATA_WRITE;
            system.client().data_write(&mut write, "", &[]).unwrap();
            assert_eq!(close_owner(1), 1);
        });
    }
}

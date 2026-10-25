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
                for (stale, fresh) in [(old_volume, volume), (old_fork, handle)]
                {
                    assert_eq!(stale & 0xffff, fresh & 0xffff, "same treaty slot reused");
                    assert_ne!(stale, fresh, "reconstruction must preserve generation history");
                }
                assert!(old_write > u64::from(u32::MAX) && write.handle > u64::from(u32::MAX));
                assert_ne!(old_write, write.handle);
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

#[test]
fn fork_ledger_walk_invalidates_both_forks_and_preserves_other_app_marks()
{
    use mac_system::testing::inspection as fixture;
    with_system(|system| {
        let volume = system.files().volume_at(0).unwrap().id;
        let item =
            system.files().lookup(volume, mac_toolbox::Cnid(ROOT_ID), "Folder/Café").unwrap();
        let dead = fixture::add("Fork owner", 0);
        let survivor = with_owner(9001, || {
                           system.files().open(volume, item.id, mac_toolbox::files::Fork::Data)
                       }).unwrap();
        let data = open(system, dead);
        let mut resource = FileCall { operation: OPEN,
                                      volume: volume.0,
                                      cnid: item.id.0,
                                      fork: RESOURCE_FORK,
                                      ..Default::default() };
        with_owner(dead, || system.client().file(&mut resource, "", &mut [])).unwrap();
        let ids = [data, resource.handle];
        with_owner(dead, || {
            assert_eq!(watch_pool(system, *b"FFRK").live, 2);
            assert_eq!(watch_pool(system, *b"FFST").live, 1);
        });
        let walked = fixture::walk_registered(dead, false, false);
        assert!(walked.failures.is_empty(), "{:?}", walked.failures);
        assert_eq!(walked.report.invalidated, 3);
        with_owner(dead, || {
            for id in ids
            {
                for operation in [READ, TELL, SEEK, CLOSE]
                {
                    let mut call = FileCall { operation,
                                              handle: id,
                                              ..Default::default() };
                    let error = system.client().file(&mut call, "", &mut []).unwrap_err();
                    assert_eq!(error, Error::NotFound);
                    assert!(error.to_string().contains("found"));
                }
            }
            assert_eq!(watch_pool(system, *b"FFRK").pending, 0);
            assert_eq!(watch_pool(system, *b"FFST").pending, 0);
        });
        assert_eq!(with_owner(9001, || survivor.position()), Ok(0));
        fixture::try_retire(dead).unwrap();
        with_owner(9001, || drop(survivor));
    });
}

#[test]
fn refused_fork_release_retains_payloads_until_typed_cleanup_retry()
{
    use mac_system::testing::inspection as fixture;
    with_system(|system| {
        let volume = system.files().volume_at(0).unwrap().id;
        let item =
            system.files().lookup(volume, mac_toolbox::Cnid(ROOT_ID), "Folder/Café").unwrap();
        let owner = fixture::add("Quarantined fork owner", 0);
        let mark = open(system, owner);
        let failed = fixture::walk_registered(owner, true, false);
        assert_eq!(failed.report.quarantined, 2);
        assert!(!failed.failures.is_empty());
        assert!(failed.failures
                      .iter()
                      .all(|message| message.contains("FFRK") || message.contains("FFST")),
                "{:?}",
                failed.failures);
        let refusal = fixture::try_retire(owner).unwrap_err();
        assert!(refusal.to_string().contains("pending") || refusal.to_string().contains("live"));
        let mut stale = FileCall { operation: TELL,
                                   handle: mark,
                                   ..Default::default() };
        assert_eq!(with_owner(owner, || system.client().file(&mut stale, "", &mut [])),
                   Err(Error::NotFound));
        let retry = fixture::walk_registered(owner, false, true);
        assert!(retry.failures.is_empty(), "{:?}", retry.failures);
        with_owner(owner, || {
            assert_eq!(watch_pool(system, *b"FFRK").pending, 0);
            assert_eq!(watch_pool(system, *b"FFST").pending, 0);
        });
        fixture::try_retire(owner).unwrap();
        let replacement =
            system.files().open(volume, item.id, mac_toolbox::files::Fork::Data).unwrap();
        assert_eq!(replacement.position(), Ok(0));
    });
}

#[test]
fn staged_writes_are_walked_without_disturbing_another_apps_draft()
{
    use mac_system::testing::inspection as fixture;
    writable_disk(|system| {
        let volume = system.files().volume_at(0).unwrap().id;
        let owner = fixture::add("Draft owner", 0);
        let mut draft = DataWriteCall { operation: BEGIN_DATA_WRITE,
                                        volume: volume.0,
                                        parent: ROOT_ID,
                                        length: 3,
                                        file_type: *b"TEXT",
                                        creator: *b"TEST",
                                        ..Default::default() };
        with_owner(owner, || system.client().data_write(&mut draft, "Draft", &[])).unwrap();
        let mut other = DataWriteCall { operation: BEGIN_DATA_WRITE,
                                        volume: volume.0,
                                        parent: ROOT_ID,
                                        length: 3,
                                        file_type: *b"TEXT",
                                        creator: *b"TEST",
                                        ..Default::default() };
        with_owner(5001, || system.client().data_write(&mut other, "Other draft", &[])).unwrap();
        with_owner(owner, || {
            assert_eq!(watch_pool(system, *b"FDWR").live, 1);
            assert_eq!(watch_pool(system, *b"FDST").live, 1);
        });
        let failed = fixture::walk_registered(owner, true, false);
        assert_eq!(failed.report.quarantined, 2);
        assert!(failed.failures
                      .iter()
                      .all(|error| error.contains("FDWR") || error.contains("FDST")),
                "{:?}",
                failed.failures);
        for operation in [APPEND_DATA_WRITE, COMMIT_DATA_WRITE, ABORT_DATA_WRITE]
        {
            let mut call = DataWriteCall { operation,
                                           handle: draft.handle,
                                           ..Default::default() };
            let error =
                with_owner(owner, || system.client().data_write(&mut call, "", &[])).unwrap_err();
            assert_eq!(error, Error::NotFound);
            assert!(error.to_string().contains("found"));
        }
        let mut rejected = DataWriteCall { operation: BEGIN_DATA_WRITE,
                                           volume: volume.0,
                                           parent: ROOT_ID,
                                           length: 0,
                                           ..Default::default() };
        let error = with_owner(owner, || {
                        system.client().data_write(&mut rejected, "After death", &[])
                    }).unwrap_err();
        assert_eq!(error, Error::ServiceBusy);
        assert!(error.to_string().contains("busy"));
        let retry = fixture::walk_registered(owner, false, true);
        assert!(retry.failures.is_empty(), "{:?}", retry.failures);
        fixture::try_retire(owner).unwrap();
        other.operation = APPEND_DATA_WRITE;
        with_owner(5001, || system.client().data_write(&mut other, "", b"yes")).unwrap();
        other.operation = ABORT_DATA_WRITE;
        with_owner(5001, || system.client().data_write(&mut other, "", &[])).unwrap();
        assert!(system.files().lookup(volume, mac_toolbox::Cnid(ROOT_ID), "Draft").is_err());
    });
}


#[test]
fn scoped_fork_outlives_child_first_owner_walk_and_closes_only_after_its_real_root()
{
    use mac_system::testing::{files::with_operation, inspection as fixture};
    use mac_toolbox::{files::Fork, inspection::Scope};
    with_system(|system| {
        let owner = fixture::add("Scoped I/O owner", 0);
        let other = fixture::add("Independent I/O owner", 0);
        with_owner(owner, || {
            let files = system.files();
            let volume = files.volume_at(0).unwrap().id;
            let entry = files.lookup(volume, mac_toolbox::Cnid(ROOT_ID), "Folder/Café").unwrap();
            // Prepare fork/backing pools before root creation, forcing the
            // real walk to reach its child before the pending operation root.
            files.open(volume, entry.id, Fork::Data).unwrap().close().unwrap();
            with_operation(|operation| {
                let handle = operation.open(volume.0, entry.id.0, DATA_FORK).unwrap();
                let own = system.inspection().snapshot(Scope::Own).unwrap().ledger(0).unwrap();
                assert_eq!(own.domain().scoped_children, 1);
                let fork = (0..own.len()).map(|at| own.at(at).unwrap())
                                         .find(|pool| pool.kind == *b"FFRK")
                                         .unwrap();
                assert_eq!((fork.live, fork.scoped_children), (1, 1));
                let mut read = FileCall { operation: READ,
                                          handle,
                                          ..Default::default() };
                let mut bytes = [0; 8];
                let refused = system.client().file(&mut read, "", &mut bytes).unwrap_err();
                assert_eq!(refused, Error::AccessDenied);
                assert!(refused.to_string().contains("protected"));
                assert_eq!(operation.read(handle, &mut bytes).unwrap(), 8);
                assert_eq!(bytes, [0, 1, 2, 3, 4, 5, 6, 7]);
                let walk = fixture::walk_registered(owner, false, false);
                assert!(walk.failures.is_empty(), "{:?}", walk.failures);
                let stopped =
                    system.inspection().snapshot(Scope::Own).unwrap().ledger(0).unwrap().domain();
                assert!(!stopped.accepting);
                assert_eq!((stopped.live,
                            stopped.pending,
                            stopped.quarantined,
                            stopped.scoped_children),
                           (0, 2, 0, 1));
                let error = fixture::try_retire(owner).unwrap_err();
                assert!(matches!(error,
                                 fixture::RetirementError::PendingResources { pending: 2,
                                                                              .. }));
                assert!(error.to_string().contains("still pending"));
                operation.validate().unwrap();
                assert_eq!(operation.read(handle, &mut bytes).unwrap(), 8);
                assert_eq!(bytes, [8, 9, 10, 11, 12, 13, 14, 15]);
                assert_eq!(system.client().file(&mut read, "", &mut bytes),
                           Err(Error::NotFound),
                           "ordinary stale ID was resurrected");
                with_owner(other, || {
                    let mut independent =
                        system.files().open(volume, entry.id, Fork::Data).unwrap();
                    assert_eq!(independent.read(&mut bytes), Ok(8));
                    assert_eq!(bytes, [0, 1, 2, 3, 4, 5, 6, 7]);
                    independent.close().unwrap();
                    assert_eq!(system.client().file(&mut read, "", &mut bytes),
                               Err(Error::NotFound));
                });
                operation.finish();
                let closed = operation.validate().unwrap_err();
                assert!(closed.to_string().contains("resource scope is closed"));
                assert_eq!(operation.open(volume.0, entry.id.0, DATA_FORK),
                           Err(mac_abi::SERVICE_BUSY));
                let done =
                    system.inspection().snapshot(Scope::Own).unwrap().ledger(0).unwrap().domain();
                assert_eq!((done.live, done.pending, done.scoped_children, done.completion_faults),
                           (0, 0, 0, 0));
                assert_eq!(system.client().file(&mut read, "", &mut bytes), Err(Error::NotFound));
            }).unwrap();
            fixture::try_retire(owner).unwrap();
        });
        with_owner(other, || {
            assert!(fixture::walk_registered(other, false, false).failures.is_empty());
            fixture::try_retire(other).unwrap();
        });
    });
}

#[test]
fn native_data_writes_grow_past_eight_and_keep_slot_reuse_and_owner_checks()
{
    writable_disk(|system| {
        let volume = system.files().volume_at(0).unwrap().id;
        let mut held = Vec::new();
        for at in 0..20
        {
            let mut call = DataWriteCall { operation: BEGIN_DATA_WRITE,
                                           volume: volume.0,
                                           parent: ROOT_ID,
                                           length: 0,
                                           ..Default::default() };
            system.client().data_write(&mut call, &format!("Draft {at}"), &[]).unwrap();
            assert!(call.handle > u64::from(u32::MAX));
            held.push(call.handle);
        }
        let initial = watch_pool(system, *b"FDWR");
        assert_eq!(initial.live, 20);
        assert_eq!(initial.capacity, mac_toolbox::inspection::CapacityPolicy::GrowableUntilMemory);
        let stale = held.remove(0);
        let mut call = DataWriteCall { operation: ABORT_DATA_WRITE,
                                       handle: stale,
                                       ..Default::default() };
        let error =
            with_owner(2002, || system.client().data_write(&mut call, "", &[])).unwrap_err();
        assert_eq!(error, Error::AccessDenied);
        assert!(error.to_string().contains("protected"));
        system.client().data_write(&mut call, "", &[]).unwrap();
        let mut fresh = DataWriteCall { operation: BEGIN_DATA_WRITE,
                                        volume: volume.0,
                                        parent: ROOT_ID,
                                        length: 0,
                                        ..Default::default() };
        system.client().data_write(&mut fresh, "Replacement draft", &[]).unwrap();
        assert_ne!(fresh.handle, stale);
        assert_eq!(watch_pool(system, *b"FDWR").slots,
                   initial.slots,
                   "replacement must reuse acknowledged storage");
        let error = system.client().data_write(&mut call, "", &[]).unwrap_err();
        assert_eq!(error, Error::NotFound);
        assert!(error.to_string().contains("found"));
        for handle in held.into_iter().chain([fresh.handle])
        {
            system.client()
                  .data_write(&mut DataWriteCall { operation: ABORT_DATA_WRITE,
                                                   handle,
                                                   ..Default::default() },
                              "",
                              &[])
                  .unwrap();
        }
        let pool = watch_pool(system, *b"FDWR");
        assert_eq!((pool.live, pool.pending), (0, 0));
    });
}

//! Streamed storage cleanup through the public SDK and the registered walk.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs,
          path::PathBuf,
          sync::atomic::{AtomicUsize, Ordering}};

use mac_abi::files::*;
use mac_system::testing::{files::{pump_stream_cleanup, with_owner},
                          inspection as fixture};
use mac_toolbox::{Error, System, inspection::Scope};
static NEXT: AtomicUsize = AtomicUsize::new(0);
fn disk(run: impl FnOnce(&System<'_>))
{
    struct Disk(PathBuf);
    impl Drop for Disk
    {
        fn drop(&mut self)
        {
            let _ = fs::remove_file(&self.0);
        }
    }
    let disk = Disk(std::env::temp_dir().join(format!("stream-ledger-{}-{}.hfsb",
                                                      std::process::id(),
                                                      NEXT.fetch_add(1, Ordering::Relaxed))));
    let mut image = hfs_b::Image::create(&disk.0, 16 * 1024 * 1024).unwrap();
    let partition = image.partition();
    mac_hfs_b::format(&mut image, partition, mac_hfs_b::FormatOptions {
        name:"Streams", created:mac_hfs_b::time::Timestamp(4_600_000_000_000_000),uuid:[39;16]
    },Default::default()).unwrap();
    drop(image);
    mac_system::testing::with_file_disk(&disk.0, false, Default::default(), |services| {
        run(&unsafe { System::from_raw(services) }.unwrap());
    }).unwrap();
}
fn begin(system: &System<'_>, name: &str) -> ForkWriteCall
{
    let mut call = ForkWriteCall { operation: BEGIN_FORK_WRITE,
                                   volume: system.files().volume_at(0).unwrap().id.0,
                                   parent: ROOT_ID,
                                   data_length: 3,
                                   when: 4_600_000_000_000_000,
                                   created: 4_600_000_000_000_000,
                                   modified: 4_600_000_000_000_000,
                                   ..Default::default() };
    system.client().fork_write(&mut call, name, "", &[], &mut []).unwrap();
    call
}
fn invoke(system: &System<'_>, handle: u64, operation: u32) -> Result<(), Error>
{
    let mut call = ForkWriteCall { operation,
                                   handle,
                                   ..Default::default() };
    system.client().fork_write(&mut call, "", "", &[], &mut [])
}
fn pool(system: &System<'_>) -> mac_toolbox::inspection::Pool
{
    let snapshot = system.inspection().snapshot(Scope::Own).unwrap();
    let ledger = snapshot.ledger(0).unwrap();
    (0..ledger.len()).map(|i| ledger.at(i).unwrap()).find(|p| p.kind == *b"FSWR").unwrap()
}
#[test]
fn abandon_invalidates_before_rollback_and_reused_slots_reject_the_old_generation()
{
    disk(|system| {
        let first = begin(system, "Abandoned");
        for operation in [APPEND_FORK_WRITE, READ_FORK_WRITE, FINISH_FORK_WRITE, ABORT_FORK_WRITE,
                          ABANDON_FORK_WRITE]
        {
            let error = with_owner(2002, || invoke(system, first.handle, operation)).unwrap_err();
            assert_eq!(error, Error::AccessDenied);
            assert!(error.to_string().contains("protected"));
        }
        invoke(system, first.handle, ABANDON_FORK_WRITE).unwrap();
        assert_eq!((pool(system).live, pool(system).pending), (0, 1));
        for operation in [READ_FORK_WRITE, FINISH_FORK_WRITE, ABORT_FORK_WRITE, ABANDON_FORK_WRITE]
        {
            let error = invoke(system, first.handle, operation).unwrap_err();
            assert_eq!(error, Error::NotFound);
            assert!(error.to_string().contains("found"));
        }
        let mut other = begin(system, "Survivor");
        assert!(first.handle > u64::from(u32::MAX) && other.handle > u64::from(u32::MAX));
        assert_eq!((pool(system).slots, pool(system).pending), (2, 1), "pending slot was reused");
        for _ in 0..100
        {
            pump_stream_cleanup();
            if pool(system).pending == 0
            {
                break;
            }
        }
        assert_eq!(pool(system).pending, 0);
        let fresh = begin(system, "Replacement");
        assert_eq!(pool(system).slots, 2, "replacement must reuse acknowledged storage");
        assert_eq!(pool(system).births, 3);
        assert_ne!(first.handle, fresh.handle);
        assert_eq!(invoke(system, first.handle, ABORT_FORK_WRITE), Err(Error::NotFound));
        other.operation = APPEND_FORK_WRITE;
        system.client().fork_write(&mut other, "", "", b"yes", &mut []).unwrap();
        other.operation = FINISH_FORK_WRITE;
        for _ in 0..100
        {
            system.client().fork_write(&mut other, "", "", &[], &mut []).unwrap();
            if other.completed != 0
            {
                break;
            }
        }
        assert_eq!(other.completed, 1);
        assert_eq!(system.files()
                         .lookup(system.files().volume_at(0).unwrap().id,
                                 mac_toolbox::Cnid(ROOT_ID),
                                 "Survivor")
                         .unwrap()
                         .data_length,
                   3);
        invoke(system, fresh.handle, ABANDON_FORK_WRITE).unwrap();
        for _ in 0..100
        {
            pump_stream_cleanup();
            if pool(system).pending == 0
            {
                break;
            }
        }
        assert_eq!(pool(system).pending, 0);
    });
}
#[test]
fn owner_walk_retains_provisional_storage_and_backing_until_the_real_rollback_finishes()
{
    for unavailable in [false, true]
    {
        disk(|system| {
            let owner = fixture::add("Stream owner", 0);
            let mark = with_owner(owner, || begin(system, "Exiting draft"));
            let walked = fixture::walk_registered(owner, unavailable, false);
            assert_eq!(walked.report.invalidated, 2);
            if unavailable
            {
                assert_eq!(walked.report.quarantined, 2);
                assert!(walked.failures.iter().all(|s| s.contains("FSWR") || s.contains("FSST")),
                        "{:?}",
                        walked.failures);
            }
            else
            {
                assert!(walked.failures.is_empty());
                assert_eq!(walked.report.deferred, 1,
                           "empty value storage can be freed while the detached payload charge \
                            stays pinned");
            }
            let error = fixture::try_retire(owner).unwrap_err();
            assert!(error.to_string().contains("pending"));
            assert_eq!(with_owner(owner, || invoke(system, mark.handle, ABORT_FORK_WRITE)),
                       Err(Error::NotFound));
            if unavailable
            {
                pump_stream_cleanup();
                assert!(fixture::try_retire(owner).is_err(),
                        "no dispatcher means no rollback acknowledgement");
                let retry = fixture::walk_registered(owner, false, true);
                assert!(retry.failures.is_empty(), "{:?}", retry.failures);
            }
            for _ in 0..100
            {
                pump_stream_cleanup();
                if fixture::try_retire(owner).is_ok()
                {
                    return;
                }
            }
            panic!("bounded streamed rollback failed to release the retained process");
        });
    }
}

#[test]
fn sdk_writer_drop_schedules_real_rollback_and_allows_the_name_to_be_used_again()
{
    disk(|system| {
        let files = system.files();
        let volume = files.volume_at(0).unwrap().id;
        let mut catalog = mac_toolbox::files::CatalogInfo::EMPTY;
        catalog.created = mac_toolbox::Timestamp::from_mac_microseconds(4_600_000_000_000_000);
        catalog.modified = catalog.created;
        let spec = || {
            mac_toolbox::files::ForkWriteSpec {
            volume, parent: mac_toolbox::Cnid(ROOT_ID), name: "Dropped SDK writer",
            replace: None, data_length: 3, resource_length: 0,
            metadata: mac_toolbox::files::ForkMetadata::from_catalog(&catalog, "")
        }
        };
        let mut writer = files.begin_fork_write(spec()).unwrap();
        writer.write_chunk(mac_toolbox::files::Fork::Data, b"old").unwrap();
        assert_eq!((pool(system).live, pool(system).pending), (1, 0));
        drop(writer);
        assert_eq!((pool(system).live, pool(system).pending), (0, 1));
        let error = files.begin_fork_write(spec()).err().unwrap();
        assert_eq!(error, Error::ServiceBusy);
        assert!(error.to_string().contains("busy"));
        for _ in 0..100
        {
            pump_stream_cleanup();
            if pool(system).pending == 0
            {
                break;
            }
        }
        assert_eq!(pool(system).pending, 0);
        let mut retry = files.begin_fork_write(spec()).unwrap();
        retry.write_chunk(mac_toolbox::files::Fork::Data, b"new").unwrap();
        let mut committed = None;
        for _ in 0..100
        {
            committed = retry.commit_step().unwrap();
            if committed.is_some()
            {
                break;
            }
        }
        let entry = committed.expect("bounded SDK retry must publish");
        let mut fork = files.open(volume, entry.id, mac_toolbox::files::Fork::Data).unwrap();
        let mut bytes = [0; 3];
        assert_eq!(fork.read(&mut bytes).unwrap(), 3);
        assert_eq!(&bytes, b"new");
    });
}

#[test]
fn native_streams_grow_past_the_old_eight_object_projection()
{
    disk(|system| {
        let mut held = Vec::new();
        for at in 0..20
        {
            held.push(begin(system, &format!("Stream {at}")));
            assert!(held.last().unwrap().handle > u64::from(u32::MAX));
        }
        assert_eq!(pool(system).live, 20);
        assert_eq!(pool(system).capacity,
                   mac_toolbox::inspection::CapacityPolicy::GrowableUntilMemory);
        let storage_slots = pool(system).slots;
        let stale = held.remove(0).handle;
        invoke(system, stale, ABANDON_FORK_WRITE).unwrap();
        for _ in 0..100
        {
            pump_stream_cleanup();
            if pool(system).pending == 0
            {
                break;
            }
        }
        let replacement = begin(system, "Stream replacement");
        assert_ne!(replacement.handle, stale);
        assert_eq!(pool(system).slots, storage_slots, "freed slot was not reused");
        let error = invoke(system, stale, ABORT_FORK_WRITE).unwrap_err();
        assert_eq!(error, Error::NotFound);
        assert!(error.to_string().contains("found"));
        for call in held.into_iter().chain([replacement])
        {
            invoke(system, call.handle, ABANDON_FORK_WRITE).unwrap();
        }
        for _ in 0..100
        {
            pump_stream_cleanup();
            if pool(system).pending == 0
            {
                break;
            }
        }
        assert_eq!((pool(system).live, pool(system).pending), (0, 0));
    });
}

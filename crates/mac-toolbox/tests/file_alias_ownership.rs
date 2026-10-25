//! Shared catalog pins through the SDK, real storage and owner-walk cleanup.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs,
          path::PathBuf,
          sync::atomic::{AtomicUsize, Ordering}};

use mac_abi::files::*;
use mac_system::testing::files::with_owner;
use mac_toolbox::{Error, System};
static NEXT: AtomicUsize = AtomicUsize::new(0);
static BOOT: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn disk(run: impl FnOnce(&System<'_>))
{
    // These fixtures temporarily install different managers under the one
    // authentic System-domain bootstrap ledger. Their lifetimes cannot overlap.
    let _boot = BOOT.lock().unwrap_or_else(|error| error.into_inner());
    struct Disk(PathBuf);
    impl Drop for Disk
    {
        fn drop(&mut self)
        {
            let _ = fs::remove_file(&self.0);
        }
    }
    let disk = Disk(std::env::temp_dir().join(format!("alias-ledger-{}-{}.hfsb",
                                                      std::process::id(),
                                                      NEXT.fetch_add(1, Ordering::Relaxed))));
    let mut image = hfs_b::Image::create(&disk.0, 16 * 1024 * 1024).unwrap();
    let partition = image.partition();
    mac_hfs_b::format(&mut image,partition,mac_hfs_b::FormatOptions {
        name:"Aliases",created:mac_hfs_b::time::Timestamp(4_600_000_000_000_000),uuid:[42;16]
    },Default::default()).unwrap();
    drop(image);
    mac_system::testing::with_file_disk(&disk.0, false, Default::default(), |services| {
        run(&unsafe { System::from_raw(services) }.unwrap());
    }).unwrap();
}
fn pool(system: &System<'_>, kind: [u8; 4]) -> mac_toolbox::inspection::Pool
{
    let snapshot = system.inspection().snapshot(mac_toolbox::inspection::Scope::Own).unwrap();
    let ledger = snapshot.ledger(0).unwrap();
    (0..ledger.len()).map(|index| ledger.at(index).unwrap()).find(|pool| pool.kind == kind).unwrap()
}
#[test]
fn executable_aliases_hold_mutation_protection_through_invalidation_and_cleanup_retry()
{
    use mac_system::testing::{files::pin_executable, inspection as fixture};
    disk(|system| {
        let files = system.files();
        let volume = files.volume_at(0).unwrap().id;
        let mut writer = files.create_data(mac_toolbox::files::DataFileSpec {
            volume, parent: mac_toolbox::Cnid(ROOT_ID), name: "Executable", replacing: None,
            file_type: mac_toolbox::TypeCode::new(*b"APPL"),
            creator: mac_toolbox::TypeCode::new(*b"TEST") }, 3).unwrap();
        writer.write_all(b"app").unwrap();
        let item = writer.finish().unwrap();
        let first = fixture::add("Executable subject", 0);
        let second = fixture::add("Other executable subject", 0);
        for owner in [first, second]
        {
            with_owner(owner, || {
                pin_executable(volume.0, item.id.0).unwrap();
                assert_eq!((pool(system, *b"FPIN").live, pool(system, *b"FPST").live), (1, 1));
            });
        }
        let protected = || {
            let error = files.remove(volume, item.id).unwrap_err();
            assert_eq!(error, Error::ServiceBusy);
            assert!(error.to_string().contains("busy"));
            assert_eq!(files.catalog(volume, item.id).unwrap().id, item.id);
        };
        protected();
        assert!(fixture::walk_registered(first, false, false).failures.is_empty());
        fixture::try_retire(first).unwrap();
        protected();
        let failed = fixture::walk_registered(second, true, false);
        assert_eq!(failed.report.quarantined, 2);
        assert!(failed.failures.iter().all(|text| text.contains("FPIN") || text.contains("FPST")),
                "{:?}",
                failed.failures);
        assert!(fixture::try_retire(second).unwrap_err().to_string().contains("pending"));
        protected();
        assert!(fixture::walk_registered(second, false, true).failures.is_empty());
        fixture::try_retire(second).unwrap();
        // Last alias, rather than ID invalidation or the first process's exit,
        // releases the common target and makes deletion safe again.
        files.remove(volume, item.id).unwrap();
        assert_eq!(files.catalog(volume, item.id).err(), Some(Error::NotFound));
    });
}

#[test]
fn document_scope_retains_pins_until_real_io_completion_after_owner_invalidation()
{
    use mac_system::testing::{files::with_pending_document_pin, inspection as fixture};
    disk(|system| {
        let files = system.files();
        let volume = files.volume_at(0).unwrap().id;
        let mut writer = files.create_data(mac_toolbox::files::DataFileSpec {
            volume, parent: mac_toolbox::Cnid(ROOT_ID), name: "Pending document", replacing: None,
            file_type: mac_toolbox::TypeCode::new(*b"TEXT"), creator: mac_toolbox::TypeCode::new(*b"TEST")
        }, 3).unwrap();
        writer.write_all(b"doc").unwrap();
        let item = writer.finish().unwrap();
        let owner = fixture::add("Document scope subject", 0);
        with_owner(owner, || {
            with_pending_document_pin(volume.0, item.id.0, || {
                assert_eq!(pool(system, *b"FPIN").live, 1);
                let walked = fixture::walk_registered(owner, false, false);
                assert!(walked.failures.is_empty());
                assert_eq!(pool(system, *b"FPIN").live, 0);
                assert_eq!(pool(system, *b"FPIN").pending, 1);
                assert!(fixture::try_retire(owner).unwrap_err().to_string().contains("pending"));
                let error = files.remove(volume, item.id).unwrap_err();
                assert_eq!(error, Error::ServiceBusy);
                assert!(error.to_string().contains("busy"));
                assert_eq!(files.catalog(volume, item.id).unwrap().id, item.id);
            }).unwrap()
        });
        with_owner(owner, || assert_eq!(pool(system, *b"FPIN").pending, 0));
        fixture::try_retire(owner).unwrap();
        files.remove(volume, item.id).unwrap();
        assert_eq!(files.catalog(volume, item.id).err(), Some(Error::NotFound));
    });
}

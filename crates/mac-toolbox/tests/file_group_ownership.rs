//! Icon-group ownership is the same registered contract as other File objects.
#![cfg(not(target_arch = "riscv64"))]
use std::{fs,
          path::PathBuf,
          sync::atomic::{AtomicUsize, Ordering}};

use mac_abi::files::*;
use mac_system::testing::{files::with_owner, inspection as fixture};
use mac_toolbox::{Error, System};
static NEXT: AtomicUsize = AtomicUsize::new(0);
fn disks(count: usize, run: impl FnOnce(&System<'_>))
{
    struct Disks(Vec<PathBuf>);
    impl Drop for Disks
    {
        fn drop(&mut self)
        {
            for path in &self.0
            {
                let _ = fs::remove_file(path);
            }
        }
    }
    let mut disks = Disks(Vec::new());
    for at in 0..count
    {
        let path = std::env::temp_dir().join(format!("groups-{}-{}-{at}.hfsb",
                                                     std::process::id(),
                                                     NEXT.fetch_add(1, Ordering::Relaxed)));
        disks.0.push(path.clone());
        let mut image = hfs_b::Image::create(&path, 16 * 1024 * 1024).unwrap();
        let partition = image.partition();
        mac_hfs_b::format(&mut image,partition,mac_hfs_b::FormatOptions {
            name:"Groups",created:mac_hfs_b::time::Timestamp(4_600_000_000_000_000),uuid:[at as u8;16]
        },Default::default()).unwrap();
    }
    let paths: Vec<_> = disks.0.iter().map(PathBuf::as_path).collect();
    mac_system::testing::with_file_disks(&paths, false, Default::default(), |services| {
        run(&unsafe { System::from_raw(services) }.unwrap());
    }).unwrap();
}
fn disk(run: impl FnOnce(&System<'_>))
{
    disks(1, run);
}
fn begin(system: &System<'_>, item: mac_toolbox::files::CatalogInfo) -> u64
{
    let mut call = IconGroupCall { operation: BEGIN_ICON_GROUP,
                                   volume: item.volume.0,
                                   when: 4_600_000_000_000_000,
                                   ..Default::default() };
    let row = IconMove { cnid: item.id.0,
                         expected_parent: item.parent.0,
                         parent: item.parent.0,
                         placement: 1,
                         point: mac_toolbox::Point { h: 120,
                                                     v: 80 },
                         ..Default::default() };
    system.client().icon_group(&mut call, &[row]).unwrap();
    call.handle
}
fn invoke(system: &System<'_>, handle: u64, operation: u32) -> Result<IconGroupCall, Error>
{
    let mut call = IconGroupCall { operation,
                                   handle,
                                   ..Default::default() };
    system.client().icon_group(&mut call, &[])?;
    Ok(call)
}
#[test]
fn native_group_abandonment_rejects_foreign_and_reused_ids_without_touching_icons()
{
    disk(|system| {
        let files = system.files();
        let volume = files.volume_at(0).unwrap().id;
        let item = files.new_folder(volume, mac_toolbox::Cnid(ROOT_ID), "Position").unwrap();
        let first = begin(system, item);
        for operation in [STEP_ICON_GROUP, CANCEL_ICON_GROUP, ABANDON_ICON_GROUP]
        {
            let error = with_owner(2002, || invoke(system, first, operation)).err().unwrap();
            assert_eq!(error, Error::AccessDenied);
            assert!(error.to_string().contains("protected"));
        }
        invoke(system, first, ABANDON_ICON_GROUP).unwrap();
        assert_eq!(files.catalog(volume, item.id).unwrap(), item, "abandonment moved an icon");
        let replacement = begin(system, item);
        assert!(first > u64::from(u32::MAX) && replacement > u64::from(u32::MAX));
        let ledger = system.inspection()
                           .snapshot(mac_toolbox::inspection::Scope::Own)
                           .unwrap()
                           .ledger(0)
                           .unwrap();
        let group = (0..ledger.len()).map(|at| ledger.at(at).unwrap())
                                     .find(|row| row.kind == *b"FIGR")
                                     .unwrap();
        assert_eq!((group.slots, group.births, group.deaths),
                   (1, 2, 1),
                   "replacement must reuse freed storage");
        assert_ne!(first, replacement);
        for operation in [STEP_ICON_GROUP, CANCEL_ICON_GROUP, ABANDON_ICON_GROUP]
        {
            let error = invoke(system, first, operation).err().unwrap();
            assert_eq!(error, Error::NotFound);
            assert!(error.to_string().contains("found"));
        }
        assert_eq!(invoke(system, replacement, STEP_ICON_GROUP).unwrap().phase, 1);
        assert_eq!(files.catalog(volume, item.id).unwrap().saved_icon_location(),
                   Some(mac_toolbox::Point { h: 120,
                                             v: 80 }));
    });
}
#[test]
fn owner_walk_and_quarantine_retry_release_native_groups_without_app_specific_exit_scan()
{
    for unavailable in [false, true]
    {
        disk(|system| {
            let files = system.files();
            let volume = files.volume_at(0).unwrap().id;
            let item =
                files.new_folder(volume, mac_toolbox::Cnid(ROOT_ID), "Walked group").unwrap();
            let owner = fixture::add("Icon group owner", 0);
            let id = with_owner(owner, || begin(system, item));
            let walk = fixture::walk_registered(owner, unavailable, false);
            assert_eq!(walk.report.invalidated, 2);
            if unavailable
            {
                assert_eq!(walk.report.quarantined, 2);
                assert!(walk.failures.iter().all(|s| s.contains("FIGR") || s.contains("FIGS")),
                        "{:?}",
                        walk.failures);
                assert!(fixture::try_retire(owner).unwrap_err().to_string().contains("pending"));
            }
            else
            {
                assert!(walk.failures.is_empty());
            }
            assert_eq!(with_owner(owner, || invoke(system, id, STEP_ICON_GROUP)).err(),
                       Some(Error::NotFound));
            if unavailable
            {
                assert!(fixture::walk_registered(owner, false, true).failures.is_empty());
            }
            fixture::try_retire(owner).unwrap();
            assert_eq!(files.catalog(volume, item.id).unwrap(), item);
            invoke(system, begin(system, item), CANCEL_ICON_GROUP).unwrap();
        });
    }
}

#[test]
fn sdk_group_drop_aborts_without_publishing_and_the_next_guard_can_commit()
{
    disk(|system| {
        use mac_toolbox::files::{IconMove, MoveProgress, Placement};
        let files = system.files();
        let volume = files.volume_at(0).unwrap().id;
        let item = files.new_folder(volume, mac_toolbox::Cnid(ROOT_ID), "Guarded icon").unwrap();
        let rows = [IconMove { id:              item.id,
                               expected_parent: item.parent,
                               parent:          item.parent,
                               placement:       Placement::At(mac_toolbox::Point { h: 140,
                                                                                   v: 90 }),
                               to_trash:        false }];
        let guard = files.begin_icon_moves(volume, &rows).unwrap();
        drop(guard);
        assert_eq!(files.catalog(volume, item.id).unwrap(), item);
        let mut retry = files.begin_icon_moves(volume, &rows).unwrap();
        assert_eq!(retry.step().unwrap(), MoveProgress::Committed { changed: true });
        assert_eq!(files.catalog(volume, item.id).unwrap().saved_icon_location(),
                   Some(mac_toolbox::Point { h: 140,
                                             v: 90 }));
    });
}

#[test]
fn native_groups_grow_past_eight_and_cross_kind_ids_are_refused()
{
    disks(20, |system| {
        let files = system.files();
        let mut held = Vec::new();
        for at in 0..20
        {
            let volume = files.volume_at(at).unwrap().id;
            let item = files.new_folder(volume, mac_toolbox::Cnid(ROOT_ID), "Icon").unwrap();
            let id = begin(system, item);
            assert!(id > u64::from(u32::MAX));
            held.push((item, id));
        }
        let (item, stale) = held.remove(0);
        invoke(system, stale, ABANDON_ICON_GROUP).unwrap();
        let replacement = begin(system, item);
        let ledger = system.inspection()
                           .snapshot(mac_toolbox::inspection::Scope::Own)
                           .unwrap()
                           .ledger(0)
                           .unwrap();
        let group = (0..ledger.len()).map(|at| ledger.at(at).unwrap())
                                     .find(|row| row.kind == *b"FIGR")
                                     .unwrap();
        assert_eq!(group.live, 20);
        assert!((20..40).contains(&group.slots));
        assert_eq!(group.capacity, mac_toolbox::inspection::CapacityPolicy::GrowableUntilMemory);
        let error = invoke(system, stale, STEP_ICON_GROUP).err().unwrap();
        assert_eq!(error, Error::NotFound);
        assert!(error.to_string().contains("found"));
        let error = system.client()
                          .fork_write(&mut ForkWriteCall { operation: ABORT_FORK_WRITE,
                                                           handle: replacement,
                                                           ..Default::default() },
                                      "",
                                      "",
                                      &[],
                                      &mut [])
                          .unwrap_err();
        assert_eq!(error, Error::NotFound);
        assert!(error.to_string().contains("found"));
        for (_, id) in held.into_iter().chain([(item, replacement)])
        {
            invoke(system, id, CANCEL_ICON_GROUP).unwrap();
        }
        assert_eq!(files.catalog(item.volume, item.id).unwrap(), item);
    });
}

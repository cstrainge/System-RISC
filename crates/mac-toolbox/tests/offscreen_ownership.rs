//! Public SDK lifetime, ownership and budget contracts for resident GWorlds.
#![cfg(not(target_arch = "riscv64"))]
use mac_system::testing::{files::with_owner, inspection as fixture, with_services};
use mac_toolbox::{Error, Point, Rect, System,
                  inspection::{Pool, Scope},
                  quickdraw::{DrawingTarget, WorldId}};

fn pool(system: &System<'_>, kind: [u8; 4]) -> Pool
{
    let membership = system.inspection().snapshot(Scope::Own).unwrap();
    let ledger = membership.ledger(0).unwrap();
    (0..ledger.len()).map(|index| ledger.at(index).unwrap()).find(|pool| pool.kind == kind).unwrap()
}
fn id(target: DrawingTarget) -> WorldId
{
    match target
    {
        DrawingTarget::World(id) => id,
        other => panic!("expected world target, got {other:?}")
    }
}
#[test]
fn disposal_reuses_the_slot_without_aliasing_stale_or_foreign_worlds()
{
    with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let owner = fixture::add("Offscreen owner", 0);
        let other = fixture::add("Other offscreen owner", 0);
        with_owner(owner, || {
            let world = system.quickdraw().new_gworld(4, 4).unwrap();
            let stale = id(world.graphics().target());
            let before = pool(&system, *b"GWLD");
            assert_eq!(before.live, 1);
            assert_eq!(pool(&system, *b"GWST").live, 1);
            drop(world);
            let fresh = system.quickdraw().new_gworld(4, 4).unwrap();
            assert_ne!(fresh.graphics().target(), DrawingTarget::World(stale));
            let after = pool(&system, *b"GWLD");
            assert_eq!(after.slots, before.slots, "the slot must actually be reused");
            assert_eq!(after.births, before.births + 1);
            assert_eq!(after.deaths, before.deaths + 1);
            assert!(after.generation_headroom < before.generation_headroom);
            let error = system.quickdraw().graphics(stale).state().err().unwrap();
            assert_eq!(error, Error::NotFound);
            assert!(!error.to_string().is_empty());
            assert_eq!(system.client()
                             .world(mac_toolbox::abi::quickdraw::WORLD_DISPOSE,
                                    stale.0,
                                    Point::default(),
                                    &mut [])
                             .err(),
                       Some(Error::NotFound));
            with_owner(other, || {
                let own = system.quickdraw().new_gworld(4, 4).unwrap();
                assert_ne!(own.graphics().target(), fresh.graphics().target());
                let error = fresh.dimensions().unwrap_err();
                assert_eq!(error, Error::AccessDenied);
                assert!(!error.to_string().is_empty());
                assert_eq!(fresh.graphics().paint_rect(Rect::new(0, 0, 4, 4)),
                           Err(Error::AccessDenied));
                assert_eq!(own.dimensions().unwrap(),
                           Point { h: 4,
                                   v: 4 });
            });
            assert_eq!(fresh.dimensions().unwrap(),
                       Point { h: 4,
                               v: 4 });
            drop(fresh);
            assert_eq!(pool(&system, *b"GWLD").live, 0);
            let result = fixture::walk_registered(owner, false, false);
            assert_eq!(result.report.released, 1, "empty backing still belongs to the owner");
            assert!(result.failures.is_empty());
        });
    });
}
#[test]
fn world_budgets_are_per_owner_and_failed_creation_can_be_retried()
{
    with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let owner = fixture::add("Raster owner", 0);
        let other = fixture::add("Other raster owner", 0);
        with_owner(owner, || {
            let world = system.quickdraw().new_gworld(640, 480).unwrap();
            let error = system.quickdraw().new_gworld(640, 480).err().unwrap();
            assert_eq!(error, Error::NoMemory);
            assert!(error.to_string().contains("memory"));
            assert_eq!(pool(&system, *b"GWLD").live, 1);
            with_owner(other, || {
                system.quickdraw().new_gworld(640, 480).unwrap();
            });
            drop(world);
            system.quickdraw().new_gworld(640, 480).unwrap();
            assert_eq!(pool(&system, *b"GWLD").live, 0);
        });
    });
}
#[test]
fn owner_walk_quarantines_worlds_and_backing_until_registered_cleanup_succeeds()
{
    with_services(|services| {
        let system = unsafe { System::from_raw(services) }.unwrap();
        let owner = fixture::add("Quarantined raster owner", 0);
        with_owner(owner, || {
            let world = system.quickdraw().new_gworld(8, 8).unwrap();
            let target = world.graphics().target();
            core::mem::forget(world);
            let failed = fixture::walk_registered(owner, true, false);
            assert_eq!((failed.report.invalidated, failed.report.quarantined), (2, 2));
            assert!(failed.failures
                          .iter()
                          .all(|message| message.contains("no registered cleanup provider")));
            assert_eq!(system.quickdraw().graphics(target).state().err(), Some(Error::NotFound));
            assert_eq!(system.quickdraw().new_gworld(1, 1).err(), Some(Error::ServiceBusy));
            assert_eq!(pool(&system, *b"GWLD").quarantined, 1);
            assert_eq!(pool(&system, *b"GWST").quarantined, 1);
            let retried = fixture::walk_registered(owner, false, true);
            assert!(retried.failures.is_empty());
            let membership = system.inspection().snapshot(Scope::Own).unwrap();
            let owner = membership.at(0).unwrap();
            assert_eq!((owner.live, owner.pending, owner.quarantined), (0, 0, 0));
        });
    });
}

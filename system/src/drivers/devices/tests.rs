extern crate std;
use alloc::{format, vec::Vec};
use std::sync::{Mutex, OnceLock};

use lifetime::Registration;
use mac_object_table::{domain::{DomainKind, OwnerIndex},
                       registry::Release};

use super::*;
use crate::toolbox::handles::{Registry, cleanup::Cause};

static SERIAL: Mutex<()> = Mutex::new(());
fn owner(kind: DomainKind) -> OwnerRef
{
    OwnerRef::try_new(OwnerIndex::issue(kind).unwrap()).unwrap()
}
fn system() -> OwnerRef
{
    static SYSTEM: OnceLock<OwnerRef> = OnceLock::new();
    SYSTEM.get_or_init(|| owner(DomainKind::System)).clone()
}
struct TestBus
{
    stopped:     bool,
    resets:      usize,
    reset_error: Option<Status>,
    probe_error: Option<Status>,
    dma_error:   Option<Status>,
    dma_offset:  u64
}
const DEVICE: Description = Description { handle:         0,
                                          bus:            mac_module::ClassId(*b"TEST"),
                                          key:            [3; 16],
                                          registers:      0x1000,
                                          register_bytes: 4096,
                                          interrupt:      4,
                                          flags:          mac_device_interface::COHERENT_DMA
                                                          | mac_device_interface::MMIO };
impl Bus for TestBus
{
    fn dma_address(&self, address: u64, _: u64) -> Result<u64, Status>
    {
        if let Some(error) = self.dma_error
        {
            return Err(error);
        }
        address.checked_add(self.dma_offset).ok_or(BAD_ARGUMENT)
    }

    fn probe(&mut self, ordinal: u32) -> Result<Description, Status>
    {
        if ordinal < 400 { Ok(DEVICE) } else { Err(NOT_FOUND) }
    }

    fn request_reset(&mut self, _: u32) -> Result<(), Status>
    {
        self.resets += 1;
        self.reset_error.map_or(Ok(()), Err)
    }

    fn stopped(&mut self, _: u32) -> Result<bool, Status>
    {
        self.probe_error.map_or(Ok(self.stopped), Err)
    }
}
struct ReleaseDevice(Registration);
impl Release<Devices<TestBus>, Failure> for ReleaseDevice
{
    fn release(&self, context: &mut Devices<TestBus>, ticket: ReleasedResource)
               -> Result<(), Failure>
    {
        context.cleanup(ticket, self.0)
    }
}
fn fixture(case: [u8; 2]) -> (Devices<TestBus>, Registry<Devices<TestBus>>)
{
    let system = system();
    let registrar = owner(DomainKind::Module);
    let mut catalog = Registry::new();
    let mut index = 0;
    let kinds =
        Registration::ALL.map(|kind| {
                             let mut specification = kind.specification();
                             // Each simulated hardware world gets its own real registered kinds;
                             // abandoned worlds deliberately keep their original System pool
                             // pinned.
                             specification.kind = mac_object_table::domain::KindCode([b'Z',
                                                                                      case[0],
                                                                                      case[1],
                                                                                      b'0'
                                                                                      + index]);
                             index += 1;
                             catalog.register(&registrar, specification, ReleaseDevice(kind))
                                    .unwrap()
                         });
    (Devices::new(TestBus { stopped:     true,
                            resets:      0,
                            reset_error: None,
                            probe_error: None,
                            dma_error:   None,
                            dma_offset:  0x1_0000_0000 },
                  system,
                  kinds).unwrap(),
     catalog)
}
fn drain(manager: &mut Devices<TestBus>, owner: &OwnerRef, catalog: &Registry<Devices<TestBus>>)
         -> Vec<alloc::string::String>
{
    let mut errors = Vec::new();
    mac_object_table::walk::drain(manager,
                                  owner,
                                  |_, key| catalog.dispatch(key.kind()),
                                  |_, error| errors.push(format!("{error}"))).unwrap();
    errors
}
fn retry(manager: &mut Devices<TestBus>, owner: &OwnerRef, catalog: &Registry<Devices<TestBus>>)
{
    for _ in 0..3
    {
        mac_object_table::walk::retry(manager,
                                      owner,
                                      |_, key| catalog.dispatch(key.kind()),
                                      |_, _| ()).unwrap();
    }
}
fn assert_reused_only_slot(owner: &OwnerRef, kind: [u8; 4])
{
    let summary = owner.summary();
    let pool = (0..summary.kinds).map(|at| owner.pool_at(summary.revision, at).unwrap())
                                 .find(|pool| pool.kind == mac_object_table::domain::KindCode(kind))
                                 .unwrap();
    assert_eq!((pool.live, pool.slots, pool.births, pool.deaths),
               (1, 1, 2, 1),
               "second publication must reuse the sole slot rather than avoid stale-ID validation");
}

#[test]
fn native_claims_and_maps_reject_stale_foreign_and_reused_ids()
{
    let _serial = SERIAL.lock().unwrap();
    let (mut manager, _catalog) = fixture(*b"01");
    let a = owner(DomainKind::Module);
    let b = owner(DomainKind::Module);
    let first = manager.claim(&a, 0, DEVICE).unwrap();
    assert!(first.handle > u32::MAX as u64);
    assert_eq!(manager.claim(&b, 0, DEVICE), Err(SERVICE_BUSY));
    assert_eq!(manager.release(&b, first.handle), Err(ACCESS_DENIED));
    let mapping = manager.map(&a, first.handle, 0x8000_0000, 4096).unwrap();
    assert_eq!(mapping.address, 0x1_8000_0000,
               "device address comes from the architecture binding, not the CPU request");
    assert!(mapping.handle > u32::MAX as u64);
    assert_eq!(manager.release(&a, first.handle), Err(SERVICE_BUSY));
    assert_eq!(manager.unmap(&b, mapping.handle), Err(ACCESS_DENIED));
    assert_eq!(manager.unmap(&a, first.handle), Err(NOT_FOUND));
    manager.unmap(&a, mapping.handle).unwrap();
    let next = manager.map(&a, first.handle, 0x8000_0000, 4096).unwrap();
    assert_ne!(next.handle, mapping.handle);
    assert_reused_only_slot(&a, *b"Z014");
    assert_eq!(manager.unmap(&a, mapping.handle), Err(NOT_FOUND));
    manager.unmap(&a, next.handle).unwrap();
    manager.release(&a, first.handle).unwrap();
    let second = manager.claim(&a, 0, DEVICE).unwrap();
    assert_ne!(first.handle, second.handle);
    assert_reused_only_slot(&a, *b"Z010");
    assert_eq!(manager.map(&a, first.handle, 0x8000_0000, 4096), Err(NOT_FOUND));
    let (mut foreign, _catalog) = fixture(*b"02");
    let foreign_claim = foreign.claim(&a, 0, DEVICE).unwrap();
    assert_ne!(foreign_claim.handle, second.handle);
    assert_eq!(foreign.release(&a, second.handle), Err(NOT_FOUND));
    foreign.release(&a, foreign_claim.handle).unwrap();
    manager.release(&a, second.handle).unwrap();
}
#[test]
fn dma_translation_refusal_preserves_claim_and_publication_counts_for_retry()
{
    let _serial = SERIAL.lock().unwrap();
    let (mut manager, _catalog) = fixture(*b"DT");
    let owner = owner(DomainKind::Module);
    let device = manager.claim(&owner, 0, DEVICE).unwrap();
    let before = owner.summary().live;
    manager.bus.dma_error = Some(mac_abi::ACCESS_DENIED);
    assert_eq!(manager.map(&owner, device.handle, 0x8000, 128), Err(mac_abi::ACCESS_DENIED));
    assert_eq!(owner.summary().live, before);
    assert_eq!(manager.maps.retained_values().count(), 0);
    manager.bus.dma_error = None;
    manager.bus.dma_offset = u64::MAX - 0x8000 - 63;
    assert_eq!(manager.map(&owner, device.handle, 0x8000, 128), Err(BAD_ARGUMENT));
    assert_eq!(owner.summary().live, before);
    manager.bus.dma_offset = 0;
    let mapping = manager.map(&owner, device.handle, 0, 128).unwrap();
    assert_eq!(mapping.address, 0, "zero is a valid physical address when the binding admits it");
    manager.unmap(&owner, mapping.handle).unwrap();
    manager.release(&owner, device.handle).unwrap();
}
#[test]
fn reset_is_bounded_and_never_releases_live_dma()
{
    let _serial = SERIAL.lock().unwrap();
    let (mut manager, _catalog) = fixture(*b"03");
    let a = owner(DomainKind::Module);
    let device = manager.claim(&a, 0, DEVICE).unwrap();
    let map = manager.map(&a, device.handle, 0x8000, 128).unwrap();
    manager.bus.stopped = false;
    assert_eq!(manager.unmap(&a, map.handle), Err(SERVICE_BUSY));
    assert_eq!(manager.start_reset(&a, device.handle, 11), Ok(false));
    assert_eq!(manager.reset_pending(&a, 12), Err(NOT_FOUND));
    assert_eq!(manager.reset_pending(&a, 11), Ok(false));
    assert_eq!(manager.bus.resets, 1);
    assert_eq!(manager.release(&a, device.handle), Err(SERVICE_BUSY));
    manager.bus.stopped = true;
    assert_eq!(manager.unmap(&a, map.handle), Err(SERVICE_BUSY));
    assert_eq!(manager.reset_pending(&a, 11), Ok(true));
    manager.unmap(&a, map.handle).unwrap();
    manager.release(&a, device.handle).unwrap();
}
#[test]
fn owner_walk_retains_unique_tickets_and_claim_exclusivity_until_real_stop_ack()
{
    let _serial = SERIAL.lock().unwrap();
    let (mut manager, catalog) = fixture(*b"04");
    let a = owner(DomainKind::Module);
    let b = owner(DomainKind::Module);
    let device = manager.claim(&a, 0, DEVICE).unwrap();
    let map = manager.map(&a, device.handle, 0x8000, 128).unwrap();
    manager.bus.stopped = false;
    let errors = drain(&mut manager, &a, &catalog);
    assert!(errors.iter()
                  .any(|e| {
                      e.contains("awaits hardware stop") && e.contains("remain retained for retry")
                  }));
    assert!(a.summary().pending >= 2);
    assert_eq!(manager.unmap(&a, map.handle), Err(NOT_FOUND));
    assert_eq!(manager.release(&a, device.handle), Err(NOT_FOUND));
    assert_eq!(manager.claim(&b, 0, DEVICE), Err(SERVICE_BUSY));
    assert_eq!(manager.bus.resets, 1, "one reset for all mappings of the same original claim");
    retry(&mut manager, &a, &catalog);
    assert!(a.summary().pending >= 2);
    assert_eq!(manager.bus.resets, 1);
    manager.bus.stopped = true;
    retry(&mut manager, &a, &catalog);
    assert_eq!(a.summary().pending, 0);
    assert_eq!(a.summary().live, 0);
    let next = manager.claim(&b, 0, DEVICE).unwrap();
    assert_ne!(next.handle, device.handle);
    manager.release(&b, next.handle).unwrap();
}
#[test]
fn reset_write_and_stop_probe_errors_preserve_typed_cause_payload_and_retry()
{
    let _serial = SERIAL.lock().unwrap();
    let (mut manager, catalog) = fixture(*b"05");
    let a = owner(DomainKind::Module);
    let device = manager.claim(&a, 0, DEVICE).unwrap();
    let map = manager.map(&a, device.handle, 0x8000, 128).unwrap();
    manager.bus.reset_error = Some(mac_abi::IO_ERROR);
    let errors = drain(&mut manager, &a, &catalog);
    assert!(errors.iter().any(|e| {
                             e.contains("reset/stop probe failed with status")
                             && e.contains("no stop acknowledgement")
                         }));
    assert_eq!(manager.maps.retained_values().count(), 1);
    assert_eq!(manager.claims.retained().count(), 1);
    manager.bus.reset_error = None;
    manager.bus.probe_error = Some(mac_abi::IO_ERROR);
    retry(&mut manager, &a, &catalog);
    assert!(a.summary().pending > 0);
    assert_eq!(manager.maps.retained_values().count(), 1);
    manager.bus.probe_error = None;
    retry(&mut manager, &a, &catalog);
    assert_eq!(a.summary().pending, 0);
    assert_eq!(manager.unmap(&a, map.handle), Err(NOT_FOUND));
}
#[test]
fn broker_reset_receipt_can_finish_after_claim_invalidation_without_reviving_it()
{
    let _serial = SERIAL.lock().unwrap();
    let (mut manager, catalog) = fixture(*b"06");
    let a = owner(DomainKind::Module);
    let b = owner(DomainKind::Module);
    let device = manager.claim(&a, 0, DEVICE).unwrap();
    manager.bus.stopped = false;
    assert_eq!(manager.start_reset(&a, device.handle, 91), Ok(false));
    drain(&mut manager, &a, &catalog);
    assert_eq!(manager.reset_pending(&b, 91), Err(NOT_FOUND));
    assert_eq!(manager.reset_pending(&a, 92), Err(NOT_FOUND));
    assert_eq!(manager.reset_pending(&a, 91), Ok(false));
    manager.bus.stopped = true;
    assert_eq!(manager.reset_pending(&a, 91), Ok(true));
    retry(&mut manager, &a, &catalog);
    assert_eq!(a.summary().pending, 0);
    assert_eq!(manager.release(&a, device.handle), Err(NOT_FOUND));
}
#[test]
fn maps_grow_beyond_projection_capacity_and_targets_stay_in_system_domain()
{
    let _serial = SERIAL.lock().unwrap();
    let (mut manager, catalog) = fixture(*b"07");
    let a = owner(DomainKind::Module);
    let device = manager.claim(&a, 0, DEVICE).unwrap();
    let mut maps = Vec::new();
    for _ in 0..320
    {
        maps.push(manager.map(&a, device.handle, 0x8000, 128).unwrap());
    }
    assert_eq!(manager.claims.retained().count(), 1);
    assert_eq!(manager.maps.retained_values().count(), 320);
    assert_eq!(a.summary().live,
               323,
               "one claim/backing plus maps/backing, no physical target charge");
    drain(&mut manager, &a, &catalog);
    retry(&mut manager, &a, &catalog);
    assert_eq!(a.summary().pending, 0);
    for map in maps
    {
        assert_eq!(manager.unmap(&a, map.handle), Err(NOT_FOUND));
    }
}
#[test]
fn foreign_cleanup_ticket_never_acknowledges_or_mutates_either_device_world()
{
    let _serial = SERIAL.lock().unwrap();
    let (mut first, _catalog) = fixture(*b"08");
    let (mut second, _catalog) = fixture(*b"09");
    let a = owner(DomainKind::Module);
    let b = owner(DomainKind::Module);
    let x = first.claim(&a, 0, DEVICE).unwrap();
    let y = second.claim(&b, 0, DEVICE).unwrap();
    let x = first.map(&a, x.handle, 0x8000, 128).unwrap();
    let y = second.map(&b, y.handle, 0x8000, 128).unwrap();
    let key = second.maps.key(y.handle).unwrap();
    let ticket = b.invalidate(key).unwrap();
    let failure = first.cleanup(ticket, Registration::Map).unwrap_err();
    assert!(format!("{failure}").contains("stale or foreign"), "{failure}");
    let (ticket, cause) = failure.into_parts();
    assert!(matches!(cause, Cause::Facility(_)));
    assert!(b.pending(key));
    assert_eq!(first.bus.resets, 0);
    assert!(first.maps.get(x.handle).is_some());
    second.cleanup(ticket, Registration::Map).unwrap();
    assert!(!b.pending(key));
    first.unmap(&a, x.handle).unwrap();
    // This fixture deliberately leaves the now-unmapped claims to abandonment;
    // their pending ledgers must not be confused with a reset acknowledgement.
}
#[test]
fn losing_manager_quarantines_live_claim_and_dma_instead_of_acknowledging_them()
{
    let _serial = SERIAL.lock().unwrap();
    let (mut manager, _catalog) = fixture(*b"10");
    let a = owner(DomainKind::Module);
    let device = manager.claim(&a, 0, DEVICE).unwrap();
    manager.map(&a, device.handle, 0x8000, 128).unwrap();
    manager.bus.stopped = false;
    let physical_pending = system().summary().pending;
    drop(manager);
    assert_eq!(system().summary().pending, physical_pending + 1, "System target remains pinned");
    assert_eq!(a.summary().live, 2, "only registered backing remains live");
    assert_eq!(a.summary().pending, 2);
    assert_eq!(a.summary().quarantined, 2);
}

#[test]
fn hardware_ack_retires_a_dead_reset_without_resetting_a_later_claim()
{
    let _serial = SERIAL.lock().unwrap();
    let (mut manager, catalog) = fixture(*b"11");
    let a = owner(DomainKind::Module);
    let b = owner(DomainKind::Module);
    let old = manager.claim(&a, 0, DEVICE).unwrap();
    manager.bus.stopped = false;
    assert_eq!(manager.start_reset(&a, old.handle, 101), Ok(false));
    drain(&mut manager, &a, &catalog);
    assert!(a.summary().pending > 0);
    // The real ACK is sufficient for hardware retirement even if the dying
    // client lost its reset waiter. It does not acknowledge the broker's DREQ.
    manager.bus.stopped = true;
    retry(&mut manager, &a, &catalog);
    assert_eq!(a.summary().pending, 0);
    let new = manager.claim(&b, 0, DEVICE).unwrap();
    let resets = manager.bus.resets;
    assert_eq!(manager.reset_pending(&a, 101), Err(NOT_FOUND));
    assert_eq!(manager.start_reset(&a, old.handle, 101), Err(NOT_FOUND));
    assert_eq!(manager.bus.resets, resets, "late work must not reset a replacement claim");
    manager.release(&b, new.handle).unwrap();
}

#[test]
fn changed_reserved_invalid_and_stopping_admission_never_publishes_partial_claims_or_maps()
{
    let _serial = SERIAL.lock().unwrap();
    let (mut manager, _catalog) = fixture(*b"12");
    let a = owner(DomainKind::Module);
    let reserved = Description { flags: DEVICE.flags | mac_device_interface::RESIDENT_OWNED,
                                 ..DEVICE };
    assert_eq!(manager.claim(&a, 0, reserved), Err(ACCESS_DENIED));
    let changed = Description { key: [9; 16],
                                ..DEVICE };
    assert_eq!(manager.claim(&a, 0, changed), Err(NOT_FOUND));
    let device = manager.claim(&a, 0, DEVICE).unwrap();
    assert_eq!(manager.map(&a, device.handle, u64::MAX, 2), Err(BAD_ARGUMENT));
    assert_eq!(manager.map(&a, device.handle, 1, 0), Err(BAD_ARGUMENT));
    assert_eq!(manager.maps.retained_values().count(), 0);
    a.stop_admission();
    assert_eq!(manager.map(&a, device.handle, 0x8000, 128), Err(SERVICE_BUSY));
    assert_eq!(manager.claim(&a, 1, DEVICE), Err(SERVICE_BUSY));
    assert_eq!(manager.maps.retained_values().count(), 0);
    // Failed publication does not increment the dependency count or leave a
    // second physical target, so the existing stopped claim remains releasable.
    assert_eq!(manager.claims.objects().count(), 1);
    manager.release(&a, device.handle).unwrap();
}

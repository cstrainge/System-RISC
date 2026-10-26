//! Managed controls through the public SDK, shared by host and RV64 userspace.
use mac_toolbox::{abi::{NOT_FOUND, OK},
                  controls::{ControlId, ScrollbarState, StandardKind, StandardState},
                  *};

use super::Failure;

/// The same census/staleness assertions run against host services and real
/// native ECALLs. No private inspection fixture supplies these observations.
pub fn ledger_pools(system: &System<'_>, window: WindowId) -> Result<(), Failure>
{
    let map = |error: mac_toolbox::inspection::Failure| Failure::Service(error.cause.status());
    let owned = system.controls().create_static_text(window,
                                                      Rect::new(4, 4, 104, 24),
                                                      "Ledger census",
                                                      Default::default(),
                                                      controls::TextAlignment::Left)?;
    let membership = system.inspection().snapshot(inspection::Scope::Own).map_err(map)?;
    let ledger = membership.ledger(0).map_err(map)?;
    let mut found = None;
    for index in 0..ledger.len()
    {
        let pool = ledger.at(index).map_err(map)?;
        if pool.kind == *b"CTRL"
        {
            found = Some((index, pool));
            break;
        }
    }
    let (index, before) = found.ok_or(Failure::Mismatch("live controls absent from owner ledger"))?;
    let registrations = system.inspection().object_kinds().map_err(map)?;
    let registration = registrations.get(before.registration
                                               .ok_or(Failure::Mismatch("control pool has no \
                                                                         release binding"))?)
                                    .map_err(map)?;
    assert_eq!(registration.namespace, *b"CTRL");
    assert_eq!(registration.provider.as_str(), "System Control Manager");
    assert_eq!(registration.label.as_str(), "Managed controls");
    assert!(registration.admitting && registration.uses >= before.live);
    assert_eq!(registration.source, inspection::RegistrationSource::Resident);
    let mut dependencies = 0;
    let mut requests = 0;
    #[cfg(target_arch = "riscv64")]
    let mut io = 0;
    for index in 0..registrations.count
    {
        let kind = registrations.at(index).map_err(map)?;
        #[cfg(target_arch = "riscv64")]
        for (bit, code) in [*b"DIOP", *b"DIOB", *b"DJOB", *b"DJST"].into_iter().enumerate()
        {
            if kind.namespace == code
            {
                assert_eq!(kind.provider.as_str(), "System I/O Manager");
                assert!(kind.admitting);
                assert_eq!(kind.source, inspection::RegistrationSource::Resident);
                io |= 1 << bit;
            }
        }
        if kind.namespace == *b"DREQ" || kind.namespace == *b"DRQS"
        {
            assert_eq!(kind.provider.as_str(), "System Module Manager");
            assert!(kind.admitting);
            assert_eq!(kind.source, inspection::RegistrationSource::Resident);
            requests |= if kind.namespace == *b"DREQ" { 1 } else { 2 };
        }
        if kind.namespace == *b"DDEP" || kind.namespace == *b"DDST"
        {
            assert!(kind.label.as_str().contains("dependency"));
            assert_eq!(kind.provider.as_str(), "System Module Manager");
            assert_eq!(kind.source, inspection::RegistrationSource::Resident);
            assert!(kind.admitting);
            dependencies |= if kind.namespace == *b"DDEP" { 1 } else { 2 };
        }
    }
    #[cfg(target_arch = "riscv64")]
    assert_eq!(io, 15, "I/O kinds must be ordinary public registrations");
    assert_eq!(requests, 3, "broker requests must use the public registry");
    assert_eq!(dependencies, 3, "module dependency kinds must use the public registry");
    assert_eq!(system.inspection().snapshot(inspection::Scope::All).err().unwrap().cause,
               Error::AccessDenied,
               "ordinary fixture must not gain cross-domain authority merely by reading \
                registrations");
    system.log("KIND_REGISTRATIONS_USERSPACE_PASS public=passed provider=System_Control_Manager \
                binding=passed\n")?;
    assert!(before.live > 0 && before.slots >= before.live);
    assert_eq!(before.capacity, inspection::CapacityPolicy::GrowableUntilMemory);
    assert_eq!(ledger.at(index).map_err(map)?, before, "inspection mutated the live ledger");
    drop(owned);
    let stale = ledger.at(index).unwrap_err();
    assert_eq!(stale.cause, Error::ScanStale, "disposal must invalidate the snapshot");
    let after = membership.ledger(0).map_err(map)?.at(index).map_err(map)?;
    assert_eq!(after.live + 1, before.live);
    assert_eq!(after.deaths, before.deaths + 1);
    assert_eq!(after.pending, 0);
    assert_eq!(after.births, before.births);
    system.log("LEDGER_POOLS_USERSPACE_PASS real_controls=passed stale=passed cleanup=passed\n")?;
    Ok(())
}

pub fn tracking(system: &System<'_>, window: WindowId) -> Result<(), Failure>
{
    let inside = Point { h: 40,
                         v: 20 };
    let outside = Point { h: 180,
                          v: 20 };
    for kind in [StandardKind::Button, StandardKind::Checkbox, StandardKind::Radio]
    {
        let state = StandardState::new(Rect::new(8, 8, 128, 32), "Action");
        let owned = system.controls().create_standard(window, kind, state)?;
        let control = owned.control();
        let press = control.begin_tracking(inside, 0)?;
        assert!(press.handled && press.tracking && !press.action && press.changed);
        assert_ne!(press.standard.highlighted, 0);
        let still = control.poll_tracking(inside, true, 1)?;
        assert!(!still.changed && !still.action, "stationary press must not repaint or fire");
        let out = control.poll_tracking(outside, true, 2)?;
        assert_eq!(out.standard.highlighted, 0);
        assert!(out.changed && !out.action);
        assert!(!control.poll_tracking(outside, false, 3)?.action);
        assert!(!control.info()?.tracking);
        control.begin_tracking(inside, 4)?;
        control.poll_tracking(outside, true, 5)?;
        assert_ne!(control.poll_tracking(inside, true, 6)?.standard.highlighted, 0);
        let release = control.poll_tracking(inside, false, 7)?;
        assert!(release.action && !release.tracking);
        assert_eq!(release.standard.highlighted, 0);
        assert!(!control.poll_tracking(inside, false, 8)?.action, "release fires once");
        control.begin_tracking(inside, 9)?;
        control.cancel()?;
        assert!(!control.poll_tracking(inside, false, 10)?.action);
        control.begin_tracking(inside, 11)?;
        let disabled = StandardState { enabled: 0,
                                       ..state };
        assert!(!control.configure(kind, disabled)?.tracking);
        assert!(!control.poll_tracking(inside, false, 12)?.action);
        assert!(!control.begin_tracking(inside, 13)?.handled);
        let invalid = StandardState { bounds: Rect::ZERO,
                                      ..state };
        assert!(control.configure(kind, invalid).is_err());
        assert_eq!(control.info()?.standard.enabled, 0, "failed configuration is atomic");
        control.configure(kind, state)?;
        control.begin_tracking(inside, 14)?;
        assert!(!control.configure(kind,
                                   StandardState { bounds: Rect::new(9, 8, 129, 32),
                                                   ..state })?
                        .tracking);
    }
    let state = ScrollbarState { bounds: Rect::new(8, 40, 24, 132),
                                 value: 50,
                                 maximum: 100,
                                 active: 1,
                                 ..Default::default() };
    let owned = system.controls().create_scrollbar(window, state, 5, 20)?;
    let control = owned.control();
    let up = Point { h: 16,
                     v: 44 };
    let first = control.begin_tracking(up, 100)?;
    assert!(first.action && first.tracking);
    assert_eq!(first.scrollbar.value, 45);
    assert!(!control.poll_tracking(up, true, 119)?.changed);
    assert_eq!(control.poll_tracking(up, true, 120)?.scrollbar.value, 40);
    assert_eq!(control.poll_tracking(outside, true, 123)?.scrollbar.value, 40);
    assert_eq!(control.poll_tracking(up, true, 126)?.scrollbar.value, 35);
    assert!(!control.poll_tracking(up, false, 127)?.tracking);
    control.configure_scrollbar(state, 5, 20)?;
    let thumb = (40..132).find(|&v| {
                             system.controls()
                                   .scrollbar_hit(window,
                                                  state.bounds,
                                                  50,
                                                  100,
                                                  true,
                                                  Point { h: 16,
                                                          v })
                                   .ok()
                             == Some(controls::Part::Thumb)
                         })
                         .unwrap();
    let anchor = Point { h: 16,
                         v: thumb };
    assert!(control.begin_tracking(anchor, 200)?.tracking);
    let moved = control.poll_tracking(Point { h: 16,
                                              v: thumb + 8 },
                                      true,
                                      201)?;
    assert!(moved.action && moved.scrollbar.value > 50);
    assert!(!control.poll_tracking(Point { h: 16,
                                           v: thumb + 8 },
                                   true,
                                   202)?
                    .changed);
    control.cancel()?;
    system.log("MANAGED_CONTROL_TRACKING_PASS release=once cancel=passed repeat=passed \
                unchanged=passed\n")
          .map_err(|error| Failure::Service(error.status()))
}

pub fn reuse(system: &System<'_>, window: WindowId, gate: mac_abi::TrapGate)
             -> Result<(), Failure>
{
    let mut legacy =
        mac_abi::controls::ControlCall { operation: mac_abi::controls::CREATE_STANDARD,
                                         window: window.0,
                                         standard: StandardState::new(Rect::new(8, 8, 128, 32),
                                                                      ""),
                                         ..Default::default() };
    legacy.standard.title[..5].copy_from_slice(&[4, b'C', b'a', b'f', 0x8e]);
    legacy.standard.title_length = 5;
    assert_eq!(unsafe {
                   gate(mac_abi::traps::PSTRING_CONTROL,
                        core::ptr::from_mut(&mut legacy).cast(),
                        core::mem::size_of_val(&legacy))
               },
               OK as i64);
    let converted = system.controls().control(ControlId(legacy.id));
    let info = converted.info()?;
    assert_eq!(&info.standard.title[..info.standard.title_length as usize], "Café".as_bytes());
    converted.dispose()?;
    let bar = system.controls().create_scrollbar(window,
                                                  ScrollbarState { bounds: Rect::new(8, 40,
                                                                                     24, 132),
                                                                   value: 0,
                                                                   maximum: 100,
                                                                   active: 1,
                                                                   ..Default::default() },
                                                  1,
                                                  10)?;
    let mut configure =
        mac_abi::controls::ControlCall { operation: mac_abi::controls::CONFIGURE_OBJECT,
                                         id: bar.id().0,
                                         scrollbar: ScrollbarState { value: 20,
                                                                     ..bar.control()
                                                                          .info()?
                                                                          .scrollbar },
                                         step: 1,
                                         page: 10,
                                         ..Default::default() };
    assert_eq!(unsafe {
                   gate(mac_abi::traps::PSTRING_CONTROL,
                        core::ptr::from_mut(&mut configure).cast(),
                        core::mem::size_of_val(&configure))
               },
               OK as i64);
    assert_eq!(bar.control().info()?.scrollbar.value, 20);
    drop(bar);
    for _ in 0..8
    {
        let state = StandardState::new(Rect::new(8, 8, 128, 32), "Owned");
        let owner = system.controls().create_standard(window, StandardKind::Button, state)?;
        let stale = owner.id();
        owner.control().dispose()?;
        let replacement = system.controls().create_standard(window, StandardKind::Button, state)?;
        assert!(replacement.id().0 > u64::from(u32::MAX),
                "native identity is not a classic projection");
        assert_ne!(stale, replacement.id());
        for id in [stale, ControlId(u64::from(window.0)), ControlId(u64::from(u32::MAX))]
        {
            let control = system.controls().control(id);
            assert!(matches!(control.info(), Err(Error::NotFound)));
            assert_eq!(control.dispose(), Err(Error::NotFound));
            assert_eq!(control.cancel(), Err(Error::NotFound));
            assert!(matches!(control.configure(StandardKind::Button, state), Err(Error::NotFound)));
            let mut call = mac_abi::controls::ControlCall { operation:
                                                                mac_abi::controls::INFO_OBJECT,
                                                            id: id.0,
                                                            ..Default::default() };
            assert_eq!(unsafe {
                           gate(mac_abi::traps::PSTRING_CONTROL,
                                core::ptr::from_mut(&mut call).cast(),
                                core::mem::size_of_val(&call))
                       },
                       NOT_FOUND as i64);
        }
        drop(owner); // A stale Drop cannot dispose the replacement.
        assert!(replacement.control().info().is_ok());
    }
    Ok(())
}

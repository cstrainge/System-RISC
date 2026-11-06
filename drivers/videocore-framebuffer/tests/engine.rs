//! Host tests for the VideoCore engine. The firmware and the Device DMA service
//! are simulated; the mailbox messages are built and decoded by the real
//! library, so a refused tag or a short reply travels the same path it would on
//! the target.
mod support;

use mac_driver::{Completion, Phase, Request};
use mac_video_interface::{ABORT, APPLY, ATTACH, Attach, CLASS, COMMIT, Damage, Outcome, Poll,
                          Prepared, RELEASE, SERVICE, Size, Surface};
use support::{Firmware, RECORD_BOUNDS, SimPlatform};
use videocore_framebuffer::{Device, Engine, Refusal, device::Hardware};

const SURFACE: Size = Size { width:  800,
                             height: 600 };
const WIDE: Size = Size { width:  1024,
                          height: 768 };

fn attach_at(size: Size) -> Attach
{
    Attach { width:      size.width,
             height:     size.height,
             min_width:  1,
             min_height: 1,
             max_width:  RECORD_BOUNDS.maximum.width,
             max_height: RECORD_BOUNDS.maximum.height }
}

fn attach(device: &mut Device<SimPlatform>, generation: u64) -> Surface
{
    device.attach(&attach_at(SURFACE), generation).expect("the initial mode is within the record")
}

fn attached(firmware: Firmware) -> Device<SimPlatform>
{
    let mut device = Device::new(SimPlatform::new(firmware), RECORD_BOUNDS);
    attach(&mut device, 1);
    device
}

fn damage(x: u32, y: u32, width: u32, height: u32) -> Damage
{
    Damage { x,
             y,
             width,
             height }
}

/// Attach through the engine, so the engine's own state matches the device's.
fn engine_attach(device: &mut Device<SimPlatform>, engine: &mut Engine, generation: u64)
                 -> Surface
{
    let mut output = [0u8; Surface::BYTES];
    let completion = engine.request(device,
                                    Request { ticket:    generation,
                                              phase:     Phase::Start,
                                              class:     CLASS,
                                              operation: ATTACH,
                                              input:     &attach_at(SURFACE).encode() },
                                    &mut output)
                           .expect("ATTACH succeeds");
    assert_eq!(completion, Completion::Complete(Surface::BYTES));
    Surface::decode(&output).expect("a decodable surface")
}

/// Fill a surface with a recognisable pattern: each word encodes its own index.
fn paint(device: &mut Device<SimPlatform>, surface: Surface)
{
    for (index, pixel) in device.platform_mut().surface(surface.allocation).iter_mut().enumerate()
    {
        *pixel = 0x0100_0000 | index as u32;
    }
}

#[test]
fn attach_reports_the_masked_physical_base_and_the_modes_pitch()
{
    // The firmware answers with a 0xC0000000-range bus address; the driver must
    // hand System the physical address, never the alias.
    let device = attached(Firmware::with_base(0xc010_0000));
    assert_eq!(device.active_geometry(), Some(SURFACE));
    assert_eq!(device.active_pitch(), Some(800 * 4));
    assert_eq!(device.platform().physical, 0x0010_0000, "the alias is reduced, not added");
    assert_eq!(device.platform().acquired, [(SURFACE, 3200, 0x0010_0000)]);
    assert_ne!(device.platform().physical, 0xc010_0000);
}

#[test]
fn present_copies_the_damaged_rows_and_cleans_exactly_that_range()
{
    let mut device = attached(Firmware::qemu());
    let surface = attach_surface(&mut device);
    paint(&mut device, surface);
    let region = damage(10, 20, 100, 50);
    device.present(region).expect("the current mode presents");
    for y in 0..SURFACE.height as usize
    {
        for x in 0..SURFACE.width as usize
        {
            let expected = if (10..110).contains(&x) && (20..70).contains(&y)
            {
                0x0100_0000 | (y * 800 + x) as u32
            }
            else
            {
                0xdead_beef
            };
            assert_eq!(device.platform().scanout_pixel(x, y), expected, "pixel {x},{y}");
        }
    }
    // The clean range covers the damaged rows with the mode's pitch: rows 20..69
    // start at 20 * 3200 + 40 and span 49 pitches plus the last row's bytes.
    assert_eq!(device.platform().cleaned, [(0x0010_0000, 20 * 3200 + 40, 49 * 3200 + 400)]);
}

/// The surface ATTACH published, rebuilt from the driver's bookkeeping so a test
/// can address the drawing buffer the way System does.
fn attach_surface(device: &mut Device<SimPlatform>) -> Surface
{
    attach_surface_for(device, SURFACE, 1)
}

fn attach_surface_for(device: &mut Device<SimPlatform>, geometry: Size, generation: u64)
                      -> Surface
{
    let handle = device.holds_geometry(geometry).expect("the mode is held");
    Surface { generation,
              allocation: handle,
              address: device.platform().address_of(handle),
              capacity: device.platform().length_of(handle),
              preferred: Size { width:  0,
                                height: 0 } }
}

#[test]
fn present_uses_the_current_modes_pitch_not_the_previous_one()
{
    let mut device = attached(Firmware { pitch_padding: 128,
                                         ..Firmware::qemu() });
    let surface = attach_surface(&mut device);
    paint(&mut device, surface);
    device.prepare(WIDE, 2).expect("within bounds");
    let prepared = device.candidate_geometry();
    assert_eq!(prepared, Some(WIDE));
    let candidate = device.holds_geometry(WIDE).expect("the candidate is held");
    device.commit(candidate, 2).expect("the candidate is the prepared one");
    assert_eq!(device.active_pitch(), Some(1024 * 4 + 128));
    let wide = attach_surface_for(&mut device, WIDE, 2);
    paint(&mut device, wide);
    device.present(damage(3, 4, 2, 1)).expect("the new mode presents");
    // Row 4 starts at 4 * (4096 + 128) bytes, not at 4 * 4096.
    assert_eq!(device.platform().scanout_pixel(3, 4), 0x0100_0000 | (4 * 1024 + 3) as u32);
    assert_eq!(device.platform().scanout_pixel(0, 4), 0xdead_beef);
    assert_eq!(device.platform().scanout_pixel(3, 5), 0xdead_beef);
    assert_eq!(device.platform().cleaned.last().map(|clean| clean.1), Some(4 * (4096 + 128) + 12));
}

#[test]
fn a_mode_change_reports_the_new_geometry_and_pitch_and_keeps_the_old_allocation()
{
    let mut device = attached(Firmware { pitch_padding: 128,
                                         ..Firmware::qemu() });
    let old = device.holds_geometry(SURFACE).expect("attached");
    let prepared = device.prepare(WIDE, 2).expect("within bounds");
    assert_eq!(prepared.outcome, Outcome::Applied);
    let published = prepared.surface.expect("an applied preparation carries its surface");
    assert_eq!(published.generation, 2);
    // Before the commit the old mode is still the one System draws into, and the
    // firmware has already moved to the new geometry, so presenting is refused
    // rather than drawing with a pitch the display no longer uses.
    assert_eq!(device.active_geometry(), Some(SURFACE));
    assert_eq!(device.present(damage(0, 0, 4, 4)), Err(Refusal::ModeChangePending));
    assert_eq!(device.retired_geometry(), None, "nothing is retired before the commit");
    assert!(device.platform().holds(old), "the old allocation is still owned");
    assert!(!device.platform().freed.contains(&old));

    assert_eq!(device.commit(published.allocation, 2), Ok(Outcome::Applied));
    assert_eq!(device.active_geometry(), Some(WIDE));
    assert_eq!(device.active_pitch(), Some(1024 * 4 + 128));
    assert_eq!(device.retired_geometry(), Some(SURFACE));
    assert!(device.platform().holds(old), "the old allocation outlives the commit");
    assert!(device.platform().freed.is_empty());

    assert_eq!(device.release(old, 1), Ok(()));
    assert_eq!(device.platform().freed, [old]);
    assert_eq!(device.retired_geometry(), None);
}

#[test]
fn an_out_of_bounds_mode_is_refused_naming_the_bound()
{
    let mut device = attached(Firmware::qemu());
    let refused = device.prepare(Size { width:  2560,
                                        height: 1440 },
                                 2)
                        .expect_err("above the record's maximum");
    assert_eq!(refused,
               Refusal::ExceedsDisplayMaximum { width:  2560,
                                                height: 1440,
                                                field:  "max_width",
                                                limit:  1920 });
    assert_eq!(refused.to_string(), "requested 2560x1440 exceeds display.max_width 1920");
    assert_eq!(refused.outcome(), Some(Outcome::TooLarge));

    let refused = device.prepare(Size { width:  0,
                                        height: 600 },
                                 3)
                        .expect_err("a zero side is not a mode");
    assert!(matches!(refused, Refusal::BelowDisplayMinimum { .. }));
    assert!(refused.to_string().contains("below the minimum display size 1x1"));
    // Neither refusal touched the firmware.
    assert_eq!(device.platform().acquired.len(), 1);
}

#[test]
fn a_refused_allocate_is_a_named_error_and_leaves_no_surface()
{
    let mut device = Device::new(SimPlatform::new(Firmware { header: 0x8000_0001,
                                                             ..Firmware::qemu() }),
                                 RECORD_BOUNDS);
    let refused = device.attach(&attach_at(SURFACE), 1).expect_err("the header refused");
    assert_eq!(refused,
               Refusal::Device(mac_videocore_fb::Error::RequestRefused { code: 0x8000_0001 }));
    assert_eq!(device.active_geometry(), None, "no descriptor was invented");
    assert!(refused.to_string().contains("response code"));
}

#[test]
fn a_short_allocate_reply_is_refused()
{
    let mut device = Device::new(SimPlatform::new(Firmware { allocate_returned: 4,
                                                             ..Firmware::qemu() }),
                                 RECORD_BOUNDS);
    let refused =
        device.attach(&attach_at(SURFACE), 1).expect_err("four bytes is not a base and a size");
    assert_eq!(refused,
               Refusal::Device(mac_videocore_fb::Error::ShortValue { tag:      0x0004_0001,
                                                                     needed:   8,
                                                                     returned: 4 }));
    assert_eq!(device.active_geometry(), None);
}

#[test]
fn an_unhonoured_tag_is_refused()
{
    let mut device = Device::new(SimPlatform::new(Firmware { reject: Some(0x0004_0001),
                                                             ..Firmware::qemu() }),
                                 RECORD_BOUNDS);
    let refused = device.attach(&attach_at(SURFACE), 1).expect_err("the allocate tag was refused");
    assert_eq!(refused,
               Refusal::Device(mac_videocore_fb::Error::TagRefused { tag:  0x0004_0001,
                                                                     code: 0 }));
    assert_eq!(device.active_geometry(), None);
}

#[test]
fn a_commit_for_an_unknown_allocation_is_refused()
{
    let mut device = attached(Firmware::qemu());
    assert_eq!(device.commit(0xdead_beef, 1),
               Err(Refusal::UnknownAllocation { allocation: 0xdead_beef }));
    assert_eq!(device.abort(0xdead_beef, 1),
               Err(Refusal::UnknownAllocation { allocation: 0xdead_beef }));
    assert_eq!(device.release(0xdead_beef, 1),
               Err(Refusal::UnknownAllocation { allocation: 0xdead_beef }));
    // A prepared candidate committed with the wrong generation is stale, not
    // unknown, so the caller learns which number was wrong.
    let prepared = device.prepare(WIDE, 2).expect("within bounds");
    let candidate = prepared.surface.expect("applied").allocation;
    assert_eq!(device.commit(candidate, 9),
               Err(Refusal::StaleGeneration { allocation: candidate,
                                              expected:   2,
                                              observed:   9 }));
}

/// The device address issued at allocation must survive the engine's bookkeeping
/// and reach the release unchanged.
///
/// Scope: this covers the engine and the `Platform` seam, which is where the
/// record is copied. It does not cover `native.rs` — that module is
/// `#[cfg(target_arch = "aarch64")]` and is not compiled into a host test — so
/// the machine adapter's own carry is evidenced by the target build and by the
/// absence of a substituted value in the source, not by this test.
#[test]
fn the_issued_device_address_reaches_the_release()
{
    let mut device = attached(Firmware::qemu());
    let old = device.holds_geometry(SURFACE).expect("attached");
    let issued = device.platform()
                       .allocations
                       .get(&old)
                       .expect("the surface is held")
                       .device_address;
    assert_ne!(issued, 0, "the Device service refuses an allocation it cannot address");
    assert_ne!(issued,
               device.platform().address_of(old),
               "the bus and CPU addresses are different numbers on this platform");

    let prepared = device.prepare(WIDE, 2).expect("within bounds");
    let candidate = prepared.surface.expect("applied").allocation;
    device.commit(candidate, 2).expect("committed");
    device.release(old, 1).expect("retired storage is released");

    assert_eq!(device.platform().released,
               [(old, issued)],
               "the release must hand back the address the allocation issued");
}

#[test]
fn releasing_twice_is_idempotent_but_the_active_surface_cannot_be_released()
{
  let mut device = attached(Firmware::qemu());
    let old = device.holds_geometry(SURFACE).expect("attached");
    let prepared = device.prepare(WIDE, 2).expect("within bounds");
    let candidate = prepared.surface.expect("applied").allocation;
    device.commit(candidate, 2).expect("committed");
    assert_eq!(device.release(old, 1), Ok(()));
    assert_eq!(device.release(old, 1), Ok(()), "a repeated release is acknowledged");
    assert_eq!(device.platform().freed, [old], "the storage is returned exactly once");
    // The live surface is not retired storage, so releasing it is refused rather
    // than blanking the screen.
    assert_eq!(device.release(candidate, 2),
               Err(Refusal::UnknownAllocation { allocation: candidate }));
}

#[test]
fn a_failed_prepare_restores_the_previous_mode()
{
    let mut device = attached(Firmware::qemu());
    device.platform_mut().out_of_memory = true;
    let refused = device.prepare(WIDE, 2).expect_err("the DMA service refused");
    assert_eq!(refused, Refusal::NoMemory);
    assert_eq!(refused.outcome(), Some(Outcome::InsufficientMemory));
    // The property channel applied the new mode before the allocation failed, so
    // the driver must have asked for the old one back.
    assert_eq!(device.platform().acquired.last().map(|mode| mode.0), Some(SURFACE));
    assert_eq!(device.active_geometry(), Some(SURFACE));
    assert_eq!(device.candidate_geometry(), None);
    assert_eq!(device.active_pitch(), Some(3200));
    device.present(damage(0, 0, 2, 2)).expect("the restored mode presents");
}

#[test]
fn abort_and_cancel_restore_the_mode_and_release_the_candidate()
{
    for cancel in [false, true]
    {
        let mut device = attached(Firmware::qemu());
        let prepared = device.prepare(WIDE, 2).expect("within bounds");
        let candidate = prepared.surface.expect("applied").allocation;
        if cancel
        {
            assert_eq!(device.cancel(2), Ok(()));
        }
        else
        {
            assert_eq!(device.abort(candidate, 2), Ok(()));
        }
        assert_eq!(device.candidate_geometry(), None);
        assert_eq!(device.platform().freed, [candidate]);
        assert_eq!(device.platform().acquired.last().map(|mode| mode.0), Some(SURFACE));
        assert_eq!(device.active_geometry(), Some(SURFACE));
        // A repeated abort is acknowledged without a second restore or free.
        assert_eq!(device.abort(candidate, 2), Ok(()));
        assert_eq!(device.platform().freed, [candidate]);
    }
}

#[test]
fn host_size_is_none_because_the_firmware_never_reports_one()
{
    let mut device = attached(Firmware::qemu());
    assert_eq!(device.host_size(), Ok(None));
    assert!(!device.pending(), "the mailbox is request/response; nothing is asynchronous");
    assert!(device.microseconds().expect("the clock answers") > 0);
}

#[test]
fn engine_attach_returns_a_surface_where_the_skeleton_refused()
{
    let mut device = Device::new(SimPlatform::new(Firmware::qemu()), RECORD_BOUNDS);
    let mut engine = Engine::new();
    let mut output = [0u8; Surface::BYTES];
    let completion = engine.request(&mut device,
                                    Request { ticket:    7,
                                              phase:     Phase::Start,
                                              class:     CLASS,
                                              operation: ATTACH,
                                              input:     &attach_at(SURFACE).encode() },
                                    &mut output)
                           .expect("ATTACH succeeds");
    assert_eq!(completion, Completion::Complete(Surface::BYTES));
    let surface = Surface::decode(&output).expect("a decodable surface");
    assert_eq!(surface.generation, 7);
    assert_ne!(surface.allocation, 0);
    assert_eq!(surface.address, device.platform().address_of(surface.allocation));
    assert!(surface.capacity >= u64::from(SURFACE.width) * u64::from(SURFACE.height) * 4);
    assert_eq!((surface.preferred.width, surface.preferred.height), (0, 0));
    assert_eq!(engine.last_refusal(), None);
}

#[test]
fn engine_service_presents_and_reports_no_settled_size()
{
    let mut device = Device::new(SimPlatform::new(Firmware::qemu()), RECORD_BOUNDS);
    let mut engine = Engine::new();
    let surface = engine_attach(&mut device, &mut engine, 1);
    paint(&mut device, surface);
    let mut output = [0u8; Poll::BYTES];
    let completion = engine.request(&mut device,
                                    Request { ticket:    1,
                                              phase:     Phase::Start,
                                              class:     CLASS,
                                              operation: SERVICE,
                                              input:     &damage(1, 2, 3, 4).encode() },
                                    &mut output)
                           .expect("SERVICE succeeds");
    assert_eq!(completion, Completion::Complete(Poll::BYTES));
    assert_eq!(Poll::decode(&output).expect("decodable").settled, None);
    assert_eq!(device.platform().scanout_pixel(1, 2), 0x0100_0000 | (2 * 800 + 1) as u32);
}

#[test]
fn engine_apply_commit_release_selects_a_new_resolution()
{
    let mut device = Device::new(SimPlatform::new(Firmware { pitch_padding: 128,
                                                             ..Firmware::qemu() }),
                                 RECORD_BOUNDS);
    let mut engine = Engine::new();
    engine_attach(&mut device, &mut engine, 1);
    let old = device.holds_geometry(SURFACE).expect("attached");
    let mut prepared_bytes = [0u8; Prepared::BYTES];
    let completion = engine.request(&mut device,
                                    Request { ticket:    11,
                                              phase:     Phase::Start,
                                              class:     CLASS,
                                              operation: APPLY,
                                              input:     &WIDE.encode() },
                                    &mut prepared_bytes)
                           .expect("APPLY succeeds");
    assert_eq!(completion, Completion::Complete(Prepared::BYTES));
    let prepared = Prepared::decode(&prepared_bytes).expect("decodable");
    assert_eq!(prepared.outcome, Outcome::Applied);
    let surface = prepared.surface.expect("an applied preparation carries its surface");

    let mut acknowledgement = [0u8; 16];
    acknowledgement[..8].copy_from_slice(&surface.allocation.to_le_bytes());
    acknowledgement[8..].copy_from_slice(&surface.generation.to_le_bytes());
    let mut outcome_bytes = [0u8; Outcome::BYTES];
    let completion = engine.request(&mut device,
                                    Request { ticket:    11,
                                              phase:     Phase::Start,
                                              class:     CLASS,
                                              operation: COMMIT,
                                              input:     &acknowledgement },
                                    &mut outcome_bytes)
                           .expect("COMMIT succeeds");
    assert_eq!(completion, Completion::Complete(Outcome::BYTES));
    assert_eq!(Outcome::from_code(u32::from_le_bytes(outcome_bytes)), Some(Outcome::Applied));
    assert_eq!(device.active_geometry(), Some(WIDE));

    let mut released = [0u8; 16];
    released[..8].copy_from_slice(&old.to_le_bytes());
    released[8..].copy_from_slice(&1u64.to_le_bytes());
    let completion = engine.request(&mut device,
                                    Request { ticket:    11,
                                              phase:     Phase::Start,
                                              class:     CLASS,
                                              operation: RELEASE,
                                              input:     &released },
                                    &mut [])
                           .expect("RELEASE succeeds");
    assert_eq!(completion, Completion::Complete(0));
    assert_eq!(device.platform().freed, [old]);
}

#[test]
fn engine_abort_after_a_refused_surface_restores_the_mode()
{
    let mut device = Device::new(SimPlatform::new(Firmware::qemu()), RECORD_BOUNDS);
    let mut engine = Engine::new();
    engine_attach(&mut device, &mut engine, 1);
    let mut prepared_bytes = [0u8; Prepared::BYTES];
    engine.request(&mut device,
                   Request { ticket:    21,
                             phase:     Phase::Start,
                             class:     CLASS,
                             operation: APPLY,
                             input:     &WIDE.encode() },
                   &mut prepared_bytes)
          .expect("APPLY succeeds");
    let surface = Prepared::decode(&prepared_bytes).expect("decodable").surface.expect("applied");
    let mut acknowledgement = [0u8; 16];
    acknowledgement[..8].copy_from_slice(&surface.allocation.to_le_bytes());
    acknowledgement[8..].copy_from_slice(&surface.generation.to_le_bytes());
    let completion = engine.request(&mut device,
                                    Request { ticket:    21,
                                              phase:     Phase::Start,
                                              class:     CLASS,
                                              operation: ABORT,
                                              input:     &acknowledgement },
                                    &mut [])
                           .expect("ABORT succeeds");
    assert_eq!(completion, Completion::Complete(0));
    assert_eq!(device.active_geometry(), Some(SURFACE));
    assert_eq!(device.platform().acquired.last().map(|mode| mode.0), Some(SURFACE));
    assert_eq!(device.platform().freed, [surface.allocation]);
}


/// Quiesce ends the module, so every surface storage it opened must be handed
/// back. A dropped record leaves the allocation on the Device service's books
/// with no owner — the leak this pins.
#[test]
fn shutdown_frees_the_active_and_prepared_allocations_and_restores_the_mode()
{
    let mut device = attached(Firmware::qemu());
    let active = device.holds_geometry(SURFACE).expect("attached");
    device.prepare(WIDE, 2).expect("within bounds");
    let candidate = device.holds_geometry(WIDE).expect("prepared");
    assert!(device.platform().holds(active) && device.platform().holds(candidate));

    device.shutdown().expect("every allocation is handed back");

    assert!(device.platform().allocations.is_empty(), "no storage is left owned by nothing");
    assert_eq!(device.platform().freed, [candidate, active]);
    assert_eq!(device.platform().acquired.last().map(|mode| mode.0),
               Some(SURFACE),
               "the firmware is put back in the mode it was displaying");
    assert_eq!(device.holds(active), None);
    assert_eq!(device.holds(candidate), None);
    assert_eq!(device.active_geometry(), None);
}

#[test]
fn shutdown_frees_the_surface_a_commit_left_retired()
{
    let mut device = attached(Firmware::qemu());
    let previous = device.holds_geometry(SURFACE).expect("attached");
    device.prepare(WIDE, 2).expect("within bounds");
    let candidate = device.holds_geometry(WIDE).expect("prepared");
    device.commit(candidate, 2).expect("the candidate is the prepared one");
    assert!(device.holds(previous).is_some() && device.holds(candidate).is_some());

    device.shutdown().expect("every allocation is handed back");

    assert!(device.platform().allocations.is_empty(), "no storage is left owned by nothing");
    assert_eq!(device.platform().freed, [previous, candidate]);
    assert_eq!(device.holds(previous), None);
    assert_eq!(device.holds(candidate), None);
}

#[test]
fn a_second_shutdown_has_nothing_left_to_free()
{
    let mut device = attached(Firmware::qemu());
    device.shutdown().expect("the first shutdown frees the surface");
    device.shutdown().expect("nothing is held, so nothing is refused");

    assert_eq!(device.platform().freed.len(), 1);
}

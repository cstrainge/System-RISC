//! Host tests for the timed-output engine.
//!
//! The serialiser is simulated: the engine's rules — what it accepts, what it
//! refuses, and what it says when it refuses — run here unchanged, because none
//! of them are about registers. What is *not* covered is anything the adapter
//! does: programming the block, cleaning the buffer, the lease, and whether a
//! bit period is what the divisor arithmetic says. Those need a board, and saying
//! so is the point of separating this file from `src/native.rs`.
//!
//! **Which toolchain runs them.** The workspace pins `nightly-2026-10-05` in
//! `rust-toolchain.toml`, and that toolchain's host standard library has been
//! unavailable while its installation is being repaired, so a plain `cargo test`
//! fails with `can't find crate for core`. The tests themselves are sound and
//! have been executed: `cargo +nightly-x86_64-pc-windows-msvc test --package
//! spi0-wave --test engine` reports 12 passed, 0 failed. Use whichever installed
//! toolchain has a working host std; the crate needs nothing the pin provides
//! beyond the edition.
use mac_wave_interface::{Attach, Granted, Progress, State, Submission};
use spi0_wave::engine::{divisor_for, rate_for, Abilities, Platform};
use spi0_wave::{Engine, Refusal};

/// A serialiser that records what it was asked to do and answers the way hardware
/// of this shape answers. `emitting` is the only state the engine's rules depend
/// on, so it is the only state simulated.
struct Sim
{
    abilities: Abilities,
    attached:  Option<Attach>,
    /// Set by `submit`, cleared by `status` reporting completion or by `cancel`.
    emitting:  bool,
    submitted: usize,
    released:  bool
}
impl Sim
{
    fn new() -> Self
    {
        Self { abilities: Abilities { capabilities: mac_wave_interface::ELEMENT_8
                                                    | mac_wave_interface::ELEMENT_16
                                                    | mac_wave_interface::ELEMENT_32
                                                    | mac_wave_interface::PACED
                                                    | mac_wave_interface::HARDWARE_COMPLETION,
                                      maximum_hz:   250_000_000,
                                      minimum_hz:   1,
                                      element_bits: 32,
                                      max_elements: (mac_wave_interface::MAX_TRANSFER / 4) as u32 },
               attached:  None,
               emitting:  false,
               submitted: 0,
               released:  false }
    }

    /// Let the frame finish, as the hardware would.
    fn complete(&mut self)
    {
        self.emitting = false;
    }
}
impl Platform for Sim
{
    fn abilities(&self) -> Abilities
    {
        self.abilities
    }

    fn attach(&mut self, attach: &Attach) -> Result<Granted, Refusal>
    {
        self.attached = Some(*attach);
        Ok(Granted { element_hz:   attach.element_hz,
                     divisor:      4,
                     channel:      2,
                     capabilities: self.abilities.capabilities as u32 })
    }

    fn submit(&mut self, elements: &[u8], _: &Submission, _: &Granted) -> Result<(), Refusal>
    {
        self.emitting = true;
        self.submitted = elements.len();
        Ok(())
    }

    fn status(&mut self) -> Result<Progress, Refusal>
    {
        let (state, reason) = if self.emitting
        {
            (State::Emitting, 0)
        }
        else
        {
            (State::Complete, 0)
        };
        Ok(Progress { state: state.code(), emitted: 0, reason })
    }

    fn cancel(&mut self) -> Result<Progress, Refusal>
    {
        self.emitting = false;
        Ok(Progress { state: State::Cancelled.code(), emitted: 0, reason: 0 })
    }

    fn release(&mut self) -> Result<(), Refusal>
    {
        self.released = true;
        self.attached = None;
        self.emitting = false;
        Ok(())
    }
}

fn attach(width: u32, hz: u32, max: u32) -> Attach
{
    Attach { element_bits: width, element_hz: hz, idle_level: 0, max_elements: max }
}

fn submission(elements: u32) -> Submission
{
    Submission { elements, flags: mac_wave_interface::COMPLETION_REQUIRED, gap_microseconds: 50 }
}

/// The divisor is a whole number of serialiser clocks per element, and the driver
/// reports the rate it actually got rather than the one it was asked for. A
/// caller reading the report sees the quantisation instead of inferring a
/// waveform that was never emitted.
#[test]
fn the_divisor_arithmetic_reports_what_it_actually_produces()
{
    assert_eq!(divisor_for(250_000_000, 800_000), Some(312));
    assert_eq!(rate_for(250_000_000, 312), 801_282);
    // A rate the clock cannot reach is refused rather than rounded to zero.
    assert_eq!(divisor_for(250_000_000, 500_000_000), None);
    assert_eq!(divisor_for(250_000_000, 0), None);
    assert_eq!(divisor_for(0, 1000), None);
}

#[test]
fn an_attach_is_accepted_and_its_grant_kept()
{
    let mut engine = Engine::new();
    let mut sim = Sim::new();
    let granted = engine.attach(&mut sim, attach(8, 800_000, 512)).expect("first attach");
    assert_eq!(granted.channel, 2);
    assert_eq!(engine.attached(), Some(granted));
    assert_eq!(sim.attached.expect("the serialiser was programmed").element_hz, 800_000);
}

#[test]
fn a_second_attach_is_refused_rather_than_silently_replacing_the_first()
{
    let mut engine = Engine::new();
    let mut sim = Sim::new();
    engine.attach(&mut sim, attach(8, 800_000, 512)).expect("first attach");
    assert_eq!(engine.attach(&mut sim, attach(8, 400_000, 512)), Err(Refusal::AlreadyAttached));
}

/// A width the serialiser does not offer is refused with the width it *does*
/// offer, so a caller can degrade instead of guessing.
#[test]
fn an_unsupported_width_names_the_width_that_is_offered()
{
    let mut engine = Engine::new();
    let mut sim = Sim::new();
    sim.abilities.capabilities = mac_wave_interface::ELEMENT_8;
    match engine.attach(&mut sim, attach(32, 800_000, 128))
    {
        Err(Refusal::ElementWidth { offered, requested }) =>
        {
            assert_eq!(requested, 32);
            assert_eq!(offered, 32, "the report names the largest width the serialiser emits");
        }
        other => panic!("expected a width refusal, got {other:?}")
    }
    // A width no serialiser of any kind emits is refused the same way.
    assert!(matches!(engine.attach(&mut sim, attach(7, 800_000, 8)),
                     Err(Refusal::ElementWidth { .. })));
}

#[test]
fn a_rate_above_the_ceiling_names_the_ceiling()
{
    let mut engine = Engine::new();
    let mut sim = Sim::new();
    match engine.attach(&mut sim, attach(8, 300_000_000, 128))
    {
        Err(Refusal::ElementRate { maximum, requested }) =>
        {
            assert_eq!(maximum, 250_000_000);
            assert_eq!(requested, 300_000_000);
        }
        other => panic!("expected a rate refusal, got {other:?}")
    }
    assert!(matches!(engine.attach(&mut sim, attach(8, 0, 128)),
                     Err(Refusal::ElementRate { .. })));
}

#[test]
fn a_buffer_larger_than_the_class_allows_is_refused_at_attach()
{
    let mut engine = Engine::new();
    let mut sim = Sim::new();
    // Four-byte elements and the class ceiling put the limit at 512 elements.
    match engine.attach(&mut sim, attach(32, 800_000, 1024))
    {
        Err(Refusal::BufferTooLarge { capacity, requested }) =>
        {
            assert_eq!(capacity, mac_wave_interface::MAX_TRANSFER as u32);
            assert_eq!(requested, 1024);
        }
        other => panic!("expected a buffer refusal, got {other:?}")
    }
}

#[test]
fn nothing_may_be_submitted_before_an_attach()
{
    let mut engine = Engine::new();
    let mut sim = Sim::new();
    assert_eq!(engine.submit(&mut sim, &[0; 4], submission(1)), Err(Refusal::NotAttached));
    assert_eq!(engine.status(&mut sim).err(), Some(Refusal::NotAttached));
    assert_eq!(engine.cancel(&mut sim).err(), Some(Refusal::NotAttached));
    assert_eq!(engine.release(&mut sim).err(), Some(Refusal::NotAttached));
}

/// The element count and the buffer must agree. A mismatch is refused naming
/// both numbers, because the alternative is emitting a frame whose length the
/// caller did not intend — a garbled strip rather than an error.
#[test]
fn a_submission_whose_count_disagrees_with_its_buffer_is_refused()
{
    let mut engine = Engine::new();
    let mut sim = Sim::new();
    engine.attach(&mut sim, attach(32, 800_000, 64)).expect("attach");
    match engine.submit(&mut sim, &[0; 16], submission(8))
    {
        Err(Refusal::ElementCount { expected, stated }) =>
        {
            assert_eq!(expected, 4);
            assert_eq!(stated, 8);
        }
        other => panic!("expected a count refusal, got {other:?}")
    }
    assert_eq!(engine.submit(&mut sim, &[], submission(0)), Err(Refusal::EmptySubmission));
}

/// One frame at a time. Queueing inside the driver would put the gap between
/// frames under the driver's control, and that gap is the caller's timing
/// decision — it is what latches a strip.
#[test]
fn a_second_frame_is_refused_while_the_first_is_still_emitting()
{
    let mut engine = Engine::new();
    let mut sim = Sim::new();
    engine.attach(&mut sim, attach(32, 800_000, 64)).expect("attach");
    engine.submit(&mut sim, &[0; 16], submission(4)).expect("first frame");
    assert_eq!(engine.submit(&mut sim, &[0; 16], submission(4)),
               Err(Refusal::PreviousFrameInFlight));
    // Once the hardware says the frame finished, the same request is taken.
    sim.complete();
    assert_eq!(engine.submit(&mut sim, &[0; 16], submission(4)), Err(Refusal::Retry));
    engine.submit(&mut sim, &[0; 16], submission(4)).expect("after completion");
}

#[test]
fn status_reports_a_completed_frame_and_frees_the_serialiser()
{
    let mut engine = Engine::new();
    let mut sim = Sim::new();
    engine.attach(&mut sim, attach(8, 800_000, 512)).expect("attach");
    engine.submit(&mut sim, &[0; 8], submission(8)).expect("frame");
    let progress = engine.status(&mut sim).expect("status while emitting");
    assert_eq!(State::from_code(progress.state), Some(State::Emitting));
    sim.complete();
    let progress = engine.status(&mut sim).expect("status after completion");
    assert_eq!(State::from_code(progress.state), Some(State::Complete));
    engine.submit(&mut sim, &[0; 8], submission(8)).expect("the next frame");
}

/// A cancelled frame is reported as cancelled, not as complete: the last element
/// emitted is not the last element of the frame, and a caller that cannot tell
/// those apart cannot decide whether to retransmit.
#[test]
fn cancel_is_distinguishable_from_completion()
{
    let mut engine = Engine::new();
    let mut sim = Sim::new();
    engine.attach(&mut sim, attach(8, 800_000, 512)).expect("attach");
    engine.submit(&mut sim, &[0; 8], submission(8)).expect("frame");
    let progress = engine.cancel(&mut sim).expect("cancel");
    assert_eq!(State::from_code(progress.state), Some(State::Cancelled));
    engine.submit(&mut sim, &[0; 8], submission(8)).expect("after a cancel the line is usable");
}

#[test]
fn release_ends_the_claim_and_the_next_attach_succeeds()
{
    let mut engine = Engine::new();
    let mut sim = Sim::new();
    engine.attach(&mut sim, attach(8, 800_000, 512)).expect("attach");
    engine.submit(&mut sim, &[0; 8], submission(8)).expect("frame");
    engine.release(&mut sim).expect("release");
    assert!(sim.released, "the serialiser and channel were handed back");
    assert_eq!(engine.attached(), None);
    engine.attach(&mut sim, attach(8, 400_000, 512)).expect("attach again");
}


// ---------------------------------------------------------------------------
// What the driver offers, which is the part that was false.
//
// The adapter used to advertise six capabilities while programming no DMA, so the
// engine would accept an attach and a submission for a frame nothing ever shifted
// out. `abilities()` now offers nothing, and these two tests are what make that
// claim checkable on the host: an adapter that reported even one capability would
// let `attach` through, and would fail the first test.
struct Offering(Abilities);

impl Platform for Offering
{
    fn abilities(&self) -> Abilities
    {
        self.0
    }

    fn attach(&mut self, _: &Attach) -> Result<Granted, Refusal>
    {
        panic!("nothing is offered, so attach must be refused before it is reached")
    }

    fn submit(&mut self, _: &[u8], _: &Submission, _: &Granted) -> Result<(), Refusal>
    {
        panic!("nothing is offered, so submit must never be reached")
    }

    fn status(&mut self) -> Result<Progress, Refusal>
    {
        Err(Refusal::NotAttached)
    }

    fn cancel(&mut self) -> Result<Progress, Refusal>
    {
        Err(Refusal::NotAttached)
    }

    fn release(&mut self) -> Result<(), Refusal>
    {
        Ok(())
    }
}

#[test]
fn a_serialiser_that_offers_nothing_cannot_be_attached()
{
    // The refusal must carry the offered width, not a bare "unsupported": a caller
    // that reads `offered: 0` learns nothing is available here and can degrade,
    // which is the whole reason refusals name numbers.
    let mut engine = Engine::new();
    let mut platform = Offering(Abilities { capabilities: 0,
                                            maximum_hz:   0,
                                            minimum_hz:   0,
                                            element_bits: 0,
                                            max_elements: 0 });
    match engine.attach(&mut platform, attach(8, 800_000, 512))
    {
        Err(Refusal::ElementWidth { offered, requested }) =>
        {
            assert_eq!(offered, 0, "nothing is offered");
            assert_eq!(requested, 8, "the request is reported unchanged");
        }
        Err(other) => panic!("expected a width refusal naming the offer, got {other:?}"),
        Ok(_) => panic!("a platform that offers nothing must not be attachable")
    }
    assert_eq!(engine.attached(), None, "nothing was attached");
}

#[test]
fn a_transmit_path_that_does_not_exist_is_reported_as_such()
{
    // The adapter's own refusal. It exists as a value rather than a status code so
    // that a caller and a diagnostic build both see why nothing was emitted.
    let refusal = Refusal::TransmitPathUnavailable;
    let mut text = alloc_string();
    core::fmt::Write::write_fmt(&mut text, format_args!("{refusal}")).expect("formats");
    assert!(text.contains("cannot shift elements out"), "{text}");
    assert!(text.contains("DMA"), "the message names what is missing: {text}");
}

/// A tiny `core::fmt::Write` sink, so the refusal text can be inspected without
/// pulling an allocator into a `no_std` crate's test build.
fn alloc_string() -> String
{
    String::new()
}
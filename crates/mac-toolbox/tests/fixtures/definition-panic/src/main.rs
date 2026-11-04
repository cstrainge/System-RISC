#![no_std]
#![no_main]
#![deny(unsafe_code)]
use core::sync::atomic::{AtomicU32, Ordering};

use mac_definition::{Error, Message, Reply, Result, Services, StaticText,
                     reflow::{self, Extent, Measurement, Requirements, Size}};

// These globals belong to this actual mapped resource, not the application
// executable. Only ordinary typed CDEF replies expose their observations.
struct DisposalCounts
{
    entries:    AtomicU32,
    effects:    AtomicU32,
    duplicates: AtomicU32
}
impl DisposalCounts
{
    const fn new() -> Self
    {
        Self { entries:    AtomicU32::new(0),
               effects:    AtomicU32::new(0),
               duplicates: AtomicU32::new(0) }
    }

    fn dispose(&self) -> Result<Reply>
    {
        let previous = increment(&self.entries)?;
        if previous == 0
        {
            // A real once-only author side effect. A replay is detected without
            // deliberately double-freeing an allocation or causing Rust UB.
            increment(&self.effects)?;
        }
        else
        {
            increment(&self.duplicates)?;
        }
        Ok(Reply::Done)
    }
}
static DISPOSALS: [DisposalCounts; 2] = [const { DisposalCounts::new() }; 2];

fn increment(counter: &AtomicU32) -> Result<u32>
{
    counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| value.checked_add(1))
           .map_err(|_| Error::BadArgument)
}

fn target(text: &str) -> Option<usize>
{
    match text
    {
        "dispose-probe/first" | "dispose-probe/panic" => Some(0),
        "dispose-probe/second" => Some(1),
        _ => None
    }
}

fn witness(text: &str) -> Result<Option<Size>>
{
    let (entries, effects) = match text
    {
        "dispose-probe/witness-first" =>
        {
            (DISPOSALS[0].entries.load(Ordering::Relaxed),
             DISPOSALS[0].effects.load(Ordering::Relaxed))
        }
        "dispose-probe/witness-second" =>
        {
            (DISPOSALS[1].entries.load(Ordering::Relaxed),
             DISPOSALS[1].effects.load(Ordering::Relaxed))
        }
        "dispose-probe/witness-duplicates" =>
        {
            (DISPOSALS[0].duplicates
                         .load(Ordering::Relaxed)
                         .checked_add(DISPOSALS[1].duplicates.load(Ordering::Relaxed))
                         .ok_or(Error::BadArgument)?,
             0)
        }
        _ => return Ok(None)
    };
    Ok(Some(Size::new(Extent::from_wide(i64::from(entries)).map_err(|_| Error::BadArgument)?,
                      Extent::from_wide(i64::from(effects)).map_err(|_| Error::BadArgument)?)))
}

const PENDING_TITLE: &str = "pending-probe/ask-size";

/// Only this exact, explicitly armed native ASK_SIZE enters the nonreturning
/// author body. CREATE and ordinary/default measurements stay nonspinning.
#[inline(never)]
fn pending_control_ask_size() -> !
{
    loop
    {
        core::hint::spin_loop();
    }
}

#[mac_definition::cdef_main]
fn main<'a>(message: Message<'a, StaticText<'a>>, services: Services<'a>) -> Result<Reply>
{
    match message
    {
        Message::Validate(state) =>
        {
            if state.text == PENDING_TITLE
               || target(state.text).is_some()
               || witness(state.text)?.is_some()
            {
                return Ok(Reply::Done);
            }
            // Preserve the original native definition-panic workload for all
            // ordinary titles. ImageProbe's finite counter actor opts in by
            // its own typed state, never by a resident configuration branch.
            panic!("DEFINITION_PANIC_PROBE: safe author body reached; report through the platform \
                    definition runtime")
        }
        Message::Dispose(state) =>
        {
            match target(state.text)
            {
                Some(index) =>
                {
                    let reply = DISPOSALS[index].dispose()?;
                    if state.text == "dispose-probe/panic"
                    {
                        // Real author entry/effect precede the platform panic.
                        // No heap corruption, raw shared global or fake ACK.
                        panic!("DEFINITION_DISPOSE_PANIC_PROBE: entries={} effects={} \
                                duplicates={}",
                               DISPOSALS[index].entries.load(Ordering::Relaxed),
                               DISPOSALS[index].effects.load(Ordering::Relaxed),
                               DISPOSALS[index].duplicates.load(Ordering::Relaxed));
                    }
                    Ok(reply)
                }
                None => Ok(Reply::Done)
            }
        }
        Message::Reflow { state,
                          request,
                          creation } =>
        {
            if services.reflow() != mac_definition::Reflow::Yes
            {
                return Err(Error::NotSupported);
            }
            match request
            {
                reflow::Message::AskSize(constraints) =>
                {
                    if state.text == PENDING_TITLE
                    {
                        if matches!(constraints.width, reflow::AxisConstraint::Exact(width)
                                                       if width.pixels() == 173)
                           && matches!(constraints.height, reflow::AxisConstraint::AtMost(height)
                                                           if height.pixels() == 79)
                        {
                            pending_control_ask_size();
                        }
                        return mac_definition::leaf_reflow(reflow::Message::AskSize(constraints),
                                                           creation,
                                                           Size::default());
                    }
                    let size = witness(state.text)?.ok_or(Error::NotSupported)?;
                    let requirements = Requirements::new(size, size);
                    Ok(Reply::Measurement(Measurement { intrinsic:   requirements,
                                                        constrained: requirements }))
                }
                reflow::Message::InvalidateLayout(_)
                | reflow::Message::AssignRect(_)
                | reflow::Message::SetVisibleRect(_) => Ok(Reply::Done),
                _ => Err(Error::NotSupported)
            }
        }
        Message::DrawGrid { .. } | Message::CalculateGridAxis(_) => Err(Error::NotSupported),
        Message::Draw { .. }
        | Message::HitTest { .. }
        | Message::CalculateRegions(_)
        | Message::Initialize(_)
        | Message::ChangeBounds { .. }
        | Message::Track { .. } => Ok(Reply::Done)
    }
}

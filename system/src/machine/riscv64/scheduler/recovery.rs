//! One boot-admitted Finder stack/CPU pair. Actual backing returns only through
//! native value destruction after scheduler quiescence; rearming scrubs one
//! physical page per maintenance checkpoint without holding metadata locks.
#![forbid(unsafe_code)]
use core::{alloc::Layout,
           sync::atomic::{AtomicBool, Ordering}};

use mac_abi::{BAD_ABI, NO_MEMORY, NOT_SUPPORTED, SERVICE_BUSY, Status};
use spinning_top::Spinlock;

use crate::{allocator::recovery::{Backing, Permit},
            machine::{emergency, interrupts, memory, traps}};

#[derive(Clone, Copy)]
enum Part
{
    Stack,
    Cpu
}
struct State
{
    initialized: bool,
    ready:       bool,
    working:     bool,
    generation:  u64,
    permit:      Option<Permit>,
    stack:       Option<Backing>,
    cpu:         Option<Backing>,
    stack_scrub: usize,
    cpu_scrub:   usize
}
static DIRTY: AtomicBool = AtomicBool::new(false);
static STATE: Spinlock<State> = Spinlock::new(State { initialized: false,
                                                      ready:       false,
                                                      working:     false,
                                                      generation:  0,
                                                      permit:      None,
                                                      stack:       None,
                                                      cpu:         None,
                                                      stack_scrub: 0,
                                                      cpu_scrub:   0 });
fn corrupt(message: &str) -> !
{
    emergency::enter(crate::emergency::Kind::SystemPanic,
                     format_args!("Finder recovery backing invariant: {message}"));
}
pub(crate) fn initialize_finder_recovery(permit: Permit) -> Result<(), Status>
{
    if interrupts::with_masked(|| STATE.lock().initialized)
    {
        return Err(BAD_ABI);
    }
    let stack_bytes = super::STACK_BYTES;
    let cpu_bytes = traps::SavedState::planned_backing_bytes().map_err(|e| e.status())? as usize;
    let layout =
        |bytes| Layout::from_size_align(bytes, memory::PAGE_BYTES as usize).map_err(|_| BAD_ABI);
    // Only bootstrap does full initialization. Interactive rearm is page-bounded.
    let stack = permit.allocate_backing(layout(stack_bytes)?).map_err(|_| NO_MEMORY)?;
    let cpu = permit.allocate_backing(layout(cpu_bytes)?).map_err(|_| NO_MEMORY)?;
    interrupts::with_masked(|| {
        let mut state = STATE.lock();
        if state.initialized
        {
            return Err(BAD_ABI);
        }
        state.initialized = true;
        state.ready = true;
        state.permit = Some(permit);
        state.stack = Some(stack);
        state.cpu = Some(cpu);
        Ok(())
    })
}
/// Unique half of a genuinely boot-admitted pair. Native Stack/SavedState own
/// these values; Drop returns storage to this reservoir rather than buddy.
pub(crate) struct FinderPiece
{
    backing:    Option<Backing>,
    part:       Part,
    dirty:      bool,
    generation: u64
}
impl FinderPiece
{
    pub(crate) fn activate(&mut self)
    {
        self.dirty = true;
    }

    fn backing(&self) -> &Backing
    {
        match &self.backing
        {
            Some(backing) => backing,
            None => corrupt("empty live piece")
        }
    }

    pub(crate) fn cpu_base(&self) -> usize
    {
        self.backing().cpu_base()
    }

    pub(crate) fn physical(&self) -> mac_kernel_model::pages::PhysicalAddress
    {
        self.backing().physical()
    }

    pub(crate) fn bytes(&self) -> u64
    {
        self.backing().bytes()
    }
}
impl Drop for FinderPiece
{
    fn drop(&mut self)
    {
        let Some(backing) = self.backing.take()
        else
        {
            return;
        };
        interrupts::with_masked(|| {
            let mut state = STATE.lock();
            if !state.initialized || state.ready || state.generation != self.generation
            {
                corrupt("foreign or premature returned piece");
            }
            let slot = match self.part
            {
                Part::Stack => &mut state.stack,
                Part::Cpu => &mut state.cpu
            };
            if slot.is_some()
            {
                corrupt("duplicate returned piece");
            }
            let scrubbed = if self.dirty { 0 } else { backing.bytes() as usize };
            *slot = Some(backing);
            match self.part
            {
                Part::Stack => state.stack_scrub = scrubbed,
                Part::Cpu => state.cpu_scrub = scrubbed
            }
            DIRTY.store(true, Ordering::Release);
        });
    }
}
pub(crate) struct FinderBackingLoan
{
    stack:  Option<FinderPiece>,
    cpu:    Option<FinderPiece>,
    permit: Permit
}
impl FinderBackingLoan
{
    pub(crate) fn permit(&self) -> Permit
    {
        self.permit.clone()
    }

    pub(crate) fn stack_bytes(&self) -> u64
    {
        self.stack.as_ref().map_or(0, FinderPiece::bytes)
    }

    pub(crate) fn cpu_bytes(&self) -> u64
    {
        self.cpu.as_ref().map_or(0, FinderPiece::bytes)
    }

    pub(super) fn into_parts(mut self) -> (FinderPiece, FinderPiece)
    {
        match (self.stack.take(), self.cpu.take())
        {
            (Some(stack), Some(cpu)) => (stack, cpu),
            _ => corrupt("incomplete Finder loan")
        }
    }
}
pub(crate) fn take_finder_recovery() -> Result<FinderBackingLoan, Status>
{
    interrupts::with_masked(|| {
        let mut state = STATE.lock();
        if !state.initialized
        {
            return Err(NOT_SUPPORTED);
        }
        if !state.ready || state.working
        {
            return Err(SERVICE_BUSY);
        }
        let generation = state.generation.checked_add(1).ok_or(NO_MEMORY)?;
        let permit = state.permit.as_ref().ok_or(BAD_ABI)?.clone();
        let pair = match (state.stack.take(), state.cpu.take())
        {
            (Some(stack), Some(cpu)) => (stack, cpu),
            _ => corrupt("ready reservoir lost backing")
        };
        state.generation = generation;
        state.ready = false;
        state.stack_scrub = 0;
        state.cpu_scrub = 0;
        Ok(FinderBackingLoan { stack: Some(FinderPiece { backing: Some(pair.0),
                                                         part: Part::Stack,
                                                         dirty: false,
                                                         generation }),
                               cpu: Some(FinderPiece { backing: Some(pair.1),
                                                       part: Part::Cpu,
                                                       dirty: false,
                                                       generation }),
                               permit })
    })
}
pub(crate) fn pump_recovery_backing()
{
    if !DIRTY.load(Ordering::Acquire)
    {
        return;
    }
    let mut completed = false;
    let work = interrupts::with_masked(|| {
        let mut state = STATE.lock();
        if !state.initialized || state.ready || state.working
        {
            return None;
        }
        let next = if state.stack.as_ref().is_some_and(|b| state.stack_scrub < (b.bytes() as usize))
        {
            Some((Part::Stack, state.stack_scrub))
        }
        else if state.cpu.as_ref().is_some_and(|b| state.cpu_scrub < (b.bytes() as usize))
        {
            Some((Part::Cpu, state.cpu_scrub))
        }
        else
        {
            None
        };
        let Some((part, offset)) = next
        else
        {
            state.ready = state.stack.is_some() && state.cpu.is_some();
            completed = state.ready;
            DIRTY.store(false, Ordering::Release);
            return None;
        };
        let backing = match part
        {
            Part::Stack => state.stack.take(),
            Part::Cpu => state.cpu.take()
        }?;
        state.working = true;
        Some((part, offset, backing, state.generation))
    });
    let Some((part, offset, mut backing, generation)) = work
    else
    {
        if completed
        {
            crate::toolbox::processes::finder::backing_ready();
        }
        return;
    };
    let count = (memory::PAGE_BYTES as usize).min(backing.bytes() as usize - offset);
    if backing.zero_range(offset, count).is_err()
    {
        corrupt("scrub extent exceeds owned backing");
    }
    let ready = interrupts::with_masked(|| {
        let mut state = STATE.lock();
        if !state.working || state.generation != generation
        {
            corrupt("scrub generation changed");
        }
        match part
        {
            Part::Stack =>
            {
                state.stack = Some(backing);
                state.stack_scrub = offset + count;
            }
            Part::Cpu =>
            {
                state.cpu = Some(backing);
                state.cpu_scrub = offset + count;
            }
        }
        state.working = false;
        state.ready = state.stack.as_ref().is_some_and(|b| state.stack_scrub == b.bytes() as usize)
                      && state.cpu.as_ref().is_some_and(|b| state.cpu_scrub == b.bytes() as usize);
        if state.ready
        {
            DIRTY.store(false, Ordering::Release);
        }
        state.ready
    });
    if ready
    {
        crate::toolbox::processes::finder::backing_ready();
    }
}

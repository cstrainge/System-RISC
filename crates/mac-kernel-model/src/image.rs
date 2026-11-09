//! Exclusion between overwriting an execution instance and its actual users.
//!
//! This is not image identity or ledger authority. The resident image manager
//! owns the gate; thread records retain pins until their stacks are reclaimed.
#![forbid(unsafe_code)]
use alloc::sync::Arc;
use core::{fmt,
           sync::atomic::{AtomicUsize, Ordering}};

const LOADING: usize = 1usize << (usize::BITS - 1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Allocation,
    Busy,
    PinLimit
}
impl fmt::Display for Error
{
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        output.write_str(match self
                         {
                             Self::Allocation => "execution gate metadata allocation failed",
                             Self::Busy =>
                             {
                                 "execution instance is loading or still retained by a thread"
                             }
                             Self::PinLimit => "execution instance use count exhausted; pin refused"
                         })
    }
}

/// Nonblocking atomic admission: no manager lock or IRQ masking is needed for
/// this counter. The image manager still authenticates the instance and owner.
pub struct ExecutionGate
{
    state: Arc<AtomicUsize>
}
impl ExecutionGate
{
    pub fn try_new() -> Result<Self, Error>
    {
        Ok(Self { state: Arc::try_new(AtomicUsize::new(0)).map_err(|_| Error::Allocation)? })
    }

    /// Retain code already initialized and published by the image manager.
    pub fn pin(&self) -> Result<ExecutionPin, Error>
    {
        self.state
            .try_update(Ordering::Acquire, Ordering::Relaxed, |state| {
                (state < LOADING - 1).then(|| state + 1)
            })
            .map_err(|state| if state & LOADING != 0 { Error::Busy } else { Error::PinLimit })?;
        Ok(ExecutionPin { state: self.state.clone() })
    }

    /// Refuse immediately while any user or another load retains this instance.
    pub fn begin_load(&self) -> Result<Loading, Error>
    {
        self.state
            .compare_exchange(0, LOADING, Ordering::Acquire, Ordering::Relaxed)
            .map_err(|_| Error::Busy)?;
        Ok(Loading { state:  self.state.clone(),
                     active: true })
    }
}

/// Must reside in the scheduler's thread record, never only on an app stack
/// which EXIT can abandon without unwinding. No public construction or cloning.
#[must_use = "retain the execution pin until the thread stack is reclaimed"]
pub struct ExecutionPin
{
    state: Arc<AtomicUsize>
}
impl Drop for ExecutionPin
{
    fn drop(&mut self)
    {
        self.state.fetch_sub(1, Ordering::Release);
    }
}

/// A refused load simply releases exclusion; it publishes no execution pin.
#[must_use = "hold load exclusion across every executable-memory write"]
pub struct Loading
{
    state:  Arc<AtomicUsize>,
    active: bool
}
impl Loading
{
    /// Publish initialized code and retain its first user in one transition.
    /// There is no unpinned interval between loading and thread admission.
    pub fn finish(mut self) -> ExecutionPin
    {
        self.state.store(1, Ordering::Release);
        self.active = false;
        ExecutionPin { state: self.state.clone() }
    }
}
impl Drop for Loading
{
    fn drop(&mut self)
    {
        if self.active
        {
            self.state.store(0, Ordering::Release);
        }
    }
}


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

#[cfg(test)]
mod tests
{
    extern crate std;
    use std::{string::ToString,
              sync::{Arc as Shared, Barrier},
              thread};

    use super::*;

    #[test]
    fn every_execution_user_blocks_overwrite_until_its_actual_release()
    {
        let gate = ExecutionGate::try_new().unwrap();
        let first = gate.pin().unwrap();
        let second = gate.pin().unwrap();
        assert_eq!(gate.begin_load().err(), Some(Error::Busy));
        drop(first);
        assert_eq!(gate.begin_load().err(), Some(Error::Busy));
        drop(second);
        let load = gate.begin_load().unwrap();
        assert_eq!(gate.pin().err(), Some(Error::Busy));
        assert_eq!(gate.begin_load().err(), Some(Error::Busy));
        drop(load);
        assert!(gate.pin().is_ok());
    }

    #[test]
    fn load_handoff_never_reopens_admission_to_another_loader()
    {
        let gate = ExecutionGate::try_new().unwrap();
        let pin = gate.begin_load().unwrap().finish();
        assert_eq!(gate.begin_load().err(), Some(Error::Busy));
        let peer = gate.pin().unwrap();
        drop(pin);
        assert_eq!(gate.begin_load().err(), Some(Error::Busy));
        drop(peer);
        assert!(gate.begin_load().is_ok());
    }

    #[test]
    fn refused_spawn_and_failed_load_leave_the_instance_retryable()
    {
        let gate = ExecutionGate::try_new().unwrap();
        fn fail_load(gate: &ExecutionGate) -> Result<(), Error>
        {
            let _loading = gate.begin_load()?;
            Err(Error::Allocation)
        }
        assert_eq!(fail_load(&gate), Err(Error::Allocation));
        let unstarted = gate.begin_load().unwrap().finish();
        drop(unstarted);
        assert!(gate.begin_load().is_ok());
        assert_eq!(Error::Busy.to_string(),
                   "execution instance is loading or still retained by a thread");
        assert_eq!(Error::Allocation.to_string(), "execution gate metadata allocation failed");
    }

    #[test]
    fn exhausted_use_count_never_wraps_into_loadable_state()
    {
        let gate = ExecutionGate::try_new().unwrap();
        gate.state.store(LOADING - 1, Ordering::Relaxed);
        assert_eq!(gate.pin().err(), Some(Error::PinLimit));
        assert_eq!(gate.begin_load().err(), Some(Error::Busy));
        assert_eq!(gate.state.load(Ordering::Relaxed), LOADING - 1);
        assert_eq!(Error::PinLimit.to_string(),
                   "execution instance use count exhausted; pin refused");
    }

    #[test]
    fn contained_host_unwind_releases_load_and_unstarted_execution_guards()
    {
        let gate = ExecutionGate::try_new().unwrap();
        assert!(std::panic::catch_unwind(|| {
                    let _loading = gate.begin_load().unwrap();
                    panic!("host load failure before publication");
                }).is_err());
        assert!(std::panic::catch_unwind(|| {
                    let _pin = gate.begin_load().unwrap().finish();
                    panic!("host admission failure before scheduler publication");
                }).is_err());
        assert!(gate.begin_load().is_ok());
        // Ordinary Rust unwinding only; no claim of privileged-fault recovery.
    }

    #[test]
    fn simultaneous_loader_and_execution_admission_are_exclusive()
    {
        let gate = Shared::new(ExecutionGate::try_new().unwrap());
        let barrier = Shared::new(Barrier::new(3));
        thread::scope(|scope| {
            let reader = scope.spawn(|| {
                                  barrier.wait();
                                  let pin = gate.pin();
                                  barrier.wait();
                                  barrier.wait();
                                  pin.is_ok()
                              });
            let writer = scope.spawn(|| {
                                  barrier.wait();
                                  let load = gate.begin_load();
                                  barrier.wait();
                                  barrier.wait();
                                  load.is_ok()
                              });
            barrier.wait();
            barrier.wait();
            assert_eq!(gate.begin_load().err(), Some(Error::Busy));
            barrier.wait();
            assert_ne!(reader.join().unwrap(), writer.join().unwrap());
        });
        assert!(gate.begin_load().is_ok());
    }
}

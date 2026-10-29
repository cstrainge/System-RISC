//! Native policy-call restrictions belong to the actual CPU, independently of
//! the interrupted app and the module's temporary execution/account overlays.
//! A returned decision cannot erase an attempted forbidden operation.
#![forbid(unsafe_code)]
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::{HartLocal, required, with_masked};

pub(super) struct State
{
    entered: AtomicBool,
    refused: AtomicU32
}
impl State
{
    pub(super) const fn new() -> Self
    {
        Self { entered: AtomicBool::new(false),
               refused: AtomicU32::new(0) }
    }

    pub(super) fn active(&self) -> bool
    {
        self.entered.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy)]
#[repr(u32)]
pub(crate) enum Operation
{
    Allocate        = 1,
    Deallocate      = 2,
    Reallocate      = 4,
    Service         = 8,
    Broker          = 16,
    Resolve         = 32,
    Worker          = 64,
    Critical        = 128,
    AllocationScope = 256
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Error
{
    Reentry,
    Forbidden
    {
        operations: u32
    }
}
impl core::fmt::Display for Error
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Reentry => f.write_str("another policy invocation is already active on this CPU"),
            Self::Forbidden { operations } =>
            {
                write!(f,
                       "forbidden native operation mask {operations:#x} (allocation=1, \
                        disposal=2, reallocation=4, service=8, broker=16, resolve=32, worker=64, \
                        critical=128, allocation-scope=256)")
            }
        }
    }
}

/// Checked before native gateway dereferences or any allocation effects. This
/// is an ordinary contract refusal; privileged code that bypasses the native
/// gateways or does not return remains outside the proven recovery contract.
pub(crate) fn deny(operation: Operation) -> bool
{
    with_masked(|| deny_selected(operation))
}

/// Resident allocation already needs this exclusion. Check policy permission
/// and perform its real operation in that one mask/restore interval.
pub(crate) fn permitted<R>(kind: Operation, operation: impl FnOnce() -> R) -> Option<R>
{
    with_masked(|| if deny_selected(kind) { None } else { Some(operation()) })
}

fn deny_selected(operation: Operation) -> bool
{
    let Some(local) = super::super::selected_local()
    else
    {
        if super::super::REGISTRY.get().is_some() || super::super::COLD.get().is_some()
        {
            super::super::unadmitted();
        }
        // No policy call exists before initial native entry admission.
        return false;
    };
    if !local.policy.active()
    {
        return false;
    }
    local.policy.refused.fetch_or(operation as u32, Ordering::Relaxed);
    true
}

struct Entered
{
    local:   &'static HartLocal,
    // A callback scope cannot migrate to or be ended on another CPU.
    _affine: core::marker::PhantomData<*mut ()>
}
impl Drop for Entered
{
    fn drop(&mut self)
    {
        self.local.policy.entered.store(false, Ordering::Release);
    }
}

/// Only the core scheduler calls this after retaining the real execution and
/// dropping every registry, scheduler and service guard. No setup/destruction
/// that needs resident allocation belongs inside this callback.
pub(crate) fn invoke<R>(operation: impl FnOnce() -> R) -> Result<R, Error>
{
    with_masked(|| {
        let local = required();
        if local.policy
                .entered
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            local.policy.refused.fetch_or(Operation::Service as u32, Ordering::Relaxed);
            return Err(Error::Reentry);
        }
        local.policy.refused.store(0, Ordering::Relaxed);
        let entered = Entered { local,
                                _affine: core::marker::PhantomData };
        let result = operation();
        let refused = local.policy.refused.load(Ordering::Relaxed);
        drop(entered);
        if refused == 0 { Ok(result) } else { Err(Error::Forbidden { operations: refused }) }
    })
}

//! Bounded resident ownership primitives. No API spins waiting for a scheduled
//! thread. Blocking/wakeup policy belongs to the future OS scheduler.
#![cfg_attr(not(target_arch = "riscv64"), allow(dead_code))]
use spinning_top::Spinlock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessError
{
    Unavailable,
    Busy,
    WrongOwner
}
impl AccessError
{
    pub const fn status(self) -> mac_abi::Status
    {
        match self
        {
            Self::Unavailable => mac_abi::NOT_SUPPORTED,
            Self::Busy => mac_abi::SERVICE_BUSY,
            Self::WrongOwner => mac_abi::WRONG_THREAD
        }
    }
}

pub struct TryState<T: Send>
{
    value: Spinlock<Option<T>>
}
impl<T: Send> TryState<T>
{
    pub const fn new() -> Self
    {
        Self { value: Spinlock::new(None) }
    }

    pub fn publish(&self, value: T) -> Result<(), AccessError>
    {
        let mut slot = self.value.try_lock().ok_or(AccessError::Busy)?;
        if slot.is_some()
        {
            return Err(AccessError::Busy);
        }
        *slot = Some(value);
        Ok(())
    }

    pub fn with<R>(&self, call: impl FnOnce(&mut T) -> R) -> Result<R, AccessError>
    {
        let mut slot = self.value.try_lock().ok_or(AccessError::Busy)?;
        Ok(call(slot.as_mut().ok_or(AccessError::Unavailable)?))
    }
}

/// Opaque firmware contexts need owner-thread access, not an invented Send impl.
/// Unsafe construction installs the authoritative current-thread provider.
pub struct OwnerState<T>
{
    owner:   crate::context::ThreadId,
    current: fn() -> crate::context::ThreadId,
    gate:    Spinlock<()>,
    value:   core::cell::UnsafeCell<Option<T>>
}
// SAFETY: all value access checks the genuine owner identity and exclusively
// borrows under gate. Non-Send T never moves to or is dropped by another thread.
unsafe impl<T> Sync for OwnerState<T>
{
}
impl<T> OwnerState<T>
{
    /// # Safety
    /// `current` must identify the actual OS thread, never caller-supplied data.
    /// Until scheduling exists, the bootstrap thread may be identified by its
    /// exclusive hart; adding a scheduler must replace that provider first.
    pub const unsafe fn new(owner: crate::context::ThreadId,
                            current: fn() -> crate::context::ThreadId)
                            -> Self
    {
        Self { owner,
               current,
               gate: Spinlock::new(()),
               value: core::cell::UnsafeCell::new(None) }
    }

    fn check(&self) -> Result<(), AccessError>
    {
        if (self.current)() != self.owner { Err(AccessError::WrongOwner) } else { Ok(()) }
    }

    pub fn publish(&self, value: T) -> Result<(), AccessError>
    {
        self.check()?;
        let _guard = self.gate.try_lock().ok_or(AccessError::Busy)?;
        // SAFETY: genuine owner +exclusive guard, no references escape.
        let slot = unsafe { &mut *self.value.get() };
        if slot.is_some()
        {
            return Err(AccessError::Busy);
        }
        *slot = Some(value);
        Ok(())
    }

    pub fn with<R>(&self, call: impl FnOnce(&mut T) -> R) -> Result<R, AccessError>
    {
        self.check()?;
        let _guard = self.gate.try_lock().ok_or(AccessError::Busy)?;
        let slot = unsafe { &mut *self.value.get() };
        Ok(call(slot.as_mut().ok_or(AccessError::Unavailable)?))
    }
}

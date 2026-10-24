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
        self.with_owned((), |(), value| call(value)).map_err(|(error, ())| error)
    }

    /// Refusal preserves the caller's linear cleanup capability.
    pub fn with_owned<A, R>(&self, argument: A, call: impl FnOnce(A, &mut T) -> R)
                            -> Result<R, (AccessError, A)>
    {
        let Some(mut slot) = self.value.try_lock()
        else
        {
            return Err((AccessError::Busy, argument));
        };
        let Some(value) = slot.as_mut()
        else
        {
            return Err((AccessError::Unavailable, argument));
        };
        Ok(call(argument, value))
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
        self.with_owned((), |(), value| call(value)).map_err(|(error, ())| error)
    }

    /// Transfer a linear capability only after resident access succeeds.
    /// Refusal returns the original argument, never a reconstructed ticket.
    pub fn with_owned<A, R>(&self, argument: A, call: impl FnOnce(A, &mut T) -> R)
                            -> Result<R, (AccessError, A)>
    {
        if let Err(error) = self.check()
        {
            return Err((error, argument));
        }
        let Some(_guard) = self.gate.try_lock()
        else
        {
            return Err((AccessError::Busy, argument));
        };
        // SAFETY: genuine owner + exclusive guard; no references escape.
        let slot = unsafe { &mut *self.value.get() };
        let Some(value) = slot.as_mut()
        else
        {
            return Err((AccessError::Unavailable, argument));
        };
        Ok(call(argument, value))
    }
}

#[cfg(test)]
mod tests
{
    use core::cell::Cell;

    use super::*;
    fn first() -> crate::context::ThreadId
    {
        crate::context::ThreadId(1)
    }
    fn second() -> crate::context::ThreadId
    {
        crate::context::ThreadId(2)
    }
    struct Ticket<'a>(&'a Cell<usize>);
    impl Drop for Ticket<'_>
    {
        fn drop(&mut self)
        {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn shared_state_refusal_preserves_ticket_until_successful_transfer()
    {
        let drops = Cell::new(0);
        let state = TryState::<u32>::new();
        let (error, ticket) =
            state.with_owned(Ticket(&drops), |_, _| panic!("unavailable state ran cleanup"))
                 .unwrap_err();
        assert_eq!(error, AccessError::Unavailable);
        assert_eq!(drops.get(), 0);
        state.publish(7).unwrap();
        let (error, ticket) =
            state.with(|_| state.with_owned(ticket, |_, _| panic!("busy state ran cleanup")))
                 .unwrap()
                 .unwrap_err();
        assert_eq!(error, AccessError::Busy);
        assert_eq!(drops.get(), 0);
        assert_eq!(state.with_owned(ticket, |ticket, value| {
                            *value += 1;
                            drop(ticket);
                            *value
                        })
                        .ok(),
                   Some(8));
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn refused_owned_access_returns_the_original_linear_argument()
    {
        let drops = Cell::new(0);
        for current in [first as fn() -> _, second as fn() -> _]
        {
            let state = unsafe { OwnerState::<u32>::new(first(), current) };
            let expected = if current() == first()
            {
                AccessError::Unavailable
            }
            else
            {
                AccessError::WrongOwner
            };
            let (error, ticket) =
                state.with_owned(Ticket(&drops), |_, _| panic!("refused access ran callback"))
                     .unwrap_err();
            assert_eq!(error, expected);
            assert_eq!(drops.get(), usize::from(expected == AccessError::WrongOwner));
            drop(ticket);
        }
        assert_eq!(drops.get(), 2);
    }

    #[test]
    fn reentrant_busy_access_preserves_capability_and_success_transfers_it_once()
    {
        let drops = Cell::new(0);
        let state = unsafe { OwnerState::new(first(), first) };
        state.publish(7u32).unwrap();
        let (error, ticket) =
            state.with(|_| {
                     state.with_owned(Ticket(&drops), |_, _| panic!("busy access ran callback"))
                 })
                 .unwrap()
                 .unwrap_err();
        assert_eq!(error, AccessError::Busy);
        assert_eq!(drops.get(), 0);
        let ticket = state.with_owned(ticket, |ticket, value| {
                              *value += 1;
                              ticket
                          })
                          .ok()
                          .unwrap();
        assert_eq!(state.with(|value| *value), Ok(8));
        assert_eq!(drops.get(), 0);
        drop(ticket);
        assert_eq!(drops.get(), 1);
    }
}

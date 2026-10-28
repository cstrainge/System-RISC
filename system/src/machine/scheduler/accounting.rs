//! First resident account consumers: application stacks and saved CPU backing.
//! Physical stack/CPU receipts are distinct from the original-account heap
//! receipts carried by allocator-backed thread metadata. Image backing is separate.
#![forbid(unsafe_code)]

use core::{fmt, ops::Deref};

use mac_kernel_model::account::{Account, AccountUse, Committed, Reserved};

use crate::{machine::{interrupts::NativeInvocationGate,
                      pages,
                      traps::{SavedState, TrapFrame}},
            toolbox::handles::OwnerRef};

pub(crate) type ProcessAccount = Account<NativeInvocationGate>;
pub(crate) type AllocationAuthority = AccountUse<NativeInvocationGate>;

#[derive(Debug)]
pub(crate) enum Error
{
    Account(mac_kernel_model::account::Error),
    Pages(pages::Error),
    Foreign,
    BackingMismatch
    {
        stack_expected: u64,
        stack_actual:   u64,
        cpu_expected:   u64,
        cpu_actual:     u64
    }
}
impl Error
{
    pub(crate) fn status(&self) -> mac_abi::Status
    {
        match self
        {
            Self::Account(error) => account_status(error),
            Self::Pages(error) => error.status(),
            Self::Foreign => mac_abi::ACCESS_DENIED,
            Self::BackingMismatch { .. } => mac_abi::BAD_ABI
        }
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Account(error) => error.fmt(f),
            Self::Pages(error) => error.fmt(f),
            Self::Foreign => f.write_str("thread backing admission belongs to another process"),
            Self::BackingMismatch { stack_expected,
                                    stack_actual,
                                    cpu_expected,
                                    cpu_actual } =>
            {
                write!(f,
                       "thread backing differs from admission: stack \
                        {stack_actual}/{stack_expected} bytes, CPU state \
                        {cpu_actual}/{cpu_expected} bytes (actual/admitted)")
            }
        }
    }
}
pub(crate) use crate::allocation::account_status;

/// Sealed, unique reservations taken from the actual process-directory account.
/// A failed preparation drops these and refunds without publishing a thread.
pub(crate) struct ThreadCharges
{
    stack:      Reserved<NativeInvocationGate>,
    cpu:        Reserved<NativeInvocationGate>,
    allocation: AllocationAuthority
}
impl ThreadCharges
{
    /// Borrow this child's canonical admitted authority before allocating either
    /// thread control block; observing its owner ID grants no such authority.
    pub(super) fn allocation(&self) -> &AllocationAuthority
    {
        &self.allocation
    }

    pub(crate) fn admit(owner: &OwnerRef, account: &ProcessAccount) -> Result<Self, Error>
    {
        if owner.owner() != account.owner()
        {
            return Err(Error::Foreign);
        }
        let stack_bytes = pages::Backing::planned_bytes(super::STACK_BYTES).map_err(Error::Pages)?;
        let cpu_bytes = SavedState::planned_backing_bytes().map_err(Error::Pages)?;
        let allocation = account.admit_use().map_err(Error::Account)?;
        let stack = allocation.reserve_application(stack_bytes).map_err(Error::Account)?;
        let cpu = allocation.reserve_kernel(cpu_bytes).map_err(Error::Account)?;
        Ok(Self { stack,
                  cpu,
                  allocation })
    }

    pub(super) fn allocate(
        self, owner: &OwnerRef, initial: impl FnOnce(usize) -> TrapFrame)
        -> Result<(Charged<pages::Stack>, Charged<SavedState>, AllocationAuthority), Error>
    {
        if self.allocation.owner() != owner.owner()
           || self.stack.owner() != owner.owner()
           || self.cpu.owner() != owner.owner()
        {
            return Err(Error::Foreign);
        }
        let stack = pages::Stack::allocate(super::STACK_BYTES).map_err(Error::Pages)?;
        let cpu = SavedState::new(initial(stack.top())).map_err(Error::Pages)?;
        if stack.backing_bytes() != self.stack.bytes() || cpu.backing_bytes() != self.cpu.bytes()
        {
            return Err(Error::BackingMismatch { stack_expected: self.stack.bytes(),
                                                stack_actual:   stack.backing_bytes(),
                                                cpu_expected:   self.cpu.bytes(),
                                                cpu_actual:     cpu.backing_bytes() });
        }
        Ok((Charged { value:   stack,
                      _charge: self.stack.commit() },
            Charged { value:   cpu,
                      _charge: self.cpu.commit() },
            self.allocation))
    }
}

/// Destruction frees physical backing before refunding its original account.
/// No mutable dereference or extraction can detach the value from its receipt.
pub(super) struct Charged<T>
{
    value:   T,
    _charge: Committed<NativeInvocationGate>
}
impl<T> Deref for Charged<T>
{
    type Target = T;

    fn deref(&self) -> &T
    {
        &self.value
    }
}

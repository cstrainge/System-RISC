//! An owned stack can be pinned independently of ThreadCore. The original THRD
//! still owns final disposal and cannot ACK while any real span pin survives.
#![forbid(unsafe_code)]
use core::{fmt,
           ops::{Deref, Range},
           sync::atomic::{AtomicBool, Ordering}};

use super::*;
pub(super) struct StackBacking
{
    // Real backing leaves before its original-account charge (Charged's order).
    stack:     spin::Once<accounting::Charged<super::super::pages::Stack>>,
    owner:     OwnerRef,
    identity:  spin::Once<CompletionKey>,
    accepting: AtomicBool
}
impl StackBacking
{
    pub(super) fn prepay(owner: &OwnerRef) -> Result<PrepaidStack, mac_abi::Status>
    {
        let backing = Arc::try_new(Self { stack:     spin::Once::new(),
                                          owner:     owner.clone(),
                                          identity:  spin::Once::new(),
                                          accepting: AtomicBool::new(true) }).map_err(|_| {
                                                                                 mac_abi::NO_MEMORY
                                                                             })?;
        Ok(PrepaidStack { backing })
    }

    pub(super) fn prepare(owner: &OwnerRef, stack: accounting::Charged<super::super::pages::Stack>)
                          -> Result<Arc<Self>, mac_abi::Status>
    {
        Ok(Self::prepay(owner)?.finish(stack))
    }

    pub(super) fn bind(&self, identity: CompletionKey) -> Result<(), mac_abi::Status>
    {
        if identity.owner() != self.owner.owner()
        {
            return Err(mac_abi::ACCESS_DENIED);
        }
        let actual = self.identity.call_once(|| identity);
        if *actual != identity
        {
            return Err(mac_abi::BAD_ABI);
        }
        Ok(())
    }

    pub(super) fn close(&self)
    {
        self.accepting.store(false, Ordering::Release);
        super::address_wait_changed();
    }

    fn visible_bounds(&self) -> Option<Range<usize>>
    {
        let stack = self.stack.get()?;
        Some(stack.top() - stack.bytes() as usize..stack.top())
    }
}
impl Deref for StackBacking
{
    type Target = accounting::Charged<super::super::pages::Stack>;

    fn deref(&self) -> &Self::Target
    {
        self.stack.get().expect("published ThreadBacking owns a completed unique stack preparation")
    }
}
/// Unique unpublished Arc allocation; no caller can obtain a backing reference
/// until its real stack is installed. Recovery prepays this before loan use.
pub(super) struct PrepaidStack
{
    backing: Arc<StackBacking>
}
impl PrepaidStack
{
    pub(super) fn finish(self, stack: accounting::Charged<super::super::pages::Stack>)
                         -> Arc<StackBacking>
    {
        self.backing.stack.call_once(|| stack);
        self.backing
    }
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum StackPinError
{
    InvalidSpan
    {
        address: usize,
        length:  usize
    },
    Stopping,
    Unbound,
    MissingBacking
}
impl StackPinError
{
    pub(crate) fn status(&self) -> mac_abi::Status
    {
        match self
        {
            Self::InvalidSpan { .. } => mac_abi::BAD_ARGUMENT,
            Self::Stopping => mac_abi::OPERATION_CANCELLED,
            Self::Unbound | Self::MissingBacking => mac_abi::CORRUPT_DATA
        }
    }
}
impl fmt::Display for StackPinError
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::InvalidSpan { address,
                                length } =>
            {
                write!(out,
                       "application stack span {address:#x}+{length} is empty, overflowing or \
                        outside visible stack bounds")
            }
            Self::Stopping =>
            {
                out.write_str("actual Process or stack incarnation is retiring; new stack spans \
                               are refused")
            }
            Self::Unbound =>
            {
                out.write_str("live application stack has no genuine THRD publication receipt")
            }
            Self::MissingBacking =>
            {
                out.write_str("live application THRD has lost its actual stack backing")
            }
        }
    }
}
pub(crate) struct StackWaitPin
{
    backing:  mac_object_table::retained::Read<StackBacking>,
    identity: CompletionKey,
    address:  usize
}
impl StackWaitPin
{
    pub(crate) fn identity(&self) -> CompletionKey
    {
        self.identity
    }

    pub(crate) fn address(&self) -> usize
    {
        self.address
    }

    pub(crate) fn accepting(&self) -> bool
    {
        self.backing.accepting.load(Ordering::Acquire) && self.backing.owner.accepting()
    }
}
/// Resolve one real same-Process stack under SCHEDULER. The independent span
/// owns no Core/context/root backreference; no typed reference into RAM escapes.
pub(crate) fn pin_application_stack(executor: &crate::context::ApplicationExecutor<'_>,
                                    address: usize, length: usize)
                                    -> Result<Option<StackWaitPin>, StackPinError>
{
    let end = address.checked_add(length)
                     .filter(|_| length != 0)
                     .ok_or(StackPinError::InvalidSpan { address,
                                                         length })?;
    if !executor.owner().accepting()
    {
        return Err(StackPinError::Stopping);
    }
    let scheduler = SCHEDULER.lock();
    for (_, thread) in scheduler.iter()
    {
        if !thread.state.get().live
           || thread.state.get().retired
           || thread.core.context.is_module_worker()
           || thread.core.context.identity().app.0 != executor.owner().owner().incarnation()
        {
            continue;
        }
        let backing = thread.core.backing.lock();
        let stack = &backing.as_ref().ok_or(StackPinError::MissingBacking)?.stack;
        if stack.owner.owner() != executor.owner().owner()
        {
            return Err(StackPinError::Unbound);
        }
        let bounds = stack.visible_bounds().ok_or(StackPinError::MissingBacking)?;
        if address >= bounds.start && address < bounds.end
        {
            if end > bounds.end
            {
                return Err(StackPinError::InvalidSpan { address,
                                                        length });
            }
            if !stack.accepting.load(Ordering::Acquire)
            {
                return Err(StackPinError::Stopping);
            }
            let identity = *stack.identity.get().ok_or(StackPinError::Unbound)?;
            return Ok(Some(StackWaitPin { backing:mac_object_table::retained::Read::new(stack.clone(),context_released),identity,address }));
        }
    }
    Ok(None)
}

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
    stack:     spin::Once<Arc<accounting::Charged<crate::machine::pages::Stack>>>,
    user:      Mutex<UserSlot>,
    owner:     OwnerRef,
    identity:  spin::Once<CompletionKey>,
    accepting: AtomicBool
}
struct UserSlot
{
    mapping: Option<super::super::paging::user::Mapping>,
    busy:    bool,
    waiting: bool
}
impl StackBacking
{
    pub(super) fn prepay(owner: &OwnerRef) -> Result<PrepaidStack, mac_abi::Status>
    {
        let backing = Arc::try_new(Self { stack:     spin::Once::new(),
                                          user:      Mutex::new(UserSlot { mapping: None,
                                                                           busy:    false,
                                                                           waiting: false }),
                                          owner:     owner.clone(),
                                          identity:  spin::Once::new(),
                                          accepting: AtomicBool::new(true) }).map_err(|_| {
                                                                                 mac_abi::NO_MEMORY
                                                                             })?;
        Ok(PrepaidStack { backing })
    }

    pub(super) fn prepare(owner: &OwnerRef, stack: accounting::Charged<crate::machine::pages::Stack>)
                          -> Result<Arc<Self>, mac_abi::Status>
    {
        Self::prepay(owner)?.finish(stack, None)
    }

    pub(super) fn prepare_user(owner: &OwnerRef,
                               stack: accounting::Charged<crate::machine::pages::Stack>,
                               space: &super::super::paging::process::Pin)
                               -> Result<Arc<Self>, mac_abi::Status>
    {
        Self::prepay(owner)?.finish(stack, Some(space))
    }

    fn mapping_turn(&self,
                    operation: impl FnOnce(&mut super::super::paging::user::Mapping)
                           -> Result<(), super::super::paging::user::Error>)
                    -> Result<(), super::user::retirement::Failure>
    {
        let mapping = {
            let mut slot = self.user.lock();
            if slot.busy
            {
                slot.waiting = true;
                return Err(super::user::retirement::Failure::rejected(mac_abi::SERVICE_BUSY));
            }
            let mapping = slot.mapping.take();
            if mapping.is_some()
            {
                slot.busy = true;
            }
            mapping
        };
        let Some(mut mapping) = mapping
        else
        {
            return Ok(());
        };
        // No stack/scheduler lock spans page-table writes or monitor ACK.
        let result = operation(&mut mapping).map_err(|cause| {
                                                super::user::retirement::Failure::mapping(cause,
                                                                                          &mapping)
                                            });
        let waiting = {
            let mut slot = self.user.lock();
            slot.mapping = Some(mapping);
            slot.busy = false;
            core::mem::take(&mut slot.waiting)
        };
        if waiting
        {
            super::context_released();
        }
        result
    }

    pub(super) fn publish_user(&self) -> Result<(), super::user::retirement::Failure>
    {
        self.mapping_turn(|mapping| mapping.publish())
    }

    pub(super) fn retire_user(&self) -> Result<(), super::user::retirement::Failure>
    {
        self.mapping_turn(|mapping| mapping.retire())
    }

    pub(super) fn user_top(&self) -> Result<usize, mac_abi::Status>
    {
        let slot = self.user.lock();
        if slot.busy
        {
            return Err(mac_abi::SERVICE_BUSY);
        }
        slot.mapping
            .as_ref()
            .map(|mapping| mapping.user_top())
            .filter(|top| *top != 0)
            .ok_or(mac_abi::BAD_ABI)
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
        let slot = self.user.lock();
        let mapping = slot.mapping.as_ref()?;
        Some(mapping.user_base()..mapping.user_top())
    }
}
impl Deref for StackBacking
{
    type Target = accounting::Charged<crate::machine::pages::Stack>;

    fn deref(&self) -> &Self::Target
    {
        self.stack
            .get()
            .expect("published ThreadBacking owns a completed unique stack preparation")
            .as_ref()
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
    pub(super) fn finish(self, stack: accounting::Charged<crate::machine::pages::Stack>,
                         space: Option<&super::super::paging::process::Pin>)
                         -> Result<Arc<StackBacking>, mac_abi::Status>
    {
        use super::super::paging::user::{Access, Kind, Mapping};
        if (self.backing.owner.owner().kind() == mac_object_table::domain::DomainKind::Process)
           != space.is_some()
        {
            return Err(mac_abi::ACCESS_DENIED);
        }
        let stack = Arc::try_new(stack).map_err(|_| mac_abi::NO_MEMORY)?;
        let mapping = match space
        {
            Some(space) =>
            {
                Some(Mapping::prepare(space, stack.clone(), Kind::Stack, Access::ReadWrite)?)
            }
            None => None
        };
        self.backing.stack.call_once(|| stack);
        self.backing.user.lock().mapping = mapping;
        Ok(self.backing)
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
            let cpu = stack.top()
                           .checked_sub(stack.bytes() as usize)
                           .and_then(|base| base.checked_add(address - bounds.start))
                           .ok_or(StackPinError::InvalidSpan { address,
                                                               length })?;
            return Ok(Some(StackWaitPin { backing:mac_object_table::retained::Read::new(stack.clone(),context_released),identity,address:cpu }));
        }
    }
    Ok(None)
}

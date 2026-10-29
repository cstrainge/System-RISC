//! Selection owned by the hart whose admitted Control is in sscratch. Thread
//! invocation/account/resource storage stays in ThreadContext. No mutable hart
//! borrow or selection guard spans a foreign call or a nested ECALL.
#![forbid(unsafe_code)]
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use spinning_top::Spinlock;

use crate::{allocator,
            context::ThreadContext,
            machine::{interrupts::with_masked,
                      pages::Stack,
                      traps::{SavedState, TrapFrame}},
            toolbox::handles::OwnerRef};

pub(crate) const IDLE: usize = usize::MAX;
/// Reset-lifetime hardware identity, minted only by checked sscratch selection.
/// The numeric label is diagnostic metadata, never authority to select a hart.
#[derive(Clone, Copy)]
pub(crate) struct HartKey(&'static HartLocal);
impl PartialEq for HartKey
{
    fn eq(&self, other: &Self) -> bool
    {
        core::ptr::eq(self.0, other.0)
    }
}
impl Eq for HartKey
{
}
impl HartKey
{
    pub(crate) fn number(self) -> usize
    {
        self.0.id
    }
}
pub(crate) fn selected_hart() -> HartKey
{
    with_masked(|| {
        match super::selected_hart()
        {
            Some(hart) => HartKey(&hart.local),
            None => super::unadmitted()
        }
    })
}
pub(super) struct HartLocal
{
    pub(super) id: usize,
    active:        Spinlock<Option<Arc<ThreadContext>>>,
    current:       AtomicUsize,
    io:            AtomicBool,
    idle:          Spinlock<Option<Idle>>
}
struct Idle
{
    _stack: Stack,
    cpu:    SavedState
}
impl HartLocal
{
    /// Primary cold -> registry transition cannot silently discard state.
    /// This runs once, masked, before any runtime HartKey/THRD is published.
    pub(super) fn is_cold(&self) -> bool
    {
        self.current.load(Ordering::Relaxed) == IDLE
        && !self.io.load(Ordering::Relaxed)
        && self.active.lock().is_none()
        && self.idle.lock().is_none()
    }

    pub(super) const fn new(id: usize) -> Self
    {
        Self { id,
               active: Spinlock::new(None),
               current: AtomicUsize::new(IDLE),
               io: AtomicBool::new(false),
               idle: Spinlock::new(None) }
    }
}

fn bootstrap_owner(local: &HartLocal) -> bool
{
    crate::machine::paging::firmware_owner_hart() == Some(local.id)
}
fn required() -> &'static HartLocal
{
    match super::selected_local()
    {
        Some(local) => local,
        None => super::unadmitted()
    }
}
pub(crate) fn hart_id() -> Option<usize>
{
    with_masked(|| super::selected_local().map(|local| local.id))
}
pub(crate) fn in_io() -> bool
{
    with_masked(|| super::selected_local().is_none_or(|local| local.io.load(Ordering::Relaxed)))
}
pub(crate) fn current() -> usize
{
    with_masked(|| required().current.load(Ordering::Relaxed))
}
pub(crate) fn select_current(index: usize)
{
    with_masked(|| {
        let current = &required().current;
        if current.load(Ordering::Relaxed) != index
        {
            current.store(index, Ordering::Relaxed);
        }
    });
}
pub(crate) fn current_context() -> crate::context::ContextRef
{
    with_masked(|| {
        let local = required();
        if local.io.load(Ordering::Relaxed)
        {
            if !bootstrap_owner(local)
            {
                super::unadmitted();
            }
            return crate::context::ContextRef::Bootstrap;
        }
        let context = local.active.lock().as_ref().cloned();
        match context
        {
            Some(context) => crate::context::ContextRef::thread(context),
            None if bootstrap_owner(local) => crate::context::ContextRef::Bootstrap,
            None => super::unadmitted()
        }
    })
}
// Only these concrete bounded operations borrow the selected context. No
// generic callback can retain the active guard across author code or ECALLs.
pub(crate) fn authenticate_module(module: u64) -> Result<(), mac_abi::Status>
{
    with_masked(|| {
        let local = super::selected_local().ok_or(mac_abi::WRONG_THREAD)?;
        if local.io.load(Ordering::Relaxed)
        {
            if !bootstrap_owner(local)
            {
                return Err(mac_abi::WRONG_THREAD);
            }
            return crate::context::authenticate_bootstrap_module(module);
        }
        match local.active.lock().as_ref()
        {
            Some(context) => context.authenticate_module(module),
            None if bootstrap_owner(local) => crate::context::authenticate_bootstrap_module(module),
            None => Err(mac_abi::WRONG_THREAD)
        }
    })
}
pub(crate) fn module_owner(module: u64) -> Result<OwnerRef, mac_abi::Status>
{
    with_masked(|| {
        let local = super::selected_local().ok_or(mac_abi::WRONG_THREAD)?;
        if local.io.load(Ordering::Relaxed)
        {
            if !bootstrap_owner(local)
            {
                return Err(mac_abi::WRONG_THREAD);
            }
            return crate::context::bootstrap_module_owner(module);
        }
        match local.active.lock().as_ref()
        {
            Some(context) => context.module_owner(module),
            None if bootstrap_owner(local) => crate::context::bootstrap_module_owner(module),
            None => Err(mac_abi::WRONG_THREAD)
        }
    })
}
pub(crate) fn module_callback_allowed(module: u64, address: usize) -> Result<(), mac_abi::Status>
{
    with_masked(|| {
        let local = super::selected_local().ok_or(mac_abi::WRONG_THREAD)?;
        if local.io.load(Ordering::Relaxed)
        {
            if !bootstrap_owner(local)
            {
                return Err(mac_abi::WRONG_THREAD);
            }
            return crate::context::bootstrap_module_callback_allowed(module, address);
        }
        match local.active.lock().as_ref()
        {
            Some(context) => context.module_callback_allowed(module, address),
            None if bootstrap_owner(local) =>
            {
                crate::context::bootstrap_module_callback_allowed(module, address)
            }
            None => Err(mac_abi::WRONG_THREAD)
        }
    })
}
pub(crate) fn critical_active() -> bool
{
    with_masked(|| {
        let Some(local) = super::selected_local()
        else
        {
            return true;
        };
        if local.io.load(Ordering::Relaxed)
        {
            return !bootstrap_owner(local) || crate::context::bootstrap_critical_active();
        }
        match local.active.lock().as_ref()
        {
            Some(context) => context.critical_active(),
            None => !bootstrap_owner(local) || crate::context::bootstrap_critical_active()
        }
    })
}
pub(crate) fn reserve_allocation(bytes: u64) -> Result<allocator::Reservation, allocator::Error>
{
    with_masked(|| {
        let local = super::selected_local().ok_or(allocator::Error::ContextUnavailable)?;
        if local.io.load(Ordering::Relaxed)
        {
            if !bootstrap_owner(local)
            {
                return Err(allocator::Error::ContextUnavailable);
            }
            return crate::context::reserve_bootstrap_allocation(bytes);
        }
        let active = local.active.lock();
        match active.as_ref()
        {
            Some(context) => context.reserve_allocation(bytes),
            None if bootstrap_owner(local) => crate::context::reserve_bootstrap_allocation(bytes),
            None => Err(allocator::Error::ContextUnavailable)
        }
    })
}
pub(crate) fn reserve_replacement(receipt: &allocator::Receipt, bytes: u64)
                                  -> Result<allocator::Reservation, allocator::Error>
{
    with_masked(|| {
        let local = super::selected_local().ok_or(allocator::Error::ContextUnavailable)?;
        if local.io.load(Ordering::Relaxed)
        {
            if !bootstrap_owner(local)
            {
                return Err(allocator::Error::ContextUnavailable);
            }
            return crate::context::reserve_bootstrap_replacement(receipt, bytes);
        }
        let active = local.active.lock();
        match active.as_ref()
        {
            Some(context) => context.reserve_replacement(receipt, bytes),
            None if bootstrap_owner(local) =>
            {
                crate::context::reserve_bootstrap_replacement(receipt, bytes)
            }
            None => Err(allocator::Error::ContextUnavailable)
        }
    })
}
/// Scheduler -> local active is the only held-lock ordering. An unchanged
/// context performs no Arc operation; any old pin drops after the guard leaves.
pub(crate) fn activate_context(context: Option<&Arc<ThreadContext>>)
{
    with_masked(|| {
        let mut active = required().active.lock();
        let unchanged = match (active.as_ref(), context)
        {
            (Some(current), Some(next)) => Arc::ptr_eq(current, next),
            (None, None) => true,
            _ => false
        };
        if unchanged
        {
            return;
        }
        let previous = core::mem::replace(&mut *active, context.cloned());
        drop(active);
        drop(previous);
    });
}
pub(crate) fn io_enter() -> Result<bool, crate::state::AccessError>
{
    with_masked(|| {
        let local = super::selected_local().ok_or(crate::state::AccessError::WrongOwner)?;
        // Opaque firmware remains owned by the sole admitted bootstrap hart.
        if !bootstrap_owner(local)
        {
            return Err(crate::state::AccessError::WrongOwner);
        }
        Ok(local.io.swap(true, Ordering::Relaxed))
    })
}
pub(crate) fn io_leave(previous: bool)
{
    with_masked(|| required().io.store(previous, Ordering::Relaxed));
}
pub(crate) fn install_idle(stack: Stack, cpu: SavedState) -> Result<(), mac_abi::Status>
{
    let mut incoming = Some(Idle { _stack: stack,
                                   cpu });
    let result = with_masked(|| {
        let local = super::selected_local().ok_or(mac_abi::WRONG_THREAD)?;
        let mut idle = local.idle.lock();
        if idle.is_some()
        {
            return Err(mac_abi::SERVICE_BUSY);
        }
        *idle = incoming.take();
        Ok(())
    });
    // A refused installation retains the original idle and drops new backing
    // outside its metadata guard.
    drop(incoming);
    result
}
pub(crate) fn idle_frame() -> *mut TrapFrame
{
    with_masked(|| {
        let idle = required().idle.lock();
        let Some(idle) = idle.as_ref()
        else
        {
            super::super::entry_lost("scheduler bootstrap did not install this hart's idle \
                                      backing");
        };
        super::publish(&idle.cpu);
        idle.cpu.frame()
    })
}

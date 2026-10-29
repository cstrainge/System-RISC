//! Selection owned by the hart whose admitted Control is in sscratch. Thread
//! invocation/account/resource storage stays in ThreadContext. No mutable hart
//! borrow or selection guard spans a foreign call or a nested ECALL.
#![forbid(unsafe_code)]
use alloc::sync::Arc;
use core::{fmt,
           sync::atomic::{AtomicBool, AtomicUsize, Ordering}};

use spinning_top::Spinlock;

use crate::{allocator,
            context::ThreadContext,
            machine::{interrupts::with_masked, scheduler::SystemIdle, traps::TrapFrame},
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
    idle_claimed:  AtomicBool,
    idle:          Spinlock<Option<SystemIdle>>
}
/// Unique preparation lease for an actual permanent registry row. It is not
/// execution or current-CPU authority and exposes no numeric-ID constructor.
/// Kept LAST in the shared SystemIdleCore so claim release follows disposal of
/// its other fields. Automatic Send/Sync reflects only this permanent atomic/
/// locked reference; PreparedEntry's boot authority remains !Send/!Sync.
pub(crate) struct IdleTarget
{
    local: &'static HartLocal
}
impl IdleTarget
{
    fn claim(local: &'static HartLocal) -> Result<Self, mac_abi::Status>
    {
        local.idle_claimed
             .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
             .map_err(|_| mac_abi::SERVICE_BUSY)?;
        Ok(Self { local })
    }

    /// Diagnostic label only. No caller can use it to issue/select a target.
    pub(crate) fn number(&self) -> usize
    {
        self.local.id
    }
}
impl Drop for IdleTarget
{
    fn drop(&mut self)
    {
        // A live Store/local owner retains the same core, and therefore this
        // target. Dropping a failed install result alone cannot mint a duplicate.
        self.local.idle_claimed.store(false, Ordering::Release);
    }
}
pub(crate) enum IdlePreparationFailure
{
    Admission(mac_abi::Status),
    Prepare
    {
        hart:   usize,
        status: mac_abi::Status
    },
    Install
    {
        status: mac_abi::Status,
        idle:   SystemIdle
    }
}
impl fmt::Display for IdlePreparationFailure
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Admission(status) =>
            {
                write!(out, "System idle preparation caller refused: status {status}")
            }
            Self::Prepare { hart,
                            status } =>
            {
                write!(out, "System idle preparation for hart {hart} refused: status {status}")
            }
            Self::Install { status,
                            idle } =>
            {
                write!(out,
                       "System idle installation for hart {} thread {} refused: status {status}; \
                        canonical owner retained",
                       idle.target().number(),
                       idle.context().identity().thread.0)
            }
        }
    }
}
/// Only actual primary bootstrap may initialize all affine idle endpoints.
/// No lock, mutable local borrow or scope guard escapes into the constructor.
fn idle_preparation_owner() -> Result<(), mac_abi::Status>
{
    let hart = super::selected_hart().ok_or(mac_abi::WRONG_THREAD)?;
    let local = &hart.local;
    if !bootstrap_owner(local)
       || local.io.load(Ordering::Relaxed)
       || local.current.load(Ordering::Relaxed) != IDLE
    {
        return Err(mac_abi::WRONG_THREAD);
    }
    let active = local.active.lock().is_some();
    if active
    {
        return Err(mac_abi::WRONG_THREAD);
    }
    Ok(())
}
/// Admission is per immutable topology row. Already-installed owners survive a
/// later row's refusal; uninstalled failures return their real owner to startup.
pub(crate) fn prepare_idles(mut prepare: impl FnMut(IdleTarget) -> Result<SystemIdle, mac_abi::Status>)
                            -> Result<(), IdlePreparationFailure>
{
    with_masked(idle_preparation_owner).map_err(IdlePreparationFailure::Admission)?;
    let registry =
        super::REGISTRY.get().ok_or(IdlePreparationFailure::Admission(mac_abi::NOT_SUPPORTED))?;
    for row in registry.rows.as_slice()
    {
        let installed = with_masked(|| row.local.idle.lock().is_some());
        if installed
        {
            continue;
        }
        let hart = row.local.id;
        let target = IdleTarget::claim(&row.local).map_err(|status| {
                                                      IdlePreparationFailure::Prepare { hart,
                                                                                        status }
                                                  })?;
        // No active/idle/registry lock crosses allocation or the resident
        // constructor. The opaque lease is retained by its actual shared core.
        let idle = prepare(target).map_err(|status| {
                                      IdlePreparationFailure::Prepare { hart,
                                                                        status }
                                  })?;
        // A constructor must return the very row capability it consumed.
        // Returning another retained owner cannot establish this row's readiness.
        if !core::ptr::eq(idle.target().local, &row.local)
        {
            return Err(IdlePreparationFailure::Install { status: mac_abi::BAD_ABI,
                                                         idle });
        }
        install_idle(idle).map_err(|(status, idle)| {
                              IdlePreparationFailure::Install { status,
                                                                idle }
                          })?;
    }
    Ok(())
}
impl HartLocal
{
    /// Primary cold -> registry transition cannot silently discard state.
    /// This runs once, masked, before any runtime HartKey/THRD is published.
    pub(super) fn is_cold(&self) -> bool
    {
        let active_empty = self.active.lock().is_none();
        let idle_empty = self.idle.lock().is_none();
        self.current.load(Ordering::Relaxed) == IDLE
        && !self.io.load(Ordering::Relaxed)
        && !self.idle_claimed.load(Ordering::Acquire)
        && active_empty
        && idle_empty
    }

    pub(super) const fn new(id: usize) -> Self
    {
        Self { id,
               active: Spinlock::new(None),
               current: AtomicUsize::new(IDLE),
               io: AtomicBool::new(false),
               idle_claimed: AtomicBool::new(false),
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
pub(crate) fn install_idle(idle: SystemIdle) -> Result<(), (mac_abi::Status, SystemIdle)>
{
    with_masked(|| {
        if let Err(status) = idle_preparation_owner()
        {
            return Err((status, idle));
        }
        let local = idle.target().local;
        if !local.idle_claimed.load(Ordering::Acquire)
        {
            return Err((mac_abi::BAD_ABI, idle));
        }
        let mut destination = local.idle.lock();
        if destination.is_some()
        {
            return Err((mac_abi::SERVICE_BUSY, idle));
        }
        // The target supplies the destination directly. There is no numeric
        // CPU lookup and no way to install a core into another row's slot.
        *destination = Some(idle);
        Ok(())
    })
}
/// Select the genuine affine System thread. Same-context idle->idle performs
/// no Arc operation; old pins are dropped outside both active and idle guards.
pub(crate) fn activate_idle_context()
{
    with_masked(|| {
        let local = required();
        let current = {
            let active = local.active.lock();
            active.as_ref().map(Arc::as_ptr)
        };
        let incoming = {
            let idle = local.idle.lock();
            let Some(idle) = idle.as_ref()
            else
            {
                super::super::entry_lost("scheduler handoff has no canonical System idle owner");
            };
            if current == Some(Arc::as_ptr(idle.context()))
            {
                return;
            }
            idle.context().clone()
        };
        let previous = {
            let mut active = local.active.lock();
            active.replace(incoming)
        };
        drop(previous);
    });
}
pub(crate) fn idle_frame() -> *mut TrapFrame
{
    with_masked(|| {
        let idle = required().idle.lock();
        let Some(idle) = idle.as_ref()
        else
        {
            super::super::entry_lost("scheduler bootstrap did not install this hart's canonical \
                                      idle owner");
        };
        super::publish(idle.cpu());
        idle.cpu().frame()
    })
}

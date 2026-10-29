//! Selection owned by the hart whose admitted Control is in sscratch. Thread
//! invocation/account/resource storage stays in ThreadContext. No mutable hart
//! borrow or selection guard spans a foreign call or a nested ECALL.
#![forbid(unsafe_code)]
use alloc::sync::Arc;
use core::{fmt,
           sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering}};

use spinning_top::Spinlock;

use crate::{allocator,
            context::ThreadContext,
            machine::{interrupts::with_masked, scheduler::SystemIdle, traps::TrapFrame},
            toolbox::handles::OwnerRef};

pub(crate) mod policy;
pub(crate) mod dispatch;

pub(crate) const IDLE: usize = usize::MAX;
/// Reset-lifetime row binding for prepared metadata. Unlike HartKey, this is
/// not proof that the caller executes on the row or may dispatch work there.
#[derive(Clone, Copy)]
pub(crate) struct HartBinding(pub(super) &'static super::Hart);
impl HartBinding
{
    pub(crate) fn number(self) -> usize
    {
        self.0.local.id
    }

    pub(crate) fn is_primary(self) -> bool
    {
        super::REGISTRY.get().is_some_and(|registry| {
                                 core::ptr::eq(self.0, &registry.rows.as_slice()[registry.primary])
                             })
    }

    pub(crate) fn ordinal(self) -> usize
    {
        self.0.ordinal
    }

    pub(crate) fn matches(self, selected: HartKey) -> bool
    {
        core::ptr::eq(self.0, selected.0)
    }
}
pub(crate) fn prepared_harts() -> Option<impl ExactSizeIterator<Item = HartBinding>>
{
    Some(super::REGISTRY.get()?.rows.as_slice().iter().map(HartBinding))
}
/// Reset-lifetime hardware identity, minted only by checked sscratch selection.
/// The numeric label is diagnostic metadata, never authority to select a hart.
#[derive(Clone, Copy)]
pub(crate) struct HartKey(&'static super::Hart);
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
    pub(crate) fn binding(self) -> HartBinding
    {
        HartBinding(self.0)
    }

    pub(crate) fn number(self) -> usize
    {
        self.0.local.id
    }

    pub(crate) fn ordinal(self) -> usize
    {
        self.0.ordinal
    }

    /// Compare the sealed row itself with the admitted primary row. Hardware
    /// numbers remain diagnostic metadata, never a placement authorization.
    pub(crate) fn is_primary(self) -> bool
    {
        super::REGISTRY.get().is_some_and(|registry| {
                                 core::ptr::eq(self.0, &registry.rows.as_slice()[registry.primary])
                             })
    }

    /// Selected-state residency boundary, not accumulated per-thread CPU time.
    pub(crate) fn resume_activity(self, idle: bool)
    {
        self.0.activity.resume(idle);
    }

    pub(crate) fn dispatch_eligible(self) -> bool
    {
        self.0.phase.load(Ordering::Acquire) == super::DISPATCH_ELIGIBLE
    }

    /// Draining peers must capture and relinquish their real running claim;
    /// lack of permission to select authors cannot mean resume the old author.
    pub(crate) fn requires_schedule(self) -> bool
    {
        matches!(self.0.phase.load(Ordering::Acquire), super::DISPATCH_ELIGIBLE | super::DRAINING)
    }
}
pub(crate) fn selected_hart() -> HartKey
{
    with_masked(|| {
        match super::selected_hart()
        {
            Some(hart) => HartKey(hart),
            None => super::unadmitted()
        }
    })
}
/// Reset-lifetime terminal ownership tied to the actual sealed selection.
/// Genuine COLD may claim before the registry, without becoming a runtime key.
/// Its pointer stays private: labels and caller-chosen numbers cannot create a
/// winner, and no factory can reconstruct execution authority from this cell.
pub(in crate::machine) struct TerminalOwnership(AtomicPtr<HartLocal>);
pub(in crate::machine) enum TerminalClaim
{
    Winner(TerminalWinner),
    Recursive,
    Peer(HartKey)
}
/// Winner affinity is valid for the actual current hot or cold selection.
/// It grants terminal stop only, never runtime/firmware execution authority.
/// A boot owner cannot transfer this native claim to another CPU.
pub(in crate::machine) struct TerminalWinner
{
    local: &'static HartLocal,
    _cpu:  core::marker::PhantomData<*mut ()>
}
impl TerminalWinner
{
    pub(in crate::machine) fn number(&self) -> usize
    {
        self.local.id
    }

    pub(in crate::machine) fn is_current(&self) -> bool
    {
        with_masked(|| {
            super::terminal_local().is_some_and(|local| core::ptr::eq(local, self.local))
        })
    }
}
impl TerminalOwnership
{
    pub(in crate::machine) const fn new() -> Self
    {
        Self(AtomicPtr::new(core::ptr::null_mut()))
    }

    /// Claims only for the genuine current CPU. Never reset/release this owner:
    /// terminal execution abandons all contexts and backing until machine reset.
    pub(in crate::machine) fn claim(&self) -> TerminalClaim
    {
        with_masked(|| {
            let local = match super::terminal_local()
            {
                Some(local) => local,
                None => super::unadmitted()
            };
            let identity = core::ptr::from_ref(local).cast_mut();
            match self.0.compare_exchange(core::ptr::null_mut(),
                                          identity,
                                          Ordering::AcqRel,
                                          Ordering::Acquire)
            {
                Ok(_) =>
                {
                    TerminalClaim::Winner(TerminalWinner { local,
                                                           _cpu: core::marker::PhantomData })
                }
                Err(owner) if owner == identity => TerminalClaim::Recursive,
                // A cold winner never returns and no peer may enter S before
                // registry publication. Thus a distinct legitimate loser has
                // an actual runtime key; this never invents a cold HartKey.
                Err(_) => TerminalClaim::Peer(selected_hart())
            }
        })
    }
}

pub(super) struct HartLocal
{
    pub(super) id: usize,
    active:        Spinlock<Option<Arc<ThreadContext>>>,
    current:       AtomicUsize,
    io:            AtomicBool,
    idle_claimed:  AtomicBool,
    idle:          Spinlock<Option<SystemIdle>>,
    policy:        policy::State
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
        && !self.policy.active()
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
               idle: Spinlock::new(None),
               policy: policy::State::new() }
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

/// Preparation examines the same canonical affine idle that the starting CPU
/// will select. No numeric thread/hart label substitutes for its owned target.
pub(super) fn idle_prepared(local: &'static HartLocal) -> bool
{
    if !local.idle_claimed.load(Ordering::Acquire)
    {
        return false;
    }
    let idle = local.idle.lock();
    idle.as_ref().is_some_and(|idle| core::ptr::eq(idle.target().local, local))
}
pub(super) fn idle_matches_profile(local: &'static HartLocal, vector_bytes: usize) -> bool
{
    let idle = local.idle.lock();
    idle.as_ref().is_some_and(|idle| {
                     core::ptr::eq(idle.target().local, local)
                     && idle.cpu().matches_vector_profile(vector_bytes)
                 })
}
/// Called only through authenticated private Starting entry with all local
/// interrupts masked. It cannot fall back to Bootstrap/None and does not
/// publish ordinary dispatch permission. The canonical Store/local core pins
/// both the real context and CPU backing until reset.
pub(super) fn initialize_starting_idle(local: &'static HartLocal)
                                       -> Result<*mut TrapFrame, mac_abi::Status>
{
    if local.current.load(Ordering::Relaxed) != IDLE || local.io.load(Ordering::Relaxed)
    {
        return Err(mac_abi::BAD_ABI);
    }
    let (context, frame) = {
        let idle = local.idle.lock();
        let idle = idle.as_ref().ok_or(mac_abi::BAD_ABI)?;
        if !core::ptr::eq(idle.target().local, local)
        {
            return Err(mac_abi::BAD_ABI);
        }
        (idle.context().clone(), idle.cpu().frame())
    };
    let mut active = local.active.lock();
    if active.is_some()
    {
        return Err(mac_abi::BAD_ABI);
    }
    *active = Some(context);
    Ok(frame)
}

/// Startup SSIP acknowledgment must come from this installed affine idle,
/// never merely a pointer copied into Control.frames. No lock is nested and
/// the reset-lifetime canonical core pins the compared context/backing.
pub(super) fn admitted_idle_capture(local: &'static HartLocal, frame: *mut TrapFrame) -> bool
{
    if local.current.load(Ordering::Relaxed) != IDLE || local.io.load(Ordering::Relaxed)
    {
        return false;
    }
    let expected = {
        let idle = local.idle.lock();
        let Some(idle) = idle.as_ref()
        else
        {
            return false;
        };
        if !core::ptr::eq(idle.target().local, local) || idle.cpu().frame() != frame
        {
            return false;
        }
        Arc::as_ptr(idle.context())
    };
    let active = local.active.lock();
    active.as_ref().is_some_and(|context| Arc::as_ptr(context) == expected)
}

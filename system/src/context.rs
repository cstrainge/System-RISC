//! Execution identity. Active classic state belongs to its resident services.
//! Identity must come
//! from the kernel scheduler, not from untrusted request fields.
#[cfg(any(target_arch = "riscv64", test))]
mod selection;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadId(pub u64);
#[cfg(target_arch = "riscv64")]
pub const BOOT_THREAD: ThreadId = ThreadId(1);
static FINDER_APP: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub fn finder_app() -> AppId
{
    AppId(FINDER_APP.load(core::sync::atomic::Ordering::Acquire))
}
#[cfg(target_arch = "riscv64")]
pub(crate) fn set_finder_app(app: u64)
{
    FINDER_APP.store(app, core::sync::atomic::Ordering::Release);
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn replace_finder_app(old: u64, owner: &crate::toolbox::handles::OwnerRef)
                                 -> Result<(), mac_abi::Status>
{
    if owner.owner().kind() != mac_object_table::domain::DomainKind::Process || !owner.accepting()
    {
        return Err(mac_abi::ACCESS_DENIED);
    }
    FINDER_APP.compare_exchange(old,
                                owner.owner().incarnation(),
                                core::sync::atomic::Ordering::AcqRel,
                                core::sync::atomic::Ordering::Acquire)
              .map(|_| ())
              .map_err(|_| mac_abi::SERVICE_BUSY)
}
#[cfg(target_arch = "riscv64")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionContext
{
    pub app:    AppId,
    pub thread: ThreadId
}
#[cfg(target_arch = "riscv64")]
pub fn bootstrap_thread() -> ThreadId
{
    // Used only to admit primary-hart bootstrap. Scheduled service calls obtain
    // their actual identities from current_thread_context instead.
    let hart = crate::machine::hart_id();
    if hart == 0 { BOOT_THREAD } else { ThreadId(0) }
}

/// Scheduler wake identity is typed; a full native transfer ID is never
/// guessed from the bit range of another manager's native ID.
#[cfg(any(target_arch = "riscv64", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeferredIo
{
    DriverJob(u64),
    FileTransfer(u64)
}
#[cfg(any(target_arch = "riscv64", test))]
impl DeferredIo
{
    fn valid(self) -> bool
    {
        match self
        {
            Self::DriverJob(id) => id != 0,
            Self::FileTransfer(id) => id != 0
        }
    }
}

/// A borrowed event readiness pass, filled in preallocated scheduler scratch.
#[cfg(target_arch = "riscv64")]
pub(crate) struct EventWaiter
{
    pub(crate) app:   u64,
    pub(crate) mask:  u16,
    pub(crate) ready: bool
}

/// Accounting belongs to an OS thread and survives hart migration with it.
/// Independent app calls must never increment each other's nesting counters.
#[cfg(any(target_arch = "riscv64", test))]
pub struct ThreadTrapState
{
    depth:            core::sync::atomic::AtomicUsize,
    #[cfg(target_arch = "riscv64")]
    reported:         core::sync::atomic::AtomicBool,
    #[cfg(target_arch = "riscv64")]
    refusal_reported: core::sync::atomic::AtomicBool,
    deferred:         spinning_top::Spinlock<Option<DeferredIo>>
}
#[cfg(any(target_arch = "riscv64", test))]
impl ThreadTrapState
{
    pub const fn new() -> Self
    {
        Self { depth:
                   core::sync::atomic::AtomicUsize::new(0),
               #[cfg(target_arch = "riscv64")]
               reported:
                   core::sync::atomic::AtomicBool::new(false),
               #[cfg(target_arch = "riscv64")]
               refusal_reported:
                   core::sync::atomic::AtomicBool::new(false),
               deferred:                                         spinning_top::Spinlock::new(None) }
    }

    pub fn enter(&self) -> TrapScope<'_>
    {
        let depth = self.depth.fetch_add(1, core::sync::atomic::Ordering::Relaxed) + 1;
        TrapScope { state: self,
                    depth }
    }

    pub fn defer(&self, job: DeferredIo) -> Result<(), mac_abi::Status>
    {
        use core::sync::atomic::Ordering::Relaxed;
        if !job.valid()
        {
            return Err(mac_abi::BAD_ARGUMENT);
        }
        #[cfg(target_arch = "riscv64")]
        if !crate::machine::traps::can_defer_io()
        {
            return Err(mac_abi::WRONG_THREAD);
        }
        if self.depth.load(Relaxed) != 1
        {
            return Err(mac_abi::WRONG_THREAD);
        }
        use mac_object_table::shared::InvocationGate as _;
        crate::toolbox::handles::InvocationGate::run(|| {
            let mut deferred = self.deferred.lock();
            if deferred.is_some()
            {
                return Err(mac_abi::SERVICE_BUSY);
            }
            *deferred = Some(job);
            Ok(())
        })
    }

    pub fn take_deferred(&self) -> Option<DeferredIo>
    {
        use mac_object_table::shared::InvocationGate as _;
        crate::toolbox::handles::InvocationGate::run(|| self.deferred.lock().take())
    }

    #[cfg(target_arch = "riscv64")]
    pub fn nesting_refusal_reported(&self) -> bool
    {
        self.refusal_reported.swap(true, core::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(target_arch = "riscv64")]
    pub fn nesting_reported(&self) -> bool
    {
        self.reported.swap(true, core::sync::atomic::Ordering::Relaxed)
    }
}
#[cfg(any(target_arch = "riscv64", test))]
pub struct TrapScope<'a>
{
    state:     &'a ThreadTrapState,
    pub depth: usize
}
#[cfg(any(target_arch = "riscv64", test))]
impl Drop for TrapScope<'_>
{
    fn drop(&mut self)
    {
        self.state.depth.fetch_sub(1, core::sync::atomic::Ordering::Relaxed);
    }
}
#[cfg(target_arch = "riscv64")]
enum ThreadOwner
{
    Process
    {
        owner: crate::toolbox::handles::OwnerRef, image: crate::images::Binding
    },
    Module
    {
        owner:     crate::toolbox::handles::OwnerRef,
        image:     crate::images::Binding,
        frame:     crate::images::ModuleFrame,
        resources: crate::kernel::ResourceSelection
    }
}
#[cfg(target_arch = "riscv64")]
impl ThreadOwner
{
    fn owner(&self) -> &crate::toolbox::handles::OwnerRef
    {
        match self
        {
            Self::Process { owner,
                            .. }
            | Self::Module { owner,
                           .. } => owner
        }
    }

    fn image(&self) -> crate::images::Binding
    {
        match self
        {
            Self::Process { image,
                            .. }
            | Self::Module { image,
                           .. } => *image
        }
    }
}
#[cfg(target_arch = "riscv64")]
pub struct ThreadContext
{
    app:               core::sync::atomic::AtomicU64,
    module_worker:     core::sync::atomic::AtomicBool,
    critical_depth:    core::sync::atomic::AtomicUsize,
    thread:            core::sync::atomic::AtomicU64,
    owner:             spinning_top::Spinlock<Option<ThreadOwner>>,
    allocation:
        spinning_top::Spinlock<Option<crate::machine::scheduler::accounting::AllocationAuthority>>,
    allocation_scope:  selection::ScopedSelection<crate::allocator::Target>,
    worker_allocation: spinning_top::Spinlock<Option<crate::allocator::Target>>,
    worker_subject:    spinning_top::Spinlock<Option<crate::drivers::jobs::subjects::Subject>>,
    resources:         selection::ScopedSelection<crate::kernel::ResourceSelection>,
    io_subject:        selection::ScopedSelection<crate::drivers::jobs::subjects::Subject>,
    module:            selection::ScopedSelection<crate::images::ModuleFrame>,
    pub traps:         ThreadTrapState,
    pub definitions:   DefinitionInvocations
}
#[cfg(target_arch = "riscv64")]
impl ThreadContext
{
    pub const fn new() -> Self
    {
        Self { app:               core::sync::atomic::AtomicU64::new(0),
               module_worker:     core::sync::atomic::AtomicBool::new(false),
               critical_depth:    core::sync::atomic::AtomicUsize::new(0),
               thread:            core::sync::atomic::AtomicU64::new(1),
               owner:             spinning_top::Spinlock::new(None),
               allocation:        spinning_top::Spinlock::new(None),
               allocation_scope:  selection::ScopedSelection::new(BOOT_THREAD),
               worker_allocation: spinning_top::Spinlock::new(None),
               worker_subject:    spinning_top::Spinlock::new(None),
               resources:         selection::ScopedSelection::new(BOOT_THREAD),
               io_subject:        selection::ScopedSelection::new(BOOT_THREAD),
               module:            selection::ScopedSelection::new(BOOT_THREAD),
               traps:             ThreadTrapState::new(),
               definitions:       DefinitionInvocations::new() }
    }

    pub fn identity(&self) -> ExecutionContext
    {
        use core::sync::atomic::Ordering::Relaxed;
        ExecutionContext { app:    AppId(self.app.load(Relaxed)),
                           thread: ThreadId(self.thread.load(Relaxed)) }
    }

    /// Admission initializes this before publication; reclamation clears it
    /// only after the context is quiescent. Current-context reads never need
    /// the scheduler lock, including its completion-before-park path.
    pub(crate) fn image(&self) -> Option<crate::images::Binding>
    {
        use mac_object_table::shared::InvocationGate as _;
        crate::toolbox::handles::InvocationGate::run(|| {
            self.owner.lock().as_ref().map(ThreadOwner::image)
        })
    }

    pub(crate) fn assign(&self, owner: crate::toolbox::handles::OwnerRef, thread: u64,
                         image: crate::images::Binding,
                         allocation: &crate::machine::scheduler::accounting::AllocationAuthority)
                         -> Result<(), mac_abi::Status>
    {
        use core::sync::atomic::Ordering::Relaxed;

        use mac_object_table::shared::InvocationGate as _;
        if thread == 0
           || allocation.owner() != owner.owner()
           || image.owner() != owner.owner().incarnation()
           || owner.owner().kind() != mac_object_table::domain::DomainKind::Process
        {
            return Err(mac_abi::BAD_ARGUMENT);
        }
        if !owner.accepting()
        {
            return Err(mac_abi::SERVICE_BUSY);
        }
        crate::toolbox::handles::InvocationGate::run(|| {
            let mut binding = self.owner.lock();
            if binding.is_some() || self.allocation.lock().is_some()
            {
                return Err(mac_abi::SERVICE_BUSY);
            }
            self.module.reset(ThreadId(thread));
            self.resources.reset(ThreadId(thread));
            self.io_subject.reset(ThreadId(thread));
            self.allocation_scope.reset(ThreadId(thread));
            *self.allocation.lock() = Some(allocation.clone());
            self.app.store(owner.owner().incarnation(), Relaxed);
            self.thread.store(thread, Relaxed);
            *binding = Some(ThreadOwner::Process { owner,
                                                   image });
            Ok(())
        })
    }

    pub(crate) fn assign_module(&self, owner: crate::toolbox::handles::OwnerRef, thread: u64,
                                execution: &crate::images::ModuleExecution,
                                allocation: &crate::machine::scheduler::accounting::AllocationAuthority,
                                subject: crate::drivers::jobs::subjects::Subject)
    {
        // The sealed worker preparation checked owner/account/execution before
        // publication; assigning its issued THRD identity cannot fail.
        self.module.reset(ThreadId(thread));
        self.resources.reset(ThreadId(thread));
        self.io_subject.reset(ThreadId(thread));
        self.allocation_scope.reset(ThreadId(thread));
        *self.allocation.lock() = Some(allocation.clone());
        *self.owner.lock() =
            Some(ThreadOwner::Module { owner,
                                       image: execution.binding(),
                                       frame: execution.frame(),
                                       resources:
                                           crate::kernel::module_resource_selection(execution) });
        *self.worker_subject.lock() = Some(subject);
        self.module_worker.store(true, core::sync::atomic::Ordering::Relaxed);
        self.thread.store(thread, core::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn is_module_worker(&self) -> bool
    {
        self.module_worker.load(core::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn install_worker_allocation(&self, target: crate::allocator::Target)
                                            -> Result<(), mac_abi::Status>
    {
        use mac_object_table::shared::InvocationGate as _;
        let operation_target = matches!(target, crate::allocator::Target::Operation(
            crate::allocation::OperationTarget::Owner { .. }
        ));
        if !self.is_module_worker() || !operation_target
        {
            return Err(mac_abi::ACCESS_DENIED);
        }
        crate::toolbox::handles::InvocationGate::run(|| {
            if self.owner
                   .lock()
                   .as_ref()
                   .is_none_or(|owner| owner.owner().owner() != target.owner())
            {
                return Err(mac_abi::ACCESS_DENIED);
            }
            *self.worker_allocation.lock() = Some(target);
            Ok(())
        })
    }

    /// A callback installed over an interrupted worker is a module invocation,
    /// not permission to park or borrow that worker's execution identity.
    pub(crate) fn is_worker_body(&self) -> bool
    {
        self.is_module_worker() && self.module.borrow(|frame| frame.is_none())
    }

    pub(crate) fn critical_active(&self) -> bool
    {
        self.critical_depth.load(core::sync::atomic::Ordering::Relaxed) != 0
    }

    pub(crate) fn worker_io_subject(&self) -> Option<crate::drivers::jobs::subjects::Subject>
    {
        use mac_object_table::shared::InvocationGate as _;
        crate::toolbox::handles::InvocationGate::run(|| self.worker_subject.lock().clone())
    }

    fn default_resources(&self) -> Option<crate::kernel::ResourceSelection>
    {
        use mac_object_table::shared::InvocationGate as _;
        crate::toolbox::handles::InvocationGate::run(|| {
            match self.owner.lock().as_ref()
            {
                Some(ThreadOwner::Module { resources,
                                           .. }) => Some(*resources),
                _ => None
            }
        })
    }

    pub(crate) fn release(&self)
    {
        use mac_object_table::shared::InvocationGate as _;
        crate::toolbox::handles::InvocationGate::run(|| {
            let owner = self.owner.lock().take();
            let allocation = self.allocation.lock().take();
            let worker_allocation = self.worker_allocation.lock().take();
            let worker_subject = self.worker_subject.lock().take();
            self.module.reset(ThreadId(0));
            self.resources.reset(ThreadId(0));
            self.io_subject.reset(ThreadId(0));
            self.allocation_scope.reset(ThreadId(0));
            self.module_worker.store(false, core::sync::atomic::Ordering::Relaxed);
            self.app.store(0, core::sync::atomic::Ordering::Relaxed);
            self.thread.store(0, core::sync::atomic::Ordering::Relaxed);
            // The allocation pin leaves with the quiescent context before
            // process account readiness is checked. No owner lock is held
            // over its original-account retirement notification.
            drop(worker_subject);
            drop(worker_allocation);
            drop(allocation);
            drop(owner);
        });
    }

    /// Called only by the scheduler while ACTIVE is borrowed under the native
    /// interrupt gate. No owner lookup, retained reader, backing allocation or
    /// AccountUse destruction is part of this reservation path.
    pub(crate) fn reserve_allocation(
        &self, bytes: u64)
        -> Result<crate::allocator::Reservation, crate::allocator::Error>
    {
        self.with_allocation_selection(|selection| selection.reserve(bytes))
    }

    /// Offer only the borrowed current capability. The allocation's original
    /// receipt decides whether it is the same canonical account and retains
    /// its own class/refund destination; current identity cannot retarget it.
    pub(crate) fn reserve_replacement(
        &self, receipt: &crate::allocator::Receipt, bytes: u64)
        -> Result<crate::allocator::Reservation, crate::allocator::Error>
    {
        self.with_allocation_selection(|selection| receipt.reserve_replacement(selection, bytes))
    }

    fn with_allocation_selection<R>(&self,
                                    operation: impl FnOnce(crate::allocator::Selection<'_>)
                                           -> Result<R, crate::allocator::Error>)
                                    -> Result<R, crate::allocator::Error>
    {
        self.allocation_scope.borrow(|selected| {
            if let Some(selected) = selected
            {
                return operation(selected.selection());
            }
            let worker_allocation = self.worker_allocation.lock();
            if let Some(target) = worker_allocation.as_ref() {
                return operation(target.selection());
            }
            let allocation = self.allocation.lock();
            let account = allocation.as_ref().ok_or(crate::allocator::Error::ContextUnavailable)?;
            operation(crate::allocator::Selection::Owner {
                account, class: mac_kernel_model::policy::MemoryClass::Kernel
            })
        })
    }

    fn selected_allocation(&self) -> AllocationSelection
    {
        self.allocation_scope.borrow(|selected| {
                                 if let Some(selected) = selected
                                 {
                                     return AllocationSelection::Selected(selected.clone());
                                 }
                                 if let Some(target) = self.worker_allocation.lock().as_ref()
                                 {
                                     return AllocationSelection::Selected(target.clone());
                                 }
                                 match self.allocation.lock().as_ref()
            {
                Some(account) => AllocationSelection::Selected(crate::allocator::Target::Owner {
                    account: account.clone(),
                    class: mac_kernel_model::policy::MemoryClass::Kernel
                }),
                None => AllocationSelection::Unavailable
            }
                             })
    }

    pub(crate) fn authenticate_module(&self, module: u64) -> Result<(), mac_abi::Status>
    {
        self.module.borrow(|frame| {
                       match frame
                       {
                           Some(frame) => frame.authenticate(module),
                           None =>
                           {
                               match self.owner.lock().as_ref()
                               {
                                   Some(ThreadOwner::Module { frame,
                                                              .. }) => frame.authenticate(module),
                                   _ => Err(mac_abi::ACCESS_DENIED)
                               }
                           }
                       }
                   })
    }

    pub(crate) fn module_owner(&self, module: u64)
                               -> Result<crate::toolbox::handles::OwnerRef, mac_abi::Status>
    {
        self.module.borrow(|frame| {
                       match frame
                       {
                           Some(frame) => frame.owner(module),
                           None =>
                           {
                               match self.owner.lock().as_ref()
                               {
                                   Some(ThreadOwner::Module { frame,
                                                              .. }) => frame.owner(module),
                                   _ => Err(mac_abi::ACCESS_DENIED)
                               }
                           }
                       }
                   })
    }

    pub(crate) fn module_callback_allowed(&self, module: u64, address: usize)
                                          -> Result<(), mac_abi::Status>
    {
        self.module.borrow(|frame| {
                       match frame
                       {
                           Some(frame) => frame.callback_allowed(module, address),
                           None =>
                           {
                               match self.owner.lock().as_ref()
                               {
                                   Some(ThreadOwner::Module { frame,
                                                              .. }) =>
                                   {
                                       frame.callback_allowed(module, address)
                                   }
                                   _ => Err(mac_abi::ACCESS_DENIED)
                               }
                           }
                       }
                   })
    }

    pub(crate) fn owner(&self) -> Result<crate::toolbox::handles::OwnerRef, mac_abi::Status>
    {
        use mac_object_table::shared::InvocationGate as _;
        crate::toolbox::handles::InvocationGate::run(|| {
            self.owner
                .lock()
                .as_ref()
                .map(|binding| binding.owner().clone())
                .ok_or(mac_abi::WRONG_THREAD)
        })
    }
}
/// An owned pin, never a static reference to reclaimable scheduler storage.
/// The static variant is reserved for the actual boot/firmware context.
#[cfg(target_arch = "riscv64")]
pub enum ContextRef
{
    Bootstrap,
    Thread(ThreadPin)
}
#[cfg(target_arch = "riscv64")]
pub struct ThreadPin
{
    context: mac_object_table::retained::Read<ThreadContext>
}
#[cfg(target_arch = "riscv64")]
impl ContextRef
{
    pub(crate) fn thread(context: alloc::sync::Arc<ThreadContext>) -> Self
    {
        Self::Thread(ThreadPin { context: mac_object_table::retained::Read::new(context, crate::machine::scheduler::context_released) })
    }

    pub(crate) fn is_bootstrap(&self) -> bool
    {
        matches!(self, Self::Bootstrap)
    }
}
#[cfg(target_arch = "riscv64")]
impl core::ops::Deref for ContextRef
{
    type Target = ThreadContext;

    fn deref(&self) -> &Self::Target
    {
        match self
        {
            Self::Bootstrap => &BOOTSTRAP_CONTEXT,
            Self::Thread(pin) => &pin.context
        }
    }
}

#[cfg(target_arch = "riscv64")]
static BOOTSTRAP_CONTEXT: ThreadContext = ThreadContext::new();
#[cfg(target_arch = "riscv64")]
static BOOT_RESOURCES: selection::PermanentSelection<crate::kernel::ResourceSelection> =
    selection::PermanentSelection::new(BOOT_THREAD);

// An I/O subject is admitted authority, independent of the executor's app/module
// label. Retain it only for the synchronous callback; Pending work owns its own
// cloned capability in the job/request record, never a stack guard.
#[cfg(target_arch = "riscv64")]
static BOOT_IO_SUBJECT: selection::PermanentSelection<crate::drivers::jobs::subjects::Subject> =
    selection::PermanentSelection::new(BOOT_THREAD);

/// An I/O handoff preserves absence as refusal, never as System authority.
/// Capturing this clones genuine authority once per handoff, not per allocation.
#[cfg(target_arch = "riscv64")]
#[derive(Clone)]
pub(crate) enum AllocationSelection
{
    Selected(crate::allocator::Target),
    Unavailable
}
#[cfg(target_arch = "riscv64")]
static BOOT_ALLOCATION: selection::PermanentSelection<AllocationSelection> =
    selection::PermanentSelection::new(BOOT_THREAD);

#[cfg(target_arch = "riscv64")]
pub(crate) fn reserve_allocation(
    bytes: u64)
    -> Result<crate::allocator::Reservation, crate::allocator::Error>
{
    crate::machine::scheduler::reserve_allocation(bytes)
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn reserve_replacement(
    receipt: &crate::allocator::Receipt, bytes: u64)
    -> Result<crate::allocator::Reservation, crate::allocator::Error>
{
    crate::machine::scheduler::reserve_replacement(receipt, bytes)
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn reserve_bootstrap_allocation(
    bytes: u64)
    -> Result<crate::allocator::Reservation, crate::allocator::Error>
{
    with_bootstrap_allocation_selection(|selection| selection.reserve(bytes))
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn reserve_bootstrap_replacement(
    receipt: &crate::allocator::Receipt, bytes: u64)
    -> Result<crate::allocator::Reservation, crate::allocator::Error>
{
    with_bootstrap_allocation_selection(|selection| receipt.reserve_replacement(selection, bytes))
}

#[cfg(target_arch = "riscv64")]
fn with_bootstrap_allocation_selection<R>(operation: impl FnOnce(crate::allocator::Selection<'_>)
                                                 -> Result<R, crate::allocator::Error>)
                                          -> Result<R, crate::allocator::Error>
{
    BOOT_ALLOCATION.borrow(|selected| match selected
    {
        Some(AllocationSelection::Selected(target)) => operation(target.selection()),
        Some(AllocationSelection::Unavailable) => Err(crate::allocator::Error::ContextUnavailable),
        // The actual permanent bootstrap executor has explicit static System
        // authority, including first heap use before any thread is published.
        None => operation(crate::allocator::Selection::System(crate::allocator::SystemClass::Ordinary))
    })
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn selected_allocation() -> AllocationSelection
{
    let context = current_thread_context();
    if context.is_bootstrap()
    {
        BOOT_ALLOCATION.borrow(|selected| {
                           selected.cloned().unwrap_or_else(|| {
            AllocationSelection::Selected(crate::allocator::Target::System(
                crate::allocator::SystemClass::Ordinary))
        })
                       })
    }
    else
    {
        context.selected_allocation()
    }
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn capture_allocation() -> Result<crate::allocation::Target, mac_abi::Status>
{
    match selected_allocation()
    {
        AllocationSelection::Selected(target) => Ok(target),
        AllocationSelection::Unavailable => Err(mac_abi::WRONG_THREAD)
    }
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn with_allocation<R>(target: crate::allocator::Target, operation: impl FnOnce() -> R)
                                 -> R
{
    // One actual context pin owns the scope's lifetime. The allocator itself
    // uses only the borrowed reservation operation above.
    let context = current_thread_context();
    if context.is_bootstrap()
    {
        BOOT_ALLOCATION.with(Some(AllocationSelection::Selected(target)), operation)
    }
    else
    {
        context.allocation_scope.with_pinned(target, operation)
    }
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn with_system_allocation<R>(class: crate::allocator::SystemClass,
                                        operation: impl FnOnce() -> R)
                                        -> R
{
    with_allocation(crate::allocator::Target::System(class), operation)
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn with_bootstrap_allocation<R>(selection: AllocationSelection,
                                           operation: impl FnOnce() -> R)
                                           -> R
{
    BOOT_ALLOCATION.with(Some(selection), operation)
}


#[cfg(target_arch = "riscv64")]
static BOOT_MODULE: selection::PermanentSelection<crate::images::ModuleFrame> =
    selection::PermanentSelection::new(BOOT_THREAD);

/// Authenticate the installed executor without deriving authority from its label.
/// Stopping admission does not invalidate an already admitted cleanup invocation.
#[cfg(target_arch = "riscv64")]
pub(crate) fn authenticate_module(module: u64) -> Result<(), mac_abi::Status>
{
    crate::machine::scheduler::authenticate_module(module)
}
#[cfg(target_arch = "riscv64")]
pub(crate) fn module_owner(module: u64)
                           -> Result<crate::toolbox::handles::OwnerRef, mac_abi::Status>
{
    crate::machine::scheduler::module_owner(module)
}
#[cfg(target_arch = "riscv64")]
pub(crate) fn bootstrap_module_owner(
    module: u64)
    -> Result<crate::toolbox::handles::OwnerRef, mac_abi::Status>
{
    BOOT_MODULE.borrow(|frame| frame.ok_or(mac_abi::ACCESS_DENIED)?.owner(module))
}
#[cfg(target_arch = "riscv64")]
pub(crate) fn module_callback_allowed(module: u64, address: usize) -> Result<(), mac_abi::Status>
{
    crate::machine::scheduler::module_callback_allowed(module, address)
}
#[cfg(target_arch = "riscv64")]
pub(crate) fn authenticate_bootstrap_module(module: u64) -> Result<(), mac_abi::Status>
{
    BOOT_MODULE.borrow(|frame| frame.ok_or(mac_abi::ACCESS_DENIED)?.authenticate(module))
}
#[cfg(target_arch = "riscv64")]
pub(crate) fn bootstrap_module_callback_allowed(module: u64, address: usize)
                                                -> Result<(), mac_abi::Status>
{
    BOOT_MODULE.borrow(|frame| {
                   frame.ok_or(mac_abi::ACCESS_DENIED)?.callback_allowed(module, address)
               })
}
#[cfg(target_arch = "riscv64")]
pub(crate) fn with_module_execution<R>(execution: &crate::images::ModuleExecution,
                                       call: impl FnOnce() -> R)
                                       -> Result<R, mac_abi::Status>
{
    let context = current_thread_context();
    if context.is_bootstrap()
    {
        Ok(BOOT_MODULE.with(Some(execution.frame()), call))
    }
    else
    {
        context.module.with(execution.frame(), call).map_err(|_| mac_abi::WRONG_THREAD)
    }
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn critical_active() -> bool
{
    crate::machine::scheduler::critical_active()
}
#[cfg(target_arch = "riscv64")]
pub(crate) fn bootstrap_critical_active() -> bool
{
    BOOTSTRAP_CONTEXT.critical_active()
}
#[cfg(target_arch = "riscv64")]
pub(crate) fn with_critical_section<R>(operation: impl FnOnce() -> R)
                                       -> Result<R, mac_abi::Status>
{
    crate::machine::interrupts::with_masked(|| {
        let context = current_thread_context();
        context.critical_depth
               .try_update(core::sync::atomic::Ordering::Relaxed,
                           core::sync::atomic::Ordering::Relaxed,
                           |depth| depth.checked_add(1))
               .map_err(|_| mac_abi::SERVICE_BUSY)?;
        struct Restore<'a>(&'a core::sync::atomic::AtomicUsize);
        impl Drop for Restore<'_>
        {
            fn drop(&mut self)
            {
                self.0.fetch_sub(1, core::sync::atomic::Ordering::Relaxed);
            }
        }
        let _restore = Restore(&context.critical_depth);
        Ok(operation())
    })
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn io_subject() -> Option<crate::drivers::jobs::subjects::Subject>
{
    let context = current_thread_context();
    if context.is_bootstrap()
    {
        BOOT_IO_SUBJECT.current()
    }
    else
    {
        context.io_subject.current().or_else(|| context.worker_io_subject())
    }
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn with_io_subject<R>(subject: crate::drivers::jobs::subjects::Subject,
                                 call: impl FnOnce() -> R)
                                 -> Result<R, mac_abi::Status>
{
    let allocation = subject.allocation().clone();
    let context = current_thread_context();
    with_allocation(allocation, || {
        if context.is_bootstrap()
        {
            Ok(BOOT_IO_SUBJECT.with(Some(subject), call))
        }
        else
        {
            context.io_subject.with(subject, call).map_err(|_| mac_abi::WRONG_THREAD)
        }
    })
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn with_bootstrap_io_subject<R>(subject: Option<crate::drivers::jobs::subjects::Subject>,
                                           call: impl FnOnce() -> R)
                                           -> R
{
    BOOT_IO_SUBJECT.with(subject, call)
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn selected_resources() -> Option<crate::kernel::ResourceSelection>
{
    let context = current_thread_context();
    if context.is_bootstrap()
    {
        BOOT_RESOURCES.current()
    }
    else
    {
        context.resources.current().or_else(|| context.default_resources())
    }
}
#[cfg(target_arch = "riscv64")]
pub(crate) fn with_selected_resources<R>(selection: crate::kernel::ResourceSelection,
                                         call: impl FnOnce() -> R)
                                         -> Result<R, selection::Reclaimed>
{
    let context = current_thread_context();
    if context.is_bootstrap()
    {
        Ok(BOOT_RESOURCES.with(Some(selection), call))
    }
    else
    {
        context.resources.with(selection, call)
    }
}
/// The native I/O wrapper has entered the permanent bootstrap execution
/// context. Transport selection metadata without changing executor identity.
#[cfg(target_arch = "riscv64")]
pub(crate) fn with_bootstrap_resources<R>(selection: Option<crate::kernel::ResourceSelection>,
                                          call: impl FnOnce() -> R)
                                          -> R
{
    BOOT_RESOURCES.with(selection, call)
}

#[cfg(target_arch = "riscv64")]
pub fn current_thread_context() -> ContextRef
{
    crate::machine::scheduler::current_context()
}
#[cfg(target_arch = "riscv64")]
pub fn current_thread() -> ThreadId
{
    current_thread_context().identity().thread
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn current_owner() -> Result<crate::toolbox::handles::OwnerRef, mac_abi::Status>
{
    let context = current_thread_context();
    if context.is_bootstrap() { crate::toolbox::handles::system_owner() } else { context.owner() }
}


#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn deferred_io_is_exclusive_to_an_outer_service_and_consumed_once()
    {
        let state = ThreadTrapState::new();
        assert_eq!(state.defer(DeferredIo::DriverJob(17)), Err(mac_abi::WRONG_THREAD));
        let outer = state.enter();
        assert_eq!(outer.depth, 1);
        assert_eq!(state.defer(DeferredIo::DriverJob(0)), Err(mac_abi::BAD_ARGUMENT));
        {
            let _nested = state.enter();
            assert_eq!(state.defer(DeferredIo::DriverJob(17)), Err(mac_abi::WRONG_THREAD));
        }
        state.defer(DeferredIo::DriverJob(17)).unwrap();
        assert_eq!(state.defer(DeferredIo::DriverJob(18)), Err(mac_abi::SERVICE_BUSY));
        assert_eq!(state.take_deferred(), Some(DeferredIo::DriverJob(17)));
        assert_eq!(state.take_deferred(), None);
        let wide = DeferredIo::FileTransfer(u64::MAX - 17);
        state.defer(wide).unwrap();
        assert_eq!(state.defer(DeferredIo::DriverJob(17)), Err(mac_abi::SERVICE_BUSY));
        assert_eq!(state.take_deferred(), Some(wide));
        assert_ne!(DeferredIo::FileTransfer(17), DeferredIo::DriverJob(17));
        drop(outer);
        assert_eq!(state.defer(DeferredIo::DriverJob(19)), Err(mac_abi::WRONG_THREAD));
    }
}


/// Diagnostic invocation identity belongs to the scheduled thread, never a hart
/// global or caller-selected request. This is not the later module resource scope.
#[cfg(any(target_arch = "riscv64", test))]
pub struct DefinitionInvocations
{
    current: core::sync::atomic::AtomicU64
}
#[cfg(any(target_arch = "riscv64", test))]
impl DefinitionInvocations
{
    pub const fn new() -> Self
    {
        Self { current: core::sync::atomic::AtomicU64::new(0) }
    }

    pub fn with<R>(&self, kind: u32, id: i16, operation: impl FnOnce() -> R) -> R
    {
        let _scope = self.enter(kind, id);
        operation()
    }

    fn enter(&self, kind: u32, id: i16) -> DefinitionScope<'_>
    {
        let identity = (u64::from(kind) << 16) | u64::from(id as u16);
        assert_ne!(kind, 0, "definition family must be named");
        let previous = self.current.swap(identity, core::sync::atomic::Ordering::Relaxed);
        DefinitionScope { state: self,
                          previous }
    }

    pub fn current(&self) -> Option<(u32, i16)>
    {
        let identity = self.current.load(core::sync::atomic::Ordering::Relaxed);
        (identity != 0).then_some(((identity >> 16) as u32, identity as i16))
    }
}
#[cfg(any(target_arch = "riscv64", test))]
struct DefinitionScope<'a>
{
    state:    &'a DefinitionInvocations,
    previous: u64
}
#[cfg(any(target_arch = "riscv64", test))]
impl Drop for DefinitionScope<'_>
{
    fn drop(&mut self)
    {
        self.state.current.store(self.previous, core::sync::atomic::Ordering::Relaxed);
    }
}
#[cfg(test)]
mod definition_tests
{
    use super::*;
    #[test]
    fn nested_definition_scopes_restore_only_their_threads_identity()
    {
        let first = DefinitionInvocations::new();
        let other = DefinitionInvocations::new();
        assert_eq!(first.current(), None);
        first.with(mac_abi::definitions::WDEF, 0, || {
                 first.with(mac_abi::definitions::CDEF, -32759, || {
                          assert_eq!(first.current(), Some((mac_abi::definitions::CDEF, -32759)));
                          assert_eq!(other.current(), None);
                      });
                 assert_eq!(first.current(), Some((mac_abi::definitions::WDEF, 0)));
             });
        assert_eq!(first.current(), None);
    }
}

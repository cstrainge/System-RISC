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
/// guessed from the bit range of a different manager's projected job ID.
#[cfg(any(target_arch = "riscv64", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeferredIo
{
    DriverJob(u32),
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

/// Accounting belongs to an OS thread and survives hart migration with it.
/// Independent app calls must never increment each other's nesting counters.
#[cfg(any(target_arch = "riscv64", test))]
pub struct ThreadTrapState
{
    depth:    core::sync::atomic::AtomicUsize,
    #[cfg(target_arch = "riscv64")]
    reported: core::sync::atomic::AtomicBool,
    deferred: spinning_top::Spinlock<Option<DeferredIo>>
}
#[cfg(any(target_arch = "riscv64", test))]
impl ThreadTrapState
{
    pub const fn new() -> Self
    {
        Self { depth:                                    core::sync::atomic::AtomicUsize::new(0),
               #[cfg(target_arch = "riscv64")]
               reported:                                 core::sync::atomic::AtomicBool::new(false),
               deferred:                                 spinning_top::Spinlock::new(None) }
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
struct ThreadOwner
{
    owner: crate::toolbox::handles::OwnerRef,
    image: crate::images::Binding
}
#[cfg(target_arch = "riscv64")]
pub struct ThreadContext
{
    app:             core::sync::atomic::AtomicU64,
    thread:          core::sync::atomic::AtomicU64,
    owner:           spinning_top::Spinlock<Option<ThreadOwner>>,
    resources:       selection::ScopedSelection<crate::kernel::ResourceSelection>,
    pub traps:       ThreadTrapState,
    pub definitions: DefinitionInvocations
}
#[cfg(target_arch = "riscv64")]
impl ThreadContext
{
    pub const fn new() -> Self
    {
        Self { app:         core::sync::atomic::AtomicU64::new(0),
               thread:      core::sync::atomic::AtomicU64::new(1),
               owner:       spinning_top::Spinlock::new(None),
               resources:   selection::ScopedSelection::new(BOOT_THREAD),
               traps:       ThreadTrapState::new(),
               definitions: DefinitionInvocations::new() }
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
            self.owner.lock().as_ref().map(|binding| binding.image)
        })
    }

    pub(crate) fn assign(&self, owner: crate::toolbox::handles::OwnerRef, thread: u64,
                         image: crate::images::Binding)
                         -> Result<(), mac_abi::Status>
    {
        use core::sync::atomic::Ordering::Relaxed;

        use mac_object_table::shared::InvocationGate as _;
        if thread == 0
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
            if binding.is_some()
            {
                return Err(mac_abi::SERVICE_BUSY);
            }
            self.resources.reset(ThreadId(thread));
            self.app.store(owner.owner().incarnation(), Relaxed);
            self.thread.store(thread, Relaxed);
            *binding = Some(ThreadOwner { owner,
                                          image });
            Ok(())
        })
    }

    pub(crate) fn release(&self)
    {
        use mac_object_table::shared::InvocationGate as _;
        crate::toolbox::handles::InvocationGate::run(|| {
            let owner = self.owner.lock().take();
            self.resources.reset(ThreadId(0));
            self.app.store(0, core::sync::atomic::Ordering::Relaxed);
            self.thread.store(0, core::sync::atomic::Ordering::Relaxed);
            drop(owner);
        });
    }

    pub(crate) fn owner(&self) -> Result<crate::toolbox::handles::OwnerRef, mac_abi::Status>
    {
        use mac_object_table::shared::InvocationGate as _;
        crate::toolbox::handles::InvocationGate::run(|| {
            self.owner
                .lock()
                .as_ref()
                .map(|binding| binding.owner.clone())
                .ok_or(mac_abi::WRONG_THREAD)
        })
    }
}
#[cfg(target_arch = "riscv64")]
static BOOTSTRAP_CONTEXT: ThreadContext = ThreadContext::new();
#[cfg(target_arch = "riscv64")]
static BOOT_RESOURCES: selection::PermanentSelection<crate::kernel::ResourceSelection> =
    selection::PermanentSelection::new(BOOT_THREAD);

#[cfg(target_arch = "riscv64")]
pub(crate) fn selected_resources() -> Option<crate::kernel::ResourceSelection>
{
    let context = current_thread_context();
    if core::ptr::eq(context, boot_context())
    {
        BOOT_RESOURCES.current()
    }
    else
    {
        context.resources.current()
    }
}
#[cfg(target_arch = "riscv64")]
pub(crate) fn with_selected_resources<R>(selection: crate::kernel::ResourceSelection,
                                         call: impl FnOnce() -> R)
                                         -> Result<R, selection::Reclaimed>
{
    let context = current_thread_context();
    if core::ptr::eq(context, boot_context())
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
pub fn current_thread_context() -> &'static ThreadContext
{
    crate::machine::scheduler::current_context()
}
#[cfg(target_arch = "riscv64")]
pub fn boot_context() -> &'static ThreadContext
{
    &BOOTSTRAP_CONTEXT
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
    if core::ptr::eq(context, boot_context())
    {
        crate::toolbox::handles::system_owner()
    }
    else
    {
        context.owner()
    }
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

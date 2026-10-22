//! Execution identity and virtualized classic state shapes. Identity must come
//! from the kernel scheduler, not from untrusted request fields.
#![allow(dead_code)] // Legacy cooperative state fields await their individual services.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LegacyContextId(pub u64);
pub const BOOT_THREAD: ThreadId = ThreadId(1);
static FINDER_APP: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub fn finder_app() -> AppId
{
    AppId(FINDER_APP.load(core::sync::atomic::Ordering::Acquire))
}
pub(crate) fn set_finder_app(app: u64)
{
    FINDER_APP.store(app, core::sync::atomic::Ordering::Release);
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionContext
{
    pub app:    AppId,
    pub thread: ThreadId,
    pub legacy: Option<LegacyContextId>
}
/// Owned by an application; legacy context switching saves/restores only fields
/// whose original contract is context-local. There is no global current-app slot.
#[derive(Clone, Debug, Default)]
pub struct LegacyGlobals
{
    pub current_resource_file: i16,
    pub graphics_port:         u64,
    pub font:                  mac_abi::quickdraw::FontSelection,
    pub mem_error:             i16,
    pub resource_error:        i16,
    pub current_volume:        u32,
    pub current_directory:     u32,
    pub event_mask:            u16,
    pub a5_world:              u64,
    pub application_zone:      u64,
    pub scrap_reference:       u64
}
#[cfg(target_arch = "riscv64")]
pub fn bootstrap_thread() -> ThreadId
{
    // Used only to admit primary-hart bootstrap. Scheduled service calls obtain
    // their actual identities from current_thread_context instead.
    let hart = crate::machine::hart_id();
    if hart == 0 { BOOT_THREAD } else { ThreadId(0) }
}

/// Accounting belongs to an OS thread and survives hart migration with it.
/// Independent app calls must never increment each other's nesting counters.
pub struct ThreadTrapState
{
    depth:    core::sync::atomic::AtomicUsize,
    reported: core::sync::atomic::AtomicBool,
    deferred: core::sync::atomic::AtomicU32
}
impl ThreadTrapState
{
    pub const fn new() -> Self
    {
        Self { depth:    core::sync::atomic::AtomicUsize::new(0),
               reported: core::sync::atomic::AtomicBool::new(false),
               deferred: core::sync::atomic::AtomicU32::new(0) }
    }

    pub fn enter(&self) -> TrapScope<'_>
    {
        let depth = self.depth.fetch_add(1, core::sync::atomic::Ordering::Relaxed) + 1;
        TrapScope { state: self,
                    depth }
    }

    pub fn defer(&self, job: u32) -> Result<(), mac_abi::Status>
    {
        use core::sync::atomic::Ordering::Relaxed;
        if job == 0
        {
            return Err(mac_abi::BAD_ARGUMENT);
        }
        if self.depth.load(Relaxed) != 1
        {
            return Err(mac_abi::WRONG_THREAD);
        }
        self.deferred
            .compare_exchange(0, job, Relaxed, Relaxed)
            .map(|_| ())
            .map_err(|_| mac_abi::SERVICE_BUSY)
    }

    pub fn take_deferred(&self) -> Option<u32>
    {
        let job = self.deferred.swap(0, core::sync::atomic::Ordering::Relaxed);
        (job != 0).then_some(job)
    }

    pub fn nesting_reported(&self) -> bool
    {
        self.reported.swap(true, core::sync::atomic::Ordering::Relaxed)
    }
}
pub struct TrapScope<'a>
{
    state:     &'a ThreadTrapState,
    pub depth: usize
}
impl Drop for TrapScope<'_>
{
    fn drop(&mut self)
    {
        self.state.depth.fetch_sub(1, core::sync::atomic::Ordering::Relaxed);
    }
}
pub struct ThreadContext
{
    app:       core::sync::atomic::AtomicU64,
    thread:    core::sync::atomic::AtomicU64,
    pub traps: ThreadTrapState
}
impl ThreadContext
{
    pub const fn new() -> Self
    {
        Self { app:    core::sync::atomic::AtomicU64::new(0),
               thread: core::sync::atomic::AtomicU64::new(1),
               traps:  ThreadTrapState::new() }
    }

    pub fn identity(&self) -> ExecutionContext
    {
        use core::sync::atomic::Ordering::Relaxed;
        ExecutionContext { app:    AppId(self.app.load(Relaxed)),
                           thread: ThreadId(self.thread.load(Relaxed)),
                           legacy: None }
    }

    pub fn assign(&self, app: u64, thread: u64)
    {
        use core::sync::atomic::Ordering::Relaxed;
        self.app.store(app, Relaxed);
        self.thread.store(thread, Relaxed);
    }
}
static BOOTSTRAP_CONTEXT: ThreadContext = ThreadContext::new();
#[cfg(target_arch = "riscv64")]
pub fn current_thread_context() -> &'static ThreadContext
{
    crate::machine::scheduler::current_context()
}
#[cfg(not(target_arch = "riscv64"))]
pub fn current_thread_context() -> &'static ThreadContext
{
    &BOOTSTRAP_CONTEXT
}
pub fn boot_context() -> &'static ThreadContext
{
    &BOOTSTRAP_CONTEXT
}
#[cfg(target_arch = "riscv64")]
pub fn current_thread() -> ThreadId
{
    current_thread_context().identity().thread
}


#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn deferred_io_is_exclusive_to_an_outer_service_and_consumed_once()
    {
        let state = ThreadTrapState::new();
        assert_eq!(state.defer(17), Err(mac_abi::WRONG_THREAD));
        let outer = state.enter();
        assert_eq!(state.defer(0), Err(mac_abi::BAD_ARGUMENT));
        {
            let _nested = state.enter();
            assert_eq!(state.defer(17), Err(mac_abi::WRONG_THREAD));
        }
        state.defer(17).unwrap();
        assert_eq!(state.defer(18), Err(mac_abi::SERVICE_BUSY));
        assert_eq!(state.take_deferred(), Some(17));
        assert_eq!(state.take_deferred(), None);
        drop(outer);
        assert_eq!(state.defer(19), Err(mac_abi::WRONG_THREAD));
    }
}

//! Execution identity and virtualized classic state shapes. Identity must come
//! from the kernel scheduler, not from untrusted request fields.
#![allow(dead_code)] // Scheduler/context routing is a declared next milestone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LegacyContextId(pub u64);
pub const BOOT_THREAD: ThreadId = ThreadId(1);
pub const FINDER_APP: AppId = AppId(1);
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
    // Bootstrap-only identity. The machine adapter supplies hardware identity;
    // a future scheduler must supply actual ThreadId before launching threads.
    let hart = crate::machine::hart_id();
    if hart == 0 { BOOT_THREAD } else { ThreadId(0) }
}

/// Accounting belongs to an OS thread and survives hart migration with it.
/// Independent app calls must never increment each other's nesting counters.
pub struct ThreadTrapState
{
    depth:    core::sync::atomic::AtomicUsize,
    reported: core::sync::atomic::AtomicBool
}
impl ThreadTrapState
{
    pub const fn new() -> Self
    {
        Self { depth:    core::sync::atomic::AtomicUsize::new(0),
               reported: core::sync::atomic::AtomicBool::new(false) }
    }

    pub fn enter(&self) -> TrapScope<'_>
    {
        let depth = self.depth.fetch_add(1, core::sync::atomic::Ordering::Relaxed) + 1;
        TrapScope { state: self,
                    depth }
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
    pub identity: ExecutionContext,
    pub traps:    ThreadTrapState
}
static BOOTSTRAP_CONTEXT: ThreadContext =
    ThreadContext { identity: ExecutionContext { app:    FINDER_APP,
                                                 thread: BOOT_THREAD,
                                                 legacy: None },
                    traps:    ThreadTrapState::new() };
/// Temporary single-thread bootstrap accessor. A scheduler must replace this
/// with its current OS ThreadContext before launching any additional threads;
/// no global current-app or per-hart nesting counter is introduced here.
pub fn current_thread_context() -> &'static ThreadContext
{
    &BOOTSTRAP_CONTEXT
}

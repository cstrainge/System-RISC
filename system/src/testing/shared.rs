//! Shared, real resident Memory service fixture. Unlike scoped TLS fixtures,
//! every host OS thread enters the same production TryState and manager.
use std::sync::{Arc, Mutex};

use mac_abi::*;

use crate::state::TryState;

static SESSION: Mutex<Option<Arc<TryState<crate::toolbox::memory::Manager>>>> = Mutex::new(None);
static SCOPE: Mutex<()> = Mutex::new(());
struct Cleanup;
impl Drop for Cleanup
{
    fn drop(&mut self)
    {
        *SESSION.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}
/// All spawned caller threads must finish before `run` returns. The service table
/// borrows a single shared manager; no canned results or test renderer is used.
pub fn with_memory_services<R>(run: impl FnOnce(&SystemServices) -> R) -> R
{
    let _scope = SCOPE.lock().unwrap_or_else(|e| e.into_inner());
    let state = Arc::new(TryState::new());
    state.publish(crate::toolbox::memory::Manager::default()).unwrap();
    *SESSION.lock().unwrap_or_else(|e| e.into_inner()) = Some(state);
    let _cleanup = Cleanup;
    let services = SystemServices { header:       Header::for_type::<SystemServices>(),
                                    trap:         gate,
                                    capabilities: 0 };
    run(&services)
}

/// Simulate application exit on the same real manager used by the shared gate.
/// Existing caller threads must be quiescent, just as native exit drains work.
pub fn clear_memory_services() -> Result<(), Status>
{
    let state = SESSION.lock().unwrap_or_else(|e| e.into_inner()).clone().ok_or(NOT_SUPPORTED)?;
    state.with(crate::toolbox::memory::Manager::clear).map_err(|error| error.status())
}
unsafe extern "C" fn gate(number: u32, pointer: *mut (), size: usize) -> i64
{
    // This host-only fixture setup lock is never held across a service call.
    let state = SESSION.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(state) = state
    else
    {
        return NOT_SUPPORTED as i64;
    };
    state.with(|manager| unsafe {
             crate::toolbox::memory::native::dispatch(manager, number, pointer, size)
         })
         .unwrap_or_else(|e| e.status() as i64)
}

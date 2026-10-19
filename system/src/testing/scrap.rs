//! Shared real Scrap Manager fixture. Host OS threads call the public native
//! service table and contend on the production nonwaiting TryState holder.
use std::sync::{Arc, Mutex};

use mac_abi::*;

use crate::state::TryState;
static SESSION: Mutex<Option<Arc<TryState<crate::toolbox::scrap::Manager>>>> = Mutex::new(None);
static SCOPE: Mutex<()> = Mutex::new(());
struct Cleanup;
impl Drop for Cleanup
{
    fn drop(&mut self)
    {
        *SESSION.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}
pub fn with_services<R>(run: impl FnOnce(&SystemServices) -> R) -> R
{
    let _scope = SCOPE.lock().unwrap_or_else(|e| e.into_inner());
    let state = Arc::new(TryState::new());
    state.publish(crate::toolbox::scrap::Manager::default()).unwrap();
    *SESSION.lock().unwrap_or_else(|e| e.into_inner()) = Some(state);
    let _cleanup = Cleanup;
    let services = SystemServices { header:       Header::for_type::<SystemServices>(),
                                    trap:         gate,
                                    capabilities: 0 };
    run(&services)
}
unsafe extern "C" fn gate(number: u32, pointer: *mut (), size: usize) -> i64
{
    // Never hold this host-fixture setup lock across the production service call.
    let state = SESSION.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(state) = state
    else
    {
        return NOT_SUPPORTED as i64;
    };
    state.with(|manager| unsafe {
             crate::toolbox::scrap::native::dispatch(manager, number, pointer, size)
         })
         .unwrap_or_else(|error| error.status() as i64)
}

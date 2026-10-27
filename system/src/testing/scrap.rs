//! Shared real Scrap Manager fixture. Host OS threads call the public native
//! service table and contend on the production nonwaiting TryState holder.
use std::sync::{Arc, Mutex};

use mac_abi::*;

use crate::state::TryState;
pub use crate::toolbox::scrap::{ChangeError, Revision, Snapshot, SnapshotContent};
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

fn state() -> Result<Arc<TryState<crate::toolbox::scrap::Manager>>, Status>
{
    SESSION.lock().unwrap_or_else(|error| error.into_inner()).clone().ok_or(NOT_SUPPORTED)
}
/// Read-only probe over the same production store reached by public SDK calls.
/// This bypasses no guest registration gate: it is a host acceptance fixture.
pub fn snapshot() -> Result<Snapshot, Status>
{
    state()?.with(|manager| manager.snapshot()).map_err(crate::state::AccessError::status)
}
#[derive(Debug)]
pub enum ProbeError
{
    Unavailable(Status),
    Change(ChangeError)
}
impl core::fmt::Display for ProbeError
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Unavailable(_) =>
            {
                f.write_str("Scrap acceptance store is unavailable; install the fixture and retry")
            }
            Self::Change(cause) => cause.fmt(f)
        }
    }
}
pub fn compare_replace(expected: Revision, text: &str) -> Result<Revision, ProbeError>
{
    state().map_err(ProbeError::Unavailable)?
           .with(|manager| manager.compare_replace_text(expected, text))
           .map_err(|error| ProbeError::Unavailable(error.status()))?
           .map_err(ProbeError::Change)
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

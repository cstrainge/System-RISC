//! Shared, real resident Memory service fixture. Unlike scoped TLS fixtures,
//! every host OS thread enters the same production TryState and manager.
use std::sync::{Arc, Mutex};

use mac_abi::*;

use crate::state::TryState;

static SESSION: Mutex<Option<Arc<TryState<Fixture>>>> = Mutex::new(None);
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
    state.publish(Fixture::new()).unwrap();
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
    state.with(|fixture| {
             let owner = fixture.owner.clone();
             mac_object_table::walk::drain(fixture,
                                           &owner,
                                           |fixture, key| fixture.registry.dispatch(key.kind()),
                                           |key, error| {
                                               panic!("shared Memory fixture retained {key:?}: \
                                                       {error}")
                                           }).unwrap();
             assert_eq!((owner.summary().live, owner.summary().pending), (0, 0));
             fixture.owner = owner_ref();
         })
         .map_err(|error| error.status())
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
    state.with(|fixture| unsafe {
             crate::toolbox::memory::native::dispatch(&mut fixture.manager, &fixture.owner, number,
                                                      pointer, size)
         })
         .unwrap_or_else(|e| e.status() as i64)
}

fn owner_ref() -> crate::toolbox::handles::OwnerRef
{
    crate::toolbox::handles::OwnerRef::try_new(mac_object_table::domain::OwnerIndex::issue(mac_object_table::domain::DomainKind::Process).unwrap()).unwrap()
}
struct Fixture
{
    manager:  crate::toolbox::memory::Manager,
    owner:    crate::toolbox::handles::OwnerRef,
    registry: crate::toolbox::handles::Registry<Fixture>
}
impl Fixture
{
    fn new() -> Self
    {
        let registrar = crate::toolbox::handles::OwnerRef::try_new(mac_object_table::domain::OwnerIndex::issue(mac_object_table::domain::DomainKind::Module).unwrap()).unwrap();
        let mut registry = crate::toolbox::handles::Registry::new();
        let blocks = registry.register(&registrar,
                                       crate::toolbox::memory::lifetime::specification(false),
                                       crate::toolbox::memory::lifetime::ReleaseBlocks)
                             .unwrap();
        let storage = registry.register(&registrar,
                                        crate::toolbox::memory::lifetime::specification(true),
                                        crate::toolbox::memory::lifetime::ReleaseStorage)
                              .unwrap();
        Self { manager: crate::toolbox::memory::Manager::new((blocks, storage)).unwrap(),
               registry,
               owner: owner_ref() }
    }
}
impl crate::toolbox::memory::lifetime::MemoryHost for Fixture
{
    fn release_memory(&mut self, ticket: mac_object_table::domain::ReleasedResource, storage: bool)
                      -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        self.manager.release(ticket, storage).map_err(Into::into)
    }
}

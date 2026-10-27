//! Real resident Scrap service with genuine Registry-issued INIT owners.
//! This host transport does not fabricate a process or worker authority, execute
//! guest native entry code, or qualify the native scheduler/interrupt contract.
#![deny(unsafe_code)]
use alloc::{string::{String, ToString},
            vec,
            vec::Vec};
use std::sync::{Arc, Mutex};

use mac_abi::{Status, SystemServices};
use mac_module::{Contract, PackageKind, Service as Requirement, Spec, Target};
pub use mac_object_table::{domain::LedgerSummary, walk::Report};
use mac_object_table::{domain::{DomainKind, OwnerIndex, ReleasedResource},
                       walk};
pub use mac_scrap_extension_interface as wire;

pub use crate::toolbox::scrap::{Revision,
                                extension::{Content, Error, Pin, Read, Session}};
use crate::{drivers::{Registry, State,
                      registry::{dependencies, interfaces, requests}},
            state::TryState,
            toolbox::{handles::{self, OwnerRef,
                                cleanup::{Failure, context::Context}},
                      scrap::{Manager,
                              extension::{self, Authority, Registration}}}};

static SESSION: Mutex<Option<Arc<TryState<World>>>> = Mutex::new(None);
static SCOPE: Mutex<()> = Mutex::new(());
struct Cleanup;
impl Drop for Cleanup
{
    fn drop(&mut self)
    {
        *SESSION.lock().unwrap_or_else(|error| error.into_inner()) = None;
    }
}

#[derive(Debug)]
pub enum ProbeError
{
    State(Status),
    Service(Error),
    Registry(crate::drivers::Error)
}
impl core::fmt::Display for ProbeError
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::State(status) =>
            {
                write!(f,
                       "Scrap extension acceptance world is unavailable ({status}); retry the \
                        live fixture")
            }
            Self::Service(cause) => cause.fmt(f),
            Self::Registry(cause) => cause.fmt(f)
        }
    }
}
impl ProbeError
{
    pub fn status(&self) -> Status
    {
        match self
        {
            Self::State(status) => *status,
            Self::Service(cause) => cause.status(),
            Self::Registry(cause) => crate::drivers::status(*cause)
        }
    }
}

struct World
{
    registry:       Registry,
    catalog:        Arc<handles::Registry<dyn Context>>,
    scrap:          Manager,
    next_package:   u64,
    refuse_cleanup: bool
}
impl World
{
    fn wire_call(&mut self, caller: u64, operation: u32, input: &[u8], output: &mut [u8])
                 -> Result<usize, Status>
    {
        let authority = match Authority::authenticate(&self.registry, caller)
        {
            Ok(authority) => authority,
            Err(cause) =>
            {
                return crate::toolbox::scrap::extension_wire::refuse(cause.status(),
                                                                     &cause,
                                                                     output);
            }
        };
        crate::toolbox::scrap::extension_wire::dispatch(&mut self.scrap, &authority, operation,
                                                        input, output)
    }

    fn new() -> Self
    {
        // The registrar owns this isolated host catalog. It is never handed to
        // a client as Scrap authority; clients get actual Registry module owners.
        let registrar = OwnerRef::try_new(OwnerIndex::issue(DomainKind::Module).unwrap()).unwrap();
        let mut catalog = handles::Registry::<dyn Context>::new();
        let interface = catalog.register(&registrar,
                                         interfaces::specification(false),
                                         interfaces::ReleaseInterfaces(false))
                               .unwrap();
        let interface_storage = catalog.register(&registrar,
                                                 interfaces::specification(true),
                                                 interfaces::ReleaseInterfaces(true))
                                       .unwrap();
        let dependency = catalog.register(&registrar,
                                          dependencies::specification(false),
                                          dependencies::ReleaseDependencies(false))
                                .unwrap();
        let dependency_storage = catalog.register(&registrar,
                                                  dependencies::specification(true),
                                                  dependencies::ReleaseDependencies(true))
                                        .unwrap();
        let request = catalog.register(&registrar,
                                       requests::specification(false),
                                       requests::ReleaseRequests(false))
                             .unwrap();
        let request_storage = catalog.register(&registrar,
                                               requests::specification(true),
                                               requests::ReleaseRequests(true))
                                     .unwrap();
        let kinds = Registration::ALL.map(|registration| {
                                         catalog.register(&registrar,
                                                          registration.specification(),
                                                          registration)
                                                .unwrap()
                                     });
        let mut registry = Registry::new(Target::RVA23U64,
                                         registrar,
                                         [interface, interface_storage, dependency,
                                          dependency_storage, request, request_storage]).unwrap();
        registry.register_service(extension::OFFER).unwrap();
        let mut scrap = Manager::default();
        scrap.prepare_extensions(kinds).unwrap();
        Self { registry,
               catalog: Arc::new(catalog),
               scrap,
               next_package: 1,
               refuse_cleanup: false }
    }

    fn module(&mut self, kind: PackageKind, declared: bool, ready: bool) -> u64
    {
        let mut identity = *b"scrap-extension!";
        identity[8..].copy_from_slice(&self.next_package.to_le_bytes());
        self.next_package = self.next_package.checked_add(1).unwrap();
        let requirements = [Requirement { service:  extension::CLASS,
                                          version:  extension::VERSION,
                                          optional: false }];
        let spec = Spec { kind,
                          kernel_abi: mac_abi::ABI_VERSION,
                          target: Target::RVA23U64,
                          identity,
                          code_id: 0,
                          code_digest: [0; 32],
                          heap_bytes: 4096,
                          stack_bytes: 4096,
                          name: "Scrap extension acceptance",
                          version: "0.1.0",
                          provenance: "real registered host service fixture",
                          offers: &[],
                          dependencies: &[],
                          services: if declared { &requirements } else { &[] },
                          devices: &[] };
        let mut bytes = vec![0; spec.encoded_len().unwrap()];
        spec.encode(&mut bytes).unwrap();
        let module =
            self.registry.stage(Contract::decode(&bytes).unwrap(), kind, kind.file_type()).unwrap();
        if ready
        {
            self.registry.commit(module).unwrap();
        }
        module
    }
}
impl Context for World
{
    fn driver_registry(&mut self) -> Option<&mut Registry>
    {
        Some(&mut self.registry)
    }

    fn release_scrap_extension(&mut self, ticket: ReleasedResource, registration: Registration)
                               -> Result<(), Failure>
    {
        if self.refuse_cleanup
        {
            return Err(Failure::context_unavailable(ticket, "acceptance Scrap cleanup context"));
        }
        self.scrap.extension_release(ticket, registration)
    }
}

#[derive(Clone)]
pub struct Fixture
{
    state: Arc<TryState<World>>
}
#[derive(Clone)]
pub struct Module
{
    fixture: Fixture,
    caller:  u64
}
impl Fixture
{
    fn with<R>(&self, operation: impl FnOnce(&mut World) -> R) -> Result<R, ProbeError>
    {
        self.state.with(operation).map_err(|error| ProbeError::State(error.status()))
    }

    pub fn extension(&self, declared: bool, ready: bool) -> Module
    {
        let caller =
            self.with(|world| world.module(PackageKind::Extension, declared, ready)).unwrap();
        Module { fixture: self.clone(),
                 caller }
    }

    pub fn driver(&self) -> Module
    {
        let caller = self.with(|world| world.module(PackageKind::Driver, true, true)).unwrap();
        Module { fixture: self.clone(),
                 caller }
    }
}
impl Module
{
    pub fn wire_call(&self, operation: u32, input: &[u8], output: &mut [u8])
                     -> Result<usize, ProbeError>
    {
        self.fixture
            .with(|world| world.wire_call(self.caller, operation, input, output))?
            .map_err(ProbeError::State)
    }

    fn service<R>(&self, operation: impl FnOnce(&Authority, &mut Manager) -> Result<R, Error>)
                  -> Result<R, ProbeError>
    {
        self.fixture.with(|world| {
            let authority = Authority::authenticate(&world.registry, self.caller).map_err(ProbeError::Service)?;
            operation(&authority, &mut world.scrap).map_err(ProbeError::Service)
        })?
    }

    pub fn open(&self, writable: bool) -> Result<Session, ProbeError>
    {
        self.service(|authority, scrap| scrap.extension_open(authority, writable))
    }

    pub fn notice(&self, session: Session) -> Result<Option<Revision>, ProbeError>
    {
        self.service(|authority, scrap| scrap.extension_notice(authority, session))
    }

    pub fn snapshot(&self, session: Session) -> Result<Pin, ProbeError>
    {
        self.service(|authority, scrap| scrap.extension_snapshot(authority, session))
    }

    pub fn info(&self, session: Session, pin: Pin) -> Result<Read, ProbeError>
    {
        self.service(|authority, scrap| scrap.extension_info(authority, session, pin))
    }

    pub fn read(&self, session: Session, pin: Pin, offset: usize, output: &mut [u8])
                -> Result<Read, ProbeError>
    {
        self.service(|authority, scrap| {
                scrap.extension_read(authority, session, pin, offset, output)
            })
    }

    pub fn import(&self, session: Session, pin: Pin, text: &str) -> Result<Revision, ProbeError>
    {
        self.service(|authority, scrap| scrap.extension_import(authority, session, pin, text))
    }

    pub fn close_pin(&self, session: Session, pin: Pin) -> Result<(), ProbeError>
    {
        self.service(|authority, scrap| scrap.extension_close_pin(authority, session, pin))
    }

    pub fn disconnect(&self, session: Session) -> Result<(), ProbeError>
    {
        self.service(|authority, scrap| scrap.extension_disconnect(authority, session))
    }

    pub fn summary(&self) -> LedgerSummary
    {
        self.fixture
            .with(|world| world.registry.module_ledger(self.caller).unwrap().summary())
            .unwrap()
    }

    pub fn stop_and_probe_cached(&self, session: Session) -> Result<Option<Revision>, ProbeError>
    {
        self.fixture.with(|world| {
            let authority = Authority::authenticate(&world.registry, self.caller).map_err(ProbeError::Service)?;
            world.registry.quiesce(self.caller).map_err(ProbeError::Registry)?;
            world.scrap.extension_notice(&authority, session).map_err(ProbeError::Service)
        })?
    }

    /// Exercise the real invalidate-first walk, including wrong-context refusal
    /// and retry. The host fixture supplies no replacement release algorithm.
    pub fn cleanup(&self, refuse: bool, retry: bool) -> CleanupReport
    {
        self.fixture
            .with(|world| {
                if world.registry.state(self.caller).unwrap() == State::Ready
                {
                    world.registry.quiesce(self.caller).unwrap();
                }
                let owner = world.registry.module_ledger(self.caller).unwrap();
                let catalog = world.catalog.clone();
                world.refuse_cleanup = refuse;
                let mut failed = Vec::new();
                let mut record =
                    |key: mac_object_table::domain::ResourceKey,
                     cause: &walk::Failure<mac_object_table::registry::Error,
                                    handles::cleanup::Cause>| {
                        failed.push((key.kind().0,
                                     key.slot(),
                                     key.generation().get(),
                                     cause.to_string()));
                    };
                let report = if retry
                             {
                                 walk::retry(world as &mut dyn Context,
                                             &owner,
                                             |_, key| catalog.dispatch(key.kind()),
                                             &mut record)
                             }
                             else
                             {
                                 walk::drain(world as &mut dyn Context,
                                             &owner,
                                             |_, key| catalog.dispatch(key.kind()),
                                             &mut record)
                             }.unwrap();
                let summary = owner.summary();
                if summary.live == 0 && summary.pending == 0 && summary.completion_faults == 0
                {
                    world.registry.finish_quiesce(self.caller).unwrap();
                }
                CleanupReport { report,
                                summary,
                                failed }
            })
            .unwrap()
    }
}
pub struct CleanupReport
{
    pub report:  Report,
    pub summary: LedgerSummary,
    pub failed:  Vec<([u8; 4], usize, u64, String)>
}

pub fn with_services<R>(run: impl FnOnce(&SystemServices, &Fixture) -> R) -> R
{
    let _scope = SCOPE.lock().unwrap_or_else(|error| error.into_inner());
    let state = Arc::new(TryState::new());
    state.publish(World::new()).unwrap();
    *SESSION.lock().unwrap_or_else(|error| error.into_inner()) = Some(state.clone());
    let _cleanup = Cleanup;
    let services = SystemServices { header:       mac_abi::Header::for_type::<SystemServices>(),
                                    trap:         native::gate,
                                    capabilities: 0 };
    run(&services, &Fixture { state })
}

// The normal native Scrap dispatch remains the only borrowed-pointer boundary.
#[allow(unsafe_code)]
mod native
{
    use super::*;
    pub(super) unsafe extern "C" fn gate(number: u32, pointer: *mut (), size: usize) -> i64
    {
        let state = SESSION.lock().unwrap_or_else(|error| error.into_inner()).clone();
        let Some(state) = state
        else
        {
            return mac_abi::NOT_SUPPORTED as i64;
        };
        state.with(|world| unsafe {
                 crate::toolbox::scrap::native::dispatch(&mut world.scrap, number, pointer, size)
             })
             .unwrap_or_else(|error| error.status() as i64)
    }
}

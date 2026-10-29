//! Class-neutral module admission and publication. No driver callback executes
//! while this registry is borrowed. Hardware and class adapters live elsewhere.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_kernel_model::{account::{Account, AccountUse},
                       policy::ConfiguredRange};
use mac_object_table::shared::InvocationGate as _;

use crate::toolbox::handles::InvocationGate;
type ModuleAccount = Account<InvocationGate>;
pub(crate) type ModuleAllocation = AccountUse<InvocationGate>;

use mac_module::{Available, ClassId, Contract, Offer, PackageKind, Policy, Service, Target};

pub use crate::toolbox::handles::cleanup::CheckpointError;
use crate::toolbox::handles::{KindLease, OwnerRef, Store};
mod modules;
pub(crate) mod interfaces;
pub(crate) mod dependencies;
pub(crate) mod requests;
#[cfg(any(test, feature = "test-support"))]
pub(crate) mod fixture;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Contract(mac_module::Error),
    Duplicate,
    Exclusive,
    Exhausted,
    Account(mac_kernel_model::account::Error),
    AllocationScope(mac_abi::Status),
    AccountPending
    {
        owner:                 mac_object_table::domain::DomainId,
        kernel_committed:      u64,
        kernel_reserved:       u64,
        application_committed: u64,
        application_reserved:  u64,
        retained_pins:         usize
    },
    Stale,
    State,
    Access,
    RequestSubject,
    RequestParent,
    RecoveryUnsupported,
    Busy,
    BackingInUse
    {
        provider:  u64,
        dependent: u64
    },
    DependencyInUse
    {
        provider:  u64,
        dependent: u64
    },
    Cleanup(CheckpointError),
    Domain(mac_object_table::domain::DirectoryError),
    Identity(mac_object_table::domain::IncarnationsExhausted),
    Owner(mac_object_table::domain::PoolError)
}
impl From<mac_module::Error> for Error
{
    fn from(value: mac_module::Error) -> Self
    {
        Self::Contract(value)
    }
}
impl core::fmt::Display for Error
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Contract(error) =>
            {
                write!(f,
                       "module contract refused: {error:?}; rebuild the package against the \
                        current System contract")
            }
            Self::Duplicate => f.write_str("module identity or registration is already installed"),
            Self::Exclusive =>
            {
                f.write_str("an exclusive service already has an installed provider")
            }
            Self::Exhausted => f.write_str("module registry could not reserve metadata storage"),
            Self::Account(error) => error.fmt(f),
            Self::AllocationScope(status) =>
            {
                write!(f, "module allocation scope unavailable (status {status})")
            }
            Self::Stale =>
            {
                f.write_str("module, interface or request identity is absent, foreign or retired; \
                             resolve the current provider")
            }
            Self::State => f.write_str("module lifecycle state does not permit this operation"),
            Self::Access =>
            {
                f.write_str("module caller is absent or its contract does not authorize this \
                             service")
            }
            Self::RequestSubject =>
            {
                f.write_str("request belongs to a different I/O operation; use the original \
                             admitted operation capability for completion")
            }
            Self::RequestParent =>
            {
                f.write_str("nested broker call has no matching active parent invocation; use the \
                             original scoped callback or its owned queue")
            }
            Self::RecoveryUnsupported =>
            {
                f.write_str("this provider graph has no preadmitted protected request endpoints; \
                             it requires ordinary memory admission")
            }
            Self::AccountPending { owner,
                                   kernel_committed,
                                   kernel_reserved,
                                   application_committed,
                                   application_reserved,
                                   retained_pins } =>
            {
                write!(f,
                       "{owner} cleanup is drained but original account receipts remain: kernel \
                        {kernel_committed} committed/{kernel_reserved} reserved bytes, \
                        application {application_committed} committed/{application_reserved} \
                        reserved bytes, {retained_pins} retained uses or receipts; wait for their \
                        original holders to release")
            }
            Self::Busy =>
            {
                f.write_str("module still owns outstanding or active requests; wait for actual \
                             completion before retrying")
            }
            Self::DependencyInUse { provider,
                                    dependent } =>
            {
                write!(f,
                       "module {provider} still supplies a declared dependency to module \
                        {dependent}; unload the dependent module before retrying")
            }
            Self::BackingInUse { provider,
                                 dependent } =>
            {
                write!(f,
                       "module {provider} still supplies a backing lease to module {dependent}; \
                        unmount the dependent source and wait for actual close acknowledgement \
                        before retrying")
            }
            Self::Domain(error) => error.fmt(f),
            Self::Identity(error) => error.fmt(f),
            Self::Owner(error) => error.fmt(f),
            Self::Cleanup(error) => error.fmt(f)
        }
    }
}
impl core::error::Error for Error
{
}
pub type Result<T> = core::result::Result<T, Error>;
fn records<T>(source: impl Iterator<Item = mac_module::Result<T>>) -> Result<Vec<T>>
{
    let mut values = Vec::new();
    values.try_reserve(source.size_hint().0).map_err(|_| Error::Exhausted)?;
    for value in source
    {
        values.try_reserve(1).map_err(|_| Error::Exhausted)?;
        values.push(value?);
    }
    Ok(values)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State
{
    Initializing,
    Ready,
    Quiescing,
    Failed
}
struct Module
{
    name:             modules::Name,
    identity:         [u8; 16],
    kind:             PackageKind,
    state:            State,
    offers:           Vec<Offer>,
    dependencies:     Vec<mac_module::Dependency>,
    dependency_ids:   Vec<u64>,
    services:         Vec<Service>,
    devices:          Vec<mac_module::DeviceMatch>,
    requests:         usize,
    file_handlers:    Vec<mac_module::file_handlers::Rule>,
    cleanup:          Option<crate::toolbox::handles::cleanup::Checkpoint>,
    account:          ModuleAccount,
    rollback_pending: bool,
    recovery_refusal: Option<Error>,
    startup:          Startup
}
struct Startup
{
    phase:  u32,
    status: i32,
    cause:  [u8; mac_module::lifecycle::MAX_CAUSE],
    length: usize
}
impl Default for Startup
{
    fn default() -> Self
    {
        Self { phase:  mac_module::lifecycle::SYNCHRONOUS,
               status: 0,
               cause:  [0; mac_module::lifecycle::MAX_CAUSE],
               length: 0 }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interface
{
    pub module: u64,
    pub offer:  Offer
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request
{
    origin:        requests::Origin,
    pub module:    u64,
    pub interface: u64,
    pub caller:    u64,
    pub class:     ClassId,
    pub operation: u32,
    pub max_reply: u32,
    interface_key: mac_object_table::domain::ResourceKey,
    offer:         mac_module::Offer
}
impl Request
{
    pub(crate) fn origin(self) -> requests::Origin
    {
        self.origin
    }
}
impl crate::toolbox::inspection::Subject for Module
{
    fn label(&self) -> &str
    {
        self.name.text()
    }

    fn foreground(&self) -> bool
    {
        false
    }

    fn execution_account(&self) -> Option<mac_kernel_model::account::Snapshot>
    {
        Some(self.account.snapshot())
    }

    fn authority(&self) -> u64
    {
        0
    }
}
pub struct Registry
{
    modules:    modules::Modules<Module>,
    interfaces: Store<Interface>,
    system:     OwnerRef,
    requests:   requests::Requests,
    dependencies: Store<dependencies::Binding>,
    #[cfg(any(test, feature = "test-support"))]
    fixture_catalog: Option<alloc::sync::Arc<crate::toolbox::handles::Registry<dyn crate::toolbox::handles::cleanup::context::Context>>>,
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fixture_operations: Option<alloc::sync::Arc<crate::drivers::jobs::subjects::Operations>>,
    target:     Target,
    services:   Vec<Service>,
    module_kernel_bytes: u64
}
impl Registry
{
    pub(crate) fn inspect<T: crate::toolbox::inspection::Subject>(&self,
                                                                  processes:&mac_object_table::domain::Directory<T,crate::toolbox::handles::InvocationGate>,
                                                                  caller: u64,
                                                                  call:&mut mac_abi::inspection::Call)
                                                                  -> mac_abi::Status
    {
        crate::toolbox::inspection::request_with_modules(processes,
                                                         self.modules.directory(),
                                                         caller,
                                                         call)
    }

    pub(crate) fn new(target: Target, system: OwnerRef, kinds: [KindLease; 6],
                      module_kernel_bytes: u64)
                      -> Result<Self>
    {
        let [interfaces,
             interface_storage,
             dependencies,
             dependency_storage,
             requests,
             request_storage] = kinds;
        Ok(Self { modules: modules::Modules::new(),
                  interfaces: Store::owned(interfaces, interface_storage).map_err(Error::Owner)?,
                  dependencies:
                      Store::owned(dependencies, dependency_storage).map_err(Error::Owner)?,
                  #[cfg(any(test, feature = "test-support"))]
                  fixture_catalog: None,
                  #[cfg(any(test, feature = "test-support"))]
                  fixture_operations: None,
                  system,
                  requests: requests::Requests::new((requests, request_storage))?,
                  target,
                  services: Vec::new(),
                  module_kernel_bytes })
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn release_request(
        &mut self, ticket: mac_object_table::domain::ReleasedResource, storage: bool)
        -> core::result::Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        self.requests.release(ticket, storage)
    }

    pub(crate) fn take_request_retirement(&mut self) -> (Option<requests::Retirement>, bool)
    {
        self.requests.take_retirement()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn fixture_subject(&self, caller: u64)
                                  -> Result<crate::drivers::jobs::subjects::Subject>
    {
        let owner = if caller == 0 { self.system.clone() } else { self.module_ledger(caller)? };
        let operations = self.fixture_operations.as_ref().ok_or(Error::State)?;
        let target = if caller == 0
        {
            crate::allocation::Target::System(crate::allocation::SystemClass::Ordinary)
        }
        else
        {
            crate::allocation::Target::Owner { account: self.module_allocation(caller)?,
                                               class:
                                                   mac_kernel_model::policy::MemoryClass::Kernel }
        };
        operations.admit(&crate::toolbox::handles::subject::Authority::Client(owner), target)
                  .map_err(|status| {
                      if status == mac_abi::NO_MEMORY { Error::Exhausted } else { Error::State }
                  })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn begin(&mut self, caller: u64, interface: u64, operation: u32, input: &[u8],
                        output_len: usize)
                        -> Result<u64>
    {
        let subject = self.fixture_subject(caller)?;
        let mut preparation = requests::dispatch::prepare(&subject, requests::Origin::Direct, input, output_len)
            .map_err(Error::AllocationScope)?;
        self.begin_scoped(&subject,
                          caller,
                          interface,
                          operation,
                          input,
                          output_len,
                          requests::Origin::Direct,
                          &mut preparation)
    }

    /// Resident services use the same capability/request machinery as modules.
    /// Provider owner zero denotes the resident kernel, not an issued module ID.
    pub fn register_service(&mut self, offer: Offer) -> Result<u64>
    {
        if offer.version == 0
           || offer.request_schema == 0
           || offer.reply_schema == 0
           || offer.max_request > 1024 * 1024
           || offer.max_reply > 1024 * 1024
           || offer.flags & Offer::KERNEL_ONLY == 0
        {
            return Err(Error::Contract(mac_module::Error::Limit));
        }
        if self.services.iter().any(|s| s.service == offer.class && s.version == offer.version)
        {
            return Err(Error::Duplicate);
        }
        if self.interfaces.iter().any(|(_, i)| {
                                     i.offer.class == offer.class
                                     && (i.offer.flags | offer.flags) & Offer::EXCLUSIVE != 0
                                 })
           || self.modules.iter().any(|(_, m)| {
                                     m.state != State::Failed
                                     && m.offers.iter().any(|i| {
                                                           i.class == offer.class
                                                           && (i.flags | offer.flags)
                                                              & Offer::EXCLUSIVE
                                                              != 0
                                                       })
                                 })
        {
            return Err(Error::Exclusive);
        }
        InvocationGate::with_system_allocation(|| self.services.try_reserve(1)).map_err(|_| {
                                                                                   Error::Exhausted
                                                                               })?;
        let id = self.interfaces
                     .insert(&self.system,
                             Interface { module: 0,
                                         offer })
                     .map_err(Error::Owner)?;
        self.services.push(Service { service:  offer.class,
                                     version:  offer.version,
                                     optional: false });
        Ok(id)
    }

    pub fn device_allowed(&self, module: u64, bus: ClassId, key: [u8; 16]) -> Result<()>
    {
        let record = self.modules.get(module).ok_or(Error::Stale)?;
        if !matches!(record.state, State::Initializing | State::Ready)
        {
            return Err(Error::State);
        }
        if record.kind != PackageKind::Driver
        {
            return Err(Error::Access);
        }
        if !record.devices.iter().any(|d| d.matches(bus, key))
        {
            return Err(Error::Access);
        }
        Ok(())
    }

    #[cfg(any(target_arch = "riscv64", test))]
    pub(crate) fn dependency_available(&self, dependency: mac_module::Dependency) -> bool
    {
        self.interfaces.iter().any(|(_, interface)| {
                                  self.modules.get(interface.module).is_some_and(|module| {
                                                                        module.state == State::Ready
                                                                        && (dependency.module
                                                                            == [0; 16]
                                                                            || dependency.module
                                                                               == module.identity)
                                                                        && interface.offer.class
                                                                           == dependency.class
                                                                        && interface.offer.version
                                                                           == dependency.version
                                                                    })
                              })
    }

    pub fn admit(&self, contract: Contract<'_>, directory: PackageKind, file_type: [u8; 4])
                 -> Result<()>
    {
        let providers = InvocationGate::with_system_allocation(|| {
            records(self.interfaces.iter().filter_map(|(_, interface)| {
                let module = self.modules.get(interface.module)?;
                (module.state == State::Ready).then_some(Ok(Available {
                    module: module.identity, class: interface.offer.class,
                    version: interface.offer.version
                }))
            }))
        })?;
        Policy { directory,
                 file_type,
                 kernel_abi: mac_abi::ABI_VERSION,
                 target: self.target,
                 max_heap: mac_module::MAX_HEAP_BYTES,
                 max_stack: mac_module::MAX_STACK_BYTES,
                 services: &self.services,
                 providers: &providers }.validate(contract)?;
        if self.modules.iter().any(|(_, m)| m.identity == contract.identity)
        {
            return Err(Error::Duplicate);
        }
        Ok(())
    }

    /// Stage only validated metadata. Loading bytes and entering code are the
    /// caller's next steps; neither can publish interfaces before commit().
    pub fn stage(&mut self, contract: Contract<'_>, directory: PackageKind, file_type: [u8; 4])
                 -> Result<u64>
    {
        self.admit(contract, directory, file_type)?;
        for offer in contract.offers()
        {
            let offer = offer?;
            if self.interfaces.iter().any(|(_, i)| {
                                         i.module == 0
                                         && i.offer.class == offer.class
                                         && (i.offer.flags | offer.flags) & Offer::EXCLUSIVE != 0
                                     })
               || self.modules.iter().any(|(_, m)| {
                                         m.state != State::Failed
                                         && m.offers.iter().any(|existing| {
                                                               existing.class == offer.class
                                                               && (existing.flags | offer.flags)
                                                                  & Offer::EXCLUSIVE
                                                                  != 0
                                                           })
                                     })
            {
                return Err(Error::Exclusive);
            }
        }
        let ledger =
            OwnerRef::issue(mac_object_table::domain::DomainKind::Module).map_err(|error| {
                                                                             match error {
                mac_object_table::domain::IndexError::Identity(error) => Error::Identity(error),
                mac_object_table::domain::IndexError::AdmissionStorage { .. }
                | mac_object_table::domain::IndexError::SharedStorage { .. } => Error::Exhausted,
                mac_object_table::domain::IndexError::Pool(error) => Error::Owner(error)
            }
                                                                         })?;
        let kernel = ConfiguredRange::new(0, self.module_kernel_bytes, self.module_kernel_bytes)
            .map_err(|_| Error::State)?;
        let application = ConfiguredRange::new(0, 0, 0).map_err(|_| Error::State)?;
        let account =
            ModuleAccount::try_new(ledger.clone(), kernel, application).map_err(Error::Account)?;
        let allocation = account.admit_use().map_err(Error::Account)?;
        let record = crate::allocation::with_kernel_allocation(allocation, || {
                         let offers = records(contract.offers())?;
                         let dependencies = records(contract.dependencies())?;
                         let mut dependency_ids = Vec::new();
                         dependency_ids.try_reserve_exact(dependencies.len())
                                       .map_err(|_| Error::Exhausted)?;
                         let services = records(contract.services())?;
                         let devices = records(contract.devices())?;
                         let name = modules::Name::new(contract.name)?;
                         let cleanup = crate::toolbox::handles::cleanup::Checkpoint::try_new()
                .map_err(|_| Error::Exhausted)?;
                         Ok::<_, Error>(Module { name,
                                                 identity: contract.identity,
                                                 kind: contract.kind,
                                                 state: State::Initializing,
                                                 offers,
                                                 dependencies,
                                                 dependency_ids,
                                                 services,
                                                 devices,
                                                 requests: 0,
                                                 file_handlers: Vec::new(),
                                                 cleanup: Some(cleanup),
                                                 account,
                                                 rollback_pending: false,
                                                 recovery_refusal: None,
                                                 startup: Startup::default() })
                     }).map_err(Error::AllocationScope)??;
        let module = self.modules.insert(ledger, record)?;
        let bound = match self.module_allocation(module)
        {
            Ok(allocation) =>
            {
                crate::allocation::with_kernel_allocation(allocation, || {
                    self.bind_dependencies(module)
                }).map_err(Error::AllocationScope)
                  .and_then(core::convert::identity)
            }
            Err(error) => Err(error)
        };
        if let Err(error) = bound
        {
            self.rollback(module)?;
            return Err(error);
        }
        Ok(module)
    }

    /// Transactional publication: reserve every interface first and roll back
    /// temporary registrations on exhaustion. Initializing modules stay hidden.
    pub fn commit(&mut self, module: u64) -> Result<()>
    {
        let allocation = self.module_allocation(module)?;
        crate::allocation::with_kernel_allocation(allocation, || self.commit_selected(module))
            .map_err(Error::AllocationScope)?
    }

    fn commit_selected(&mut self, module: u64) -> Result<()>
    {
        let record = self.modules.get(module).ok_or(Error::Stale)?;
        if record.state != State::Initializing
        {
            return Err(Error::State);
        }
        if self.requests.iter().any(|(_, r)| r.caller == module)
        {
            return Err(Error::Busy);
        }
        if record.startup.phase == mac_module::lifecycle::PENDING
        {
            // This commits execution/admission only. No provisional interface
            // is minted or discoverable before scheduled confirmation.
            self.modules.get_mut(module).ok_or(Error::Stale)?.state = State::Ready;
            return Ok(());
        }
        self.publish_interfaces(module)
    }

    fn publish_interfaces(&mut self, module: u64) -> Result<()>
    {
        let record = self.modules.get(module).ok_or(Error::Stale)?;
        if self.requests.iter().any(|(_, r)| r.caller == u64::from(module))
        {
            return Err(Error::Busy);
        }
        let mut published = Vec::new();
        published.try_reserve(record.offers.len()).map_err(|_| Error::Exhausted)?;
        let owner = self.modules.directory().get(module).ok_or(Error::Stale)?.ledger().clone();
        for &offer in &record.offers
        {
            match self.interfaces.insert(&owner,
                                         Interface { module,
                                                     offer })
            {
                Ok(id) => published.push(id),
                Err(error) =>
                {
                    for id in published
                    {
                        self.interfaces.remove(id);
                    }
                    return Err(Error::Owner(error));
                }
            }
        }
        let record = self.modules.get_mut(module).ok_or(Error::Stale)?;
        record.state = State::Ready;
        record.startup.phase = mac_module::lifecycle::ACTIVE;
        record.startup.status = 0;
        record.startup.length = 0;
        Ok(())
    }

    pub(crate) fn startup_phase(&self, module: u64) -> Result<u32>
    {
        Ok(self.modules.get(module).ok_or(Error::Stale)?.startup.phase)
    }

    pub(crate) fn preference_subject(&self, module: u64) -> Result<(OwnerRef, [u8; 16])>
    {
        let record = self.modules.get(module).ok_or(Error::Access)?;
        if !matches!(record.state, State::Initializing | State::Ready)
        {
            return Err(Error::State);
        }
        Ok((self.module_ledger(module)?, record.identity))
    }

    pub(crate) fn startup_call(&mut self, module: u64, operation: u32, input: &[u8],
                               output: &mut [u8])
                               -> Result<usize>
    {
        use mac_module::lifecycle as wire;
        if output.len() < wire::REPLY_BYTES
        {
            return Err(Error::Contract(mac_module::Error::Limit));
        }
        let record = self.modules.get(module).ok_or(Error::Access)?;
        if !matches!(record.state, State::Initializing | State::Ready)
        {
            return Err(Error::State);
        }
        match operation
        {
            wire::DEFER
                if input.is_empty()
                   && record.state == State::Initializing
                   && record.startup.phase == wire::SYNCHRONOUS =>
            {
                self.modules.get_mut(module).ok_or(Error::Stale)?.startup.phase = wire::PENDING;
            }
            wire::CONFIRM
                if input.is_empty()
                   && record.state == State::Ready
                   && record.startup.phase == wire::ACTIVE =>
            {
                ()
            }
            wire::CONFIRM
                if input.is_empty()
                   && record.state == State::Ready
                   && matches!(record.startup.phase, wire::PENDING | wire::REFUSED) =>
            {
                let allocation = self.module_allocation(module)?;
                crate::allocation::with_kernel_allocation(allocation, || {
                    self.publish_interfaces(module)
                }).map_err(Error::AllocationScope)??;
                #[cfg(target_arch = "riscv64")]
                serial!("MODULE_ASYNC_READY name={} id={}",
                        self.modules.get(module).ok_or(Error::Stale)?.name.text(),
                        module);
            }
            wire::STATUS if input.is_empty() =>
            {
                output[..4].copy_from_slice(&record.startup.phase.to_le_bytes());
                output[4..8].copy_from_slice(&record.startup.status.to_le_bytes());
                output[8..8 + record.startup.length].copy_from_slice(&record.startup.cause
                                                                         [..record.startup.length]);
                return Ok(8 + record.startup.length);
            }
            wire::REFUSE | wire::REPORT
                if input.len() > 4 && input.len() <= 4 + wire::MAX_CAUSE =>
            {
                let status =
                    i32::from_le_bytes(input[..4].try_into().map_err(|_| Error::Access)?);
                let cause = core::str::from_utf8(&input[4..]).map_err(|_| Error::Access)?;
                if status > 0 || cause.chars().any(char::is_control)
                {
                    return Err(Error::Access);
                }
                if operation == wire::REFUSE
                {
                    if status == 0
                       || record.state != State::Ready
                       || !matches!(record.startup.phase, wire::PENDING | wire::REFUSED)
                    {
                        return Err(Error::State);
                    }
                    let record = self.modules.get_mut(module).ok_or(Error::Stale)?;
                    record.startup.phase = wire::REFUSED;
                }
                let record = self.modules.get_mut(module).ok_or(Error::Stale)?;
                record.startup.status = status;
                record.startup.length = cause.len();
                record.startup.cause[..cause.len()].copy_from_slice(cause.as_bytes());
                #[cfg(target_arch = "riscv64")]
                serial!("MODULE_ASYNC_STATUS name={} id={} status={} cause={}",
                        self.modules.get(module).ok_or(Error::Stale)?.name.text(),
                        module,
                        status,
                        cause);
            }
            _ => return Err(Error::State)
        }
        Ok(0)
    }

    pub fn rollback(&mut self, module: u64) -> Result<()>
    {
        let record = self.modules.get(module).ok_or(Error::Stale)?;
        if record.state != State::Initializing
           && !(record.state == State::Quiescing && record.rollback_pending)
        {
            return Err(Error::State);
        }
        let record = self.modules.get_mut(module).ok_or(Error::Stale)?;
        let first = !record.rollback_pending;
        record.rollback_pending = true;
        record.state = State::Quiescing;
        self.modules.stop(module)?;
        #[cfg(target_arch = "riscv64")]
        if first
        {
            crate::drivers::recovery_changed();
        }
        #[cfg(not(target_arch = "riscv64"))]
        let _ = first;
        let owner = self.module_ledger(module)?;
        self.requests.cancel_entry(module, owner.owner());
        let drained = self.drain_module(module);
        if self.requests.iter().any(|(_, r)| r.caller == u64::from(module))
        {
            return Err(Error::Busy);
        }
        drained?;
        self.drain_module_metadata(module)?;
        self.modules.remove(module)?;
        Ok(())
    }

    pub(crate) fn module_ledger(&self, module: u64) -> Result<OwnerRef>
    {
        self.modules.directory().get(module).map(|entry| entry.ledger().clone()).ok_or(Error::Stale)
    }

    pub(crate) fn module_allocation(&self, module: u64) -> Result<ModuleAllocation>
    {
        self.modules.get(module).ok_or(Error::Stale)?.account.admit_use().map_err(Error::Account)
    }

    pub fn state(&self, module: u64) -> Result<State>
    {
        Ok(self.modules.get(module).ok_or(Error::Stale)?.state)
    }

    pub fn kind(&self, module: u64) -> Result<PackageKind>
    {
        Ok(self.modules.get(module).ok_or(Error::Stale)?.kind)
    }

    /// Metadata is staged with its module. Matching never sees an initializing,
    /// failed or quiescing owner, including after generation-tagged slot reuse.
    pub fn install_file_handlers(&mut self, module: u64, rules: &[mac_module::file_handlers::Rule])
                                 -> Result<()>
    {
        if rules.len() > mac_module::file_handlers::MAX_RULES
        {
            return Err(Error::Exhausted);
        }
        let own = self.modules.get(module).ok_or(Error::Stale)?;
        if own.state != State::Initializing || !own.file_handlers.is_empty()
        {
            return Err(Error::State);
        }
        for (i, rule) in rules.iter().enumerate()
        {
            let conflicts =
                |other: &mac_module::file_handlers::Rule| other.file_type == rule.file_type;
            if rules[..i].iter().any(conflicts)
               || self.modules.iter().any(|(id, m)| {
                                         id != module
                                         && matches!(m.state, State::Initializing | State::Ready)
                                         && m.file_handlers.iter().any(conflicts)
                                     })
            {
                return Err(Error::Duplicate);
            }
        }
        let allocation = self.module_allocation(module)?;
        let owned = crate::allocation::with_kernel_allocation(allocation, || {
                        let mut owned = Vec::new();
                        owned.try_reserve_exact(rules.len()).map_err(|_| Error::Exhausted)?;
                        owned.extend_from_slice(rules);
                        Ok::<_, Error>(owned)
                    }).map_err(Error::AllocationScope)??;
        self.modules.get_mut(module).ok_or(Error::Stale)?.file_handlers = owned;
        Ok(())
    }

    pub fn file_handler(&self, file_type: [u8; 4])
                        -> Option<(u64, mac_module::file_handlers::Rule)>
    {
        for (id, module) in
            self.modules
                .iter()
                .filter(|(_, m)| {
                    m.state == State::Ready && m.startup.phase == mac_module::lifecycle::ACTIVE
                })
        {
            if let Some(rule) = module.file_handlers.iter().find(|r| r.file_type == file_type)
            {
                return Some((id, *rule));
            }
        }
        None
    }

    pub fn find(&self, class: ClassId, version: u32)
                -> impl Iterator<Item = (u64, Interface)> + '_
    {
        self.interfaces
            .iter()
            .filter(move |(_, i)| {
                i.offer.class == class
                && i.offer.version == version
                && (i.module == 0
                    || self.modules.get(i.module).is_some_and(|m| m.state == State::Ready))
            })
            .map(|(id, i)| (id, *i))
    }

    /// Kernel discovery distinguishes absence from a stale service contract.
    /// Never silently treat an installed incompatible provider as no hardware.
    pub fn select(&self, class: ClassId, version: u32, capabilities: u64) -> Result<Option<u64>>
    {
        let mut mismatch = false;
        for (id, interface) in self.interfaces.iter()
        {
            if interface.offer.class != class
               || interface.offer.capabilities & capabilities != capabilities
               || interface.module != 0
                  && !self.modules
                          .get(interface.module)
                          .is_some_and(|module| module.state == State::Ready)
            {
                continue;
            }
            if interface.offer.version == version
            {
                return Ok(Some(id));
            }
            mismatch = true;
        }
        if mismatch { Err(Error::Contract(mac_module::Error::Version)) } else { Ok(None) }
    }

    pub fn resolve(&self, caller: u64, class: ClassId, version: u32) -> Result<u64>
    {
        if caller == 0
        {
            return self.find(class, version).next().map(|(id, _)| id).ok_or(Error::Stale);
        }
        let module = self.modules.get(caller).ok_or(Error::Access)?;
        if !matches!(module.state, State::Initializing | State::Ready)
        {
            return Err(Error::State);
        }
        if module.services.iter().any(|s| s.service == class && s.version == version)
           && let Some((id, _)) = self.find(class, version).find(|(_, i)| i.module == 0)
        {
            return Ok(id);
        }
        for &id in &module.dependency_ids
        {
            let Some(binding) = self.dependencies.get(id)
            else
            {
                continue;
            };
            if binding.dependent == caller && binding.class == class && binding.version == version
            {
                self.interface(binding.interface)?;
                return Ok(binding.interface);
            }
        }
        if module.dependencies.iter().any(|d| d.class == class && d.version == version)
        {
            return Err(Error::Stale);
        }
        Err(Error::Access)
    }

    pub fn interface(&self, id: u64) -> Result<Interface>
    {
        let interface = *self.interfaces.get(id).ok_or(Error::Stale)?;
        if interface.module != 0
           && self.modules.get(interface.module).is_none_or(|m| m.state != State::Ready)
        {
            return Err(Error::State);
        }
        Ok(interface)
    }

    /// Validate a ticket-free resident call before dispatch. The resident owns
    /// the callback forever; no independently unloadable provider is admitted.
    pub(crate) fn authorize_synchronous(&self, subject: &crate::drivers::jobs::subjects::Subject,
                                        caller: u64, interface: u64, input_len: usize,
                                        output_len: usize)
                                        -> Result<Interface>
    {
        let provider = self.authorize_call(caller, interface, input_len, output_len)?;
        // A borrowed/inherited Subject is not the unique owner of its external
        // root. Authenticate that real scope even when there is no parent
        // request; dropping the external root revokes fresh leaf admission.
        // Subject stop alone does not revoke a still-open admitted operation.
        subject.scope().admitting().map_err(|cause| {
                                        Error::Owner(
            mac_object_table::domain::PoolError::Scope {
                owner: subject.scope().subject(), kind: subject.scope().kind(), cause
            })
                                    })?;
        if provider.module != 0
        {
            return Err(Error::Access);
        }
        self.requests.validate_parent(subject, caller, requests::Origin::Direct)?;
        Ok(provider)
    }

    pub(crate) fn authorize_call(&self, caller: u64, interface: u64, input_len: usize,
                                 output_len: usize)
                                 -> Result<Interface>
    {
        let provider = self.interface(interface)?;
        if input_len > provider.offer.max_request as usize
           || output_len > provider.offer.max_reply as usize
        {
            return Err(Error::Contract(mac_module::Error::Limit));
        }
        if caller != 0
        {
            let owner = self.modules.get(caller).ok_or(Error::Access)?;
            if !matches!(owner.state, State::Initializing | State::Ready)
            {
                return Err(Error::State);
            }
            let allowed = if provider.module == 0
            {
                owner.services
                     .iter()
                     .any(|s| {
                         s.service == provider.offer.class && s.version == provider.offer.version
                     })
            }
            else
            {
                owner.dependency_ids
                     .iter()
                     .filter_map(|&id| self.dependencies.get(id))
                     .any(|binding| binding.dependent == caller && binding.interface == interface)
            };
            if !allowed
            {
                return Err(Error::Access);
            }
        }
        Ok(provider)
    }

    /// Caller 0 is resident System. Module callers are limited to dependencies
    /// declared in their own admitted contract; applications use class services.
    pub(crate) fn begin_scoped(&mut self, subject: &crate::drivers::jobs::subjects::Subject,
                               caller: u64, interface: u64, operation: u32, input: &[u8],
                               output_len: usize, origin: requests::Origin,
                               preparation: &mut requests::dispatch::Preparation)
                               -> Result<u64>
    {
        let storage = preparation.storage().map_err(Error::AllocationScope)?;
        if !storage.fits(input.len(), output_len)
        {
            return Err(Error::Contract(mac_module::Error::Limit));
        }
        let provider = self.authorize_call(caller, interface, input.len(), output_len)?;

        if provider.offer.flags & Offer::SERIALIZED != 0 && self.requests.busy(interface)
        {
            return Err(Error::Busy);
        }
        let count = if provider.module == 0
        {
            0
        }
        else
        {
            self.modules
                .get(provider.module)
                .ok_or(Error::Stale)?
                .requests
                .checked_add(1)
                .ok_or(Error::Exhausted)?
        };
        let ticket = self.requests.insert(subject,
                                           Request { origin,
                                                     module: provider.module,
                                                     interface,
                                                     caller,
                                                     class: provider.offer.class,
                                                     operation,
                                                     max_reply: output_len as u32,
                                                     interface_key: self.interfaces
                                                                        .key(interface)
                                                                        .ok_or(Error::Stale)?,
                                                     offer: provider.offer },
                                           preparation)?;
        if provider.module != 0
        {
            self.modules.get_mut(provider.module).ok_or(Error::Stale)?.requests = count;
        }
        Ok(ticket)
    }

    pub(crate) fn request<'a>(&self, caller: u64, access: impl Into<requests::Access<'a>>)
                              -> Result<Request>
    {
        let access = access.into();
        let request = self.requests.get(access)?;
        if request.caller != caller
        {
            return Err(Error::Access);
        }
        if caller != 0
           && self.modules.get(caller).is_none_or(|m| {
                                          !matches!(m.state,
                                                    State::Initializing
                                                    | State::Ready
                                                    | State::Quiescing)
                                      })
        {
            return Err(Error::Access);
        }

        if request.module != 0
           && !self.requests.terminal_reply(access)?
           && self.modules
                  .get(request.module)
                  .is_none_or(|m| !matches!(m.state, State::Ready | State::Quiescing))
        {
            return Err(Error::State);
        }
        Ok(request)
    }

    pub(crate) fn interface_for_request<'a>(&self, caller: u64,
                                            access: impl Into<requests::Access<'a>>)
                                            -> Result<Interface>
    {
        let request = self.request(caller, access)?;
        Ok(Interface { module: request.module,
                       offer:  request.offer })
    }

    pub(crate) fn request_dispatch<'a>(&self, caller: u64, access: impl Into<requests::Access<'a>>)
                                       -> Result<alloc::sync::Arc<requests::dispatch::Storage>>
    {
        let access = access.into();
        self.request(caller, access)?;
        self.requests.dispatch(access)
    }

    pub(crate) fn request_invocation<'a>(&self, caller: u64,
                                         access: impl Into<requests::Access<'a>>)
                                         -> Result<crate::drivers::jobs::subjects::Subject>
    {
        let access = access.into();
        self.request(caller, access)?;
        self.requests.invoking(access)
    }

    pub(crate) fn next_cancellation(&mut self) -> Result<Option<requests::cancellation::Plan>>
    {
        self.requests.next_cancellation()
    }

    pub(crate) fn return_cancellation(
        &mut self, plan: requests::cancellation::Plan)
        -> core::result::Result<(), requests::cancellation::Refusal>
    {
        self.requests.return_cancellation(plan)
    }

    pub(crate) fn retain_uncertain_cancellation(
        &mut self, plan: requests::cancellation::Plan, status: mac_abi::Status)
        -> core::result::Result<(), requests::cancellation::Refusal>
    {
        self.requests.retain_uncertain_cancellation(plan, status)
    }

    pub(crate) fn acknowledge_cancellation(
        &mut self, plan: requests::cancellation::Plan, status: mac_abi::Status, length: usize)
        -> core::result::Result<requests::cancellation::Acknowledged,
                                requests::cancellation::Refusal>
    {
        let request = plan.request();
        let count = if request.module != 0
        {
            let Some(module) = self.modules.get_mut(request.module)
            else
            {
                return Err(requests::cancellation::Refusal { cause: Error::Stale,
                                                             plan });
            };
            let Some(remaining) = module.requests.checked_sub(1)
            else
            {
                return Err(requests::cancellation::Refusal { cause: Error::State,
                                                             plan });
            };
            Some((&mut module.requests, remaining))
        }
        else
        {
            None
        };
        let result = self.requests.acknowledge_cancellation(plan, status, length)?;
        if let Some((count, remaining)) = count
        {
            *count = remaining;
        }
        Ok(result)
    }

    pub(crate) fn rollback_after(&self, previous: u64) -> Option<u64>
    {
        self.modules
            .iter()
            .filter_map(|(id, module)| (id > previous && module.rollback_pending).then_some(id))
            .min()
    }

    pub(crate) fn recovery_refusal(&mut self, module: u64, cause: Error) -> Option<&str>
    {
        let module = self.modules.get_mut(module)?;
        if module.recovery_refusal == Some(cause)
        {
            return None;
        }
        module.recovery_refusal = Some(cause);
        Some(module.name.text())
    }

    /// Extract the actual completion guard for synchronous caller ACK. Its
    /// payload and cancellation backing must be destroyed outside Registry.
    pub(crate) fn complete_dispatch<'a>(&mut self, caller: u64,
                                        access: impl Into<requests::Access<'a>>, output_len: usize)
                                        -> Result<requests::Retirement>
    {
        let access = access.into();
        self.request(caller, access)?;
        if !self.requests.terminal_reply(access)? || !self.requests.exclusive_dispatch(access)?
        {
            return Err(Error::Busy);
        }
        self.complete(caller, access, output_len)?;
        self.requests.take_completed(access)
    }

    /// Release on terminal success, failure or acknowledged cancellation only.
    /// PENDING retains the ticket; late or foreign completions cannot reuse it.
    pub(crate) fn complete<'a>(&mut self, caller: u64, access: impl Into<requests::Access<'a>>,
                               output_len: usize)
                               -> Result<Request>
    {
        let access = access.into();
        let request = self.request(caller, access)?;
        if self.requests.is_active(access)?
        {
            return Err(Error::Busy);
        }
        if output_len > request.max_reply as usize
        {
            return Err(Error::Contract(mac_module::Error::Limit));
        }
        if self.requests.completed(access)?
        {
            self.requests.finish(access)?;
            return Ok(request);
        }
        let count = if request.module != 0
        {
            Some(self.modules
                     .get(request.module)
                     .ok_or(Error::Stale)?
                     .requests
                     .checked_sub(1)
                     .ok_or(Error::State)?)
        }
        else
        {
            None
        };
        self.requests.finish(access)?;
        if let Some(count) = count
        {
            self.modules.get_mut(request.module).ok_or(Error::Stale)?.requests = count;
        }
        Ok(request)
    }

    /// Withdraw before asking the module to quiesce; no new requests enter.
    pub fn quiesce(&mut self, module: u64) -> Result<()>
    {
        let record = self.modules.get_mut(module).ok_or(Error::Stale)?;
        if record.state != State::Ready
        {
            return Err(Error::State);
        }
        record.state = State::Quiescing;
        self.modules.stop(module)?;
        Ok(())
    }

    /// Pin code until reboot even after failure. Outstanding requests require
    /// terminal cancellation before failure is finalized.
    pub fn finish_quiesce(&mut self, module: u64) -> Result<()>
    {
        self.reap_closed_dependencies();
        let record = self.modules.get_mut(module).ok_or(Error::Stale)?;
        if record.state != State::Quiescing
        {
            return Err(Error::State);
        }
        if record.requests != 0 || self.requests.iter().any(|(_, r)| r.caller == u64::from(module))
        {
            return Err(Error::Busy);
        }
        if let Some(binding) = self.dependencies.retained_values().find(|b| b.provider == module)
        {
            return Err(if binding.scope.is_some()
                       {
                           Error::BackingInUse { provider:  module,
                                                 dependent: binding.dependent }
                       }
                       else
                       {
                           Error::DependencyInUse { provider:  module,
                                                    dependent: binding.dependent }
                       });
        }
        self.drain_module(module)?;
        self.drain_module_metadata(module)?;
        self.modules.get_mut(module).ok_or(Error::Stale)?.state = State::Failed;
        Ok(())
    }
}

#[cfg(test)]
mod tests
{
    use alloc::string::ToString;

    use mac_module::{BROKER_ABI, Spec};

    use super::*;
    pub(super) const OFFER: Offer = Offer { class:          ClassId(*b"TEST"),
                                            version:        1,
                                            request_schema: 1,
                                            reply_schema:   1,
                                            max_request:    16,
                                            max_reply:      16,
                                            flags:          0,
                                            capabilities:   0 };
    pub(super) fn metadata(identity: [u8; 16], offers: &[Offer],
                           dependencies: &[mac_module::Dependency])
                           -> Vec<u8>
    {
        metadata_with_requirements(identity, offers, dependencies, &[], &[])
    }
    fn metadata_with_requirements(identity: [u8; 16], offers: &[Offer],
                                  dependencies: &[mac_module::Dependency], services: &[Service],
                                  devices: &[mac_module::DeviceMatch])
                                  -> Vec<u8>
    {
        let spec = Spec { kind: PackageKind::Driver,
                          kernel_abi: mac_abi::ABI_VERSION,
                          target: Target::RVA23U64,
                          identity,
                          code_id: 0,
                          code_digest: [0; 32],
                          heap_bytes: 0,
                          stack_bytes: 4096,
                          name: "Test",
                          version: "0.1.0",
                          provenance: "unit",
                          offers,
                          dependencies,
                          services,
                          devices };
        let mut bytes = alloc::vec![0;spec.encoded_len().unwrap()];
        spec.encode(&mut bytes).unwrap();
        bytes
    }
    pub(super) fn stage(registry: &mut Registry, bytes: &[u8]) -> u64
    {
        registry.stage(Contract::decode(bytes).unwrap(), PackageKind::Driver, *b"DRVR").unwrap()
    }
    #[test]
    fn interface_slots_reuse_full_width_tokens_and_refuse_wrong_cleanup_context()
    {
        use mac_object_table::registry::CleanupFailure;

        use crate::toolbox::handles::cleanup::{Cause, context::Context};
        let bytes = metadata(*b"interface-reuse!", &[OFFER], &[]);
        let mut registry = fixture::Fixture::new(Target::RVA23U64);
        let module = stage(&mut registry, &bytes);
        registry.commit(module).unwrap();
        let id = registry.resolve(0, OFFER.class, OFFER.version).unwrap();
        let owner = registry.module_ledger(module).unwrap();
        let first = registry.interfaces.key(id).unwrap();
        assert_eq!(first.owner(), owner.owner());
        assert!(id > u64::from(u32::MAX));
        drop(registry.interfaces.remove(id).unwrap());
        let fresh = registry.interfaces
                            .insert(&owner,
                                    Interface { module,
                                                offer: OFFER })
                            .unwrap();
        let second = registry.interfaces.key(fresh).unwrap();
        assert_eq!(first.slot(), second.slot());
        assert_ne!(first.generation(), second.generation());
        assert_ne!(id, fresh);
        assert_eq!(registry.interface(id), Err(Error::Stale));
        assert!(registry.interface(fresh).is_ok());
        struct Missing;
        impl Context for Missing
        {
            fn driver_registry(&mut self) -> Option<&mut Registry>
            {
                None
            }
        }
        let ticket = owner.invalidate(second).unwrap();
        let dispatch = registry._kinds.dispatch(second.kind()).unwrap();
        let error = dispatch.invoke(&mut Missing, ticket).unwrap_err();
        assert!(error.to_string().contains("driver registry")
                && error.to_string().contains("retained for retry"));
        let (ticket, cause) = error.into_parts();
        assert!(matches!(cause, Cause::ContextUnavailable { .. }));
        assert_eq!(ticket.key(), second);
        owner.quarantine(ticket).unwrap();
        let ticket = owner.resume_quarantined().unwrap();
        let dispatch = registry._kinds.dispatch(second.kind()).unwrap();
        dispatch.invoke(&mut *registry as &mut dyn Context, ticket).unwrap();
        assert!(registry.interface(fresh).is_err());
        assert_eq!(owner.summary().pending, 0);
    }

    #[test]
    fn module_domain_retirement_preserves_live_and_pending_resource_authority()
    {
        use mac_object_table::domain::{CapacityPolicy, DirectoryError, DomainKind, KindCode};
        let bytes = metadata(*b"domain-test-uuid", &[], &[]);
        let mut registry = fixture::Fixture::new(Target::RVA23U64);
        let id = stage(&mut registry, &bytes);
        assert!(id > u64::from(u32::MAX));
        let owner = registry.modules.directory().get(id).unwrap().ledger().clone();
        assert_eq!(owner.owner().kind(), DomainKind::Module);
        let kind = KindCode(*b"MTST");
        owner.add_pool(kind, CapacityPolicy::GrowableUntilMemory).unwrap();
        owner.grow(kind, 1).unwrap();
        let key = owner.allocate(kind).unwrap();
        let refused = registry.rollback(id).unwrap_err();
        assert!(matches!(refused,
                         Error::Domain(DirectoryError::PendingResources { pending: 1,
                                                                          .. })));
        assert!(refused.to_string().contains("still pending"));
        assert_eq!(registry.state(id), Ok(State::Quiescing));
        assert_eq!((owner.summary().live, owner.summary().quarantined), (0, 1));
        let ticket = owner.resume_quarantined().unwrap();
        assert_eq!(ticket.key(), key);
        let pending = registry.rollback(id).unwrap_err();
        assert!(matches!(pending,
                         Error::Domain(DirectoryError::PendingResources { pending: 1,
                                                                          .. })));
        assert!(pending.to_string().contains("still pending"));
        owner.complete_release(ticket).unwrap();
        registry.rollback(id).unwrap();
        assert!(!owner.accepting());
        assert_eq!(registry.state(id), Err(Error::Stale));
        let fresh = stage(&mut registry, &bytes);
        assert_ne!(fresh, id);
        assert_eq!(registry.state(id), Err(Error::Stale));
        assert_eq!(registry.state(fresh), Ok(State::Initializing));
    }

    #[test]
    fn file_associations_follow_module_publication_and_withdrawal()
    {
        use mac_module::file_handlers::{Action, Rule};
        let bytes = metadata(*b"test-module-uuid", &[OFFER], &[]);
        let rule = Rule::new(*b"SIT!", Action::MountFilesystem, 128).unwrap();
        let mut registry = fixture::Fixture::new(Target::RVA23U64);
        let first = stage(&mut registry, &bytes);
        registry.install_file_handlers(first, &[rule]).unwrap();
        assert_eq!(registry.file_handler(*b"SIT!"), None);
        registry.rollback(first).unwrap();
        let module = stage(&mut registry, &bytes);
        assert_ne!(first, module);
        registry.install_file_handlers(module, &[rule]).unwrap();
        registry.commit(module).unwrap();
        assert_eq!(registry.file_handler(*b"SIT!"), Some((module, rule)));
        assert_eq!(registry.file_handler(*b"BINA"), None);
        assert_eq!(registry.file_handler(*b"APPL"), None);
        assert_eq!(registry.file_handler(*b"BINA"), None);
        registry.quiesce(module).unwrap();
        assert_eq!(registry.file_handler(*b"SIT!"), None);
        registry.finish_quiesce(module).unwrap();
        assert_eq!(registry.file_handler(*b"BINA"), None);
        // Failed module code stays pinned until reboot; a separate incarnation
        // without declarations cannot inherit its retired associations.
        let replacement = metadata(*b"next-module-uuid", &[OFFER], &[]);
        let module2 = stage(&mut registry, &replacement);
        registry.commit(module2).unwrap();
        assert_eq!(registry.file_handler(*b"SIT!"), None);
    }
    #[test]
    fn initialization_is_hidden_and_rollback_never_reuses_ids()
    {
        let bytes = metadata(*b"test-module-uuid", &[OFFER], &[]);
        let mut registry = fixture::Fixture::new(Target::RVA23U64);
        let stale = stage(&mut registry, &bytes);
        assert_eq!(registry.find(OFFER.class, 1).count(), 0);
        assert_eq!(registry.stage(Contract::decode(&bytes).unwrap(),
                                  PackageKind::Driver,
                                  *b"DRVR"),
                   Err(Error::Duplicate));
        registry.rollback(stale).unwrap();
        let live = stage(&mut registry, &bytes);
        assert_ne!(stale, live);
        assert_eq!(registry.state(stale), Err(Error::Stale));
        registry.commit(live).unwrap();
        assert_eq!(registry.find(OFFER.class, 1).count(), 1);
        assert_eq!(registry.rollback(live), Err(Error::State));
        let (interface, _) = registry.find(OFFER.class, 1).next().unwrap();
        assert!(u32::try_from(live).is_err(),
                "module incarnation was narrowed to a projected interface handle");
        assert_eq!(registry.state(u64::from(interface)), Err(Error::Stale));
    }
    #[test]
    fn request_reuse_foreign_completions_and_quiesce_are_checked()
    {
        let bytes = metadata(*b"test-module-uuid", &[OFFER], &[]);
        let mut registry = fixture::Fixture::new(Target::RVA23U64);
        let module = stage(&mut registry, &bytes);
        registry.commit(module).unwrap();
        let (interface, _) = registry.find(OFFER.class, 1).next().unwrap();
        let stale = registry.begin(0, interface, 7, &[0; 3], 10).unwrap();
        assert_eq!(registry.complete(123, stale, 0), Err(Error::Access));
        assert_eq!(registry.complete(0, stale, 11), Err(Error::Contract(mac_module::Error::Limit)));
        let lease = registry.request_dispatch(0, stale).unwrap().acquire().unwrap();
        assert_eq!(registry.request_dispatch(0, stale).unwrap().acquire().err(),
                   Some(mac_abi::SERVICE_BUSY));
        assert_eq!(registry.complete(0, stale, 0), Err(Error::Busy));
        drop(lease);
        registry.complete(0, stale, 10).unwrap();
        let live = registry.begin(0, interface, 7, &[0; 3], 10).unwrap();
        assert_ne!(live, stale);
        assert_eq!(registry.complete(0, stale, 0), Err(Error::Stale));
        registry.quiesce(module).unwrap();
        assert_eq!(registry.interface_for_request(0, live).unwrap().module, module);
        assert_eq!(registry.find(OFFER.class, 1).count(), 0);
        assert_eq!(registry.finish_quiesce(module), Err(Error::Busy));
        assert_eq!(registry.begin(0, interface, 7, &[], 0), Err(Error::State));
        registry.complete(0, live, 0).unwrap();
        registry.finish_quiesce(module).unwrap();
        assert_eq!(registry.interface(interface), Err(Error::Stale));
        assert_eq!(registry.state(module), Ok(State::Failed));
    }
    #[test]
    fn declared_dependencies_and_exclusive_claims_control_calls()
    {
        let bytes = metadata(*b"test-module-uuid",
                             &[Offer { flags: Offer::EXCLUSIVE | Offer::SERIALIZED,
                                       ..OFFER }],
                             &[]);
        let other = metadata(*b"other-module-id!", &[OFFER], &[]);
        let mut registry = fixture::Fixture::new(Target::RVA23U64);
        let provider = stage(&mut registry, &bytes);
        let dep = mac_module::Dependency { module:   *b"test-module-uuid",
                                           class:    OFFER.class,
                                           version:  1,
                                           optional: false };
        assert!(!registry.dependency_available(dep));
        assert_eq!(registry.stage(Contract::decode(&other).unwrap(),
                                  PackageKind::Driver,
                                  *b"DRVR"),
                   Err(Error::Exclusive));
        registry.commit(provider).unwrap();
        assert!(registry.dependency_available(dep));
        assert!(registry.dependency_available(mac_module::Dependency { module: [0; 16],
                                                                       ..dep }));
        assert!(!registry.dependency_available(mac_module::Dependency { version: 2,
                                                                        ..dep }));
        let (interface, _) = registry.find(OFFER.class, 1).next().unwrap();
        let consumer_bytes = metadata(*b"consumer-module!", &[], &[dep]);
        let consumer = stage(&mut registry, &consumer_bytes);
        let no_dep_bytes = metadata(*b"unrelated-id!!!!", &[], &[]);
        let unrelated = stage(&mut registry, &no_dep_bytes);
        assert_eq!(registry.begin(u64::from(unrelated), interface, 1, &[], 0), Err(Error::Access));
        let ticket = registry.begin(u64::from(consumer), interface, 1, &[], 0).unwrap();
        assert_eq!(registry.begin(0, interface, 1, &[], 0), Err(Error::Busy));
        registry.complete(u64::from(consumer), ticket, 0).unwrap();
        assert_eq!(BROKER_ABI, 1);
    }
    #[test]
    fn failed_initialization_retains_pending_dependency_ownership_until_cancellation()
    {
        let bytes = metadata(*b"test-module-uuid", &[OFFER], &[]);
        let mut registry = fixture::Fixture::new(Target::RVA23U64);
        let provider = stage(&mut registry, &bytes);
        registry.commit(provider).unwrap();
        let (interface, _) = registry.find(OFFER.class, 1).next().unwrap();
        let dependency = mac_module::Dependency { module:   [0; 16],
                                                  class:    OFFER.class,
                                                  version:  1,
                                                  optional: false };
        let bytes = metadata(*b"consumer-module!", &[], &[dependency]);
        let consumer = stage(&mut registry, &bytes);
        let subject = registry.fixture_subject(consumer).unwrap();
        let ticket =
            registry.begin_scoped(&subject,
                                  consumer,
                                  interface,
                                  1,
                                  &[],
                                  0,
                                  requests::Origin::Direct,
                                  &mut requests::dispatch::prepare(&subject,
                                                                   requests::Origin::Direct,
                                                                   &[],
                                                                   0).unwrap())
                    .unwrap();
        drop(subject);
        assert_eq!(registry.commit(consumer), Err(Error::Busy));
        assert_eq!(registry.rollback(consumer), Err(Error::Busy));
        assert_eq!(registry.state(consumer), Ok(State::Quiescing));
        assert_eq!(registry.request(consumer, ticket), Err(Error::Stale));
        registry.quiesce(provider).unwrap();
        let refused = registry.finish_quiesce(provider).unwrap_err();
        assert_eq!(refused, Error::Busy);
        assert!(refused.to_string().contains("actual completion"));
        let plan = registry.next_cancellation().unwrap().unwrap();
        assert_eq!(plan.ticket(), ticket);
        registry.acknowledge_cancellation(plan, mac_abi::OPERATION_CANCELLED, 0).unwrap();
        registry.rollback(consumer).unwrap();
        registry.finish_quiesce(provider).unwrap();
        assert_eq!(registry.state(consumer), Err(Error::Stale));
        assert_eq!(registry.state(provider), Ok(State::Failed));
    }

    #[test]
    fn backing_dependency_pins_exact_provider_until_the_last_owned_operation_ends()
    {
        use crate::toolbox::handles::subject::Authority;
        let mut registry = fixture::Fixture::new(Target::RVA23U64);
        let provider = stage(&mut registry, &metadata(*b"test-module-uuid", &[OFFER], &[]));
        registry.commit(provider).unwrap();
        let (interface, _) = registry.find(OFFER.class, 1).next().unwrap();
        let consumer = stage(&mut registry, &metadata(*b"consumer-module!", &[], &[]));
        registry.commit(consumer).unwrap();
        let owner = registry.module_ledger(consumer).unwrap();
        let operations = registry.fixture_operations.as_ref().unwrap().clone();
        let subject = operations.admit_current(&Authority::Client(owner)).unwrap();
        let last = subject.clone();
        registry.pin_backing_provider(consumer, interface, subject.scope()).unwrap();
        registry.pin_backing_provider(consumer, interface, subject.scope()).unwrap();
        assert_eq!(registry.dependencies.iter().count(),
                   1,
                   "same lease/provider is one registered claim");
        // Lifetime claims do not grant undeclared resolution/call authority.
        assert_eq!(registry.begin(consumer, interface, 1, &[], 0), Err(Error::Access));
        registry.quiesce(provider).unwrap();
        let error = registry.finish_quiesce(provider).unwrap_err();
        assert_eq!(error,
                   Error::BackingInUse { provider,
                                         dependent: consumer });
        assert!(error.to_string().contains("actual close acknowledgement"));
        drop(subject);
        registry.reap_closed_dependencies();
        assert_eq!(registry.dependencies.iter().count(),
                   1,
                   "retained callback still owns the root");
        drop(last);
        registry.reap_closed_dependencies();
        assert_eq!(registry.dependencies.iter().count(), 0);
        registry.finish_quiesce(provider).unwrap();
    }

    #[test]
    fn backing_dependency_scope_refusal_retains_the_unique_ticket_and_rejects_foreign_subjects()
    {
        use mac_object_table::registry::{CleanupFailure, Release};

        use crate::toolbox::handles::subject::Authority;
        let mut registry = fixture::Fixture::new(Target::RVA23U64);
        let provider = stage(&mut registry, &metadata(*b"test-module-uuid", &[OFFER], &[]));
        registry.commit(provider).unwrap();
        let (interface, _) = registry.find(OFFER.class, 1).next().unwrap();
        let consumer = stage(&mut registry, &metadata(*b"consumer-module!", &[], &[]));
        registry.commit(consumer).unwrap();
        let other = stage(&mut registry, &metadata(*b"unrelated-id!!!!", &[], &[]));
        registry.commit(other).unwrap();
        let operations = registry.fixture_operations.as_ref().unwrap().clone();
        let owner = registry.module_ledger(consumer).unwrap();
        let subject = operations.admit_current(&Authority::Client(owner.clone())).unwrap();
        let foreign = operations.admit_current(&Authority::Client(registry.module_ledger(other)
                                                                          .unwrap()))
                                .unwrap();
        assert_eq!(registry.pin_backing_provider(consumer, interface, foreign.scope()),
                   Err(Error::Access));
        registry.pin_backing_provider(consumer, interface, subject.scope()).unwrap();
        let id = registry.dependencies.iter().next().unwrap().0;
        let key = registry.dependencies.key(id).unwrap();
        let ticket = owner.invalidate(key).unwrap();
        let refused =
            dependencies::ReleaseDependencies(false).release(&mut *registry, ticket).unwrap_err();
        assert!(refused.to_string().contains("original cleanup ticket remain retained"));
        assert!(owner.pending(key));
        drop(subject);
        let (ticket, _) = refused.into_parts();
        dependencies::ReleaseDependencies(false).release(&mut *registry, ticket).unwrap();
        assert!(!owner.pending(key));
        drop(foreign);
    }

    #[test]
    fn refused_dependency_registration_rolls_back_the_unentered_module_domain()
    {
        use mac_object_table::domain::{KindCode, PoolError};
        let mut registry = fixture::Fixture::new(Target::RVA23U64);
        let provider = stage(&mut registry, &metadata(*b"test-module-uuid", &[OFFER], &[]));
        registry.commit(provider).unwrap();
        let before = registry.modules.iter().count();
        let registrar = registry.system.clone();
        drop(registry.fixture_catalog.take());
        let withdrawn =
            alloc::sync::Arc::get_mut(&mut registry._kinds).unwrap()
                                                           .unregister(&registrar,
                                                                       KindCode(*b"DDEP"))
                                                           .unwrap();
        drop(withdrawn);
        registry.fixture_catalog = Some(registry._kinds.clone());
        let declaration = mac_module::Dependency { module:   [0; 16],
                                                   class:    OFFER.class,
                                                   version:  1,
                                                   optional: false };
        let bytes = metadata(*b"consumer-module!", &[], &[declaration]);
        let error =
            registry.stage(Contract::decode(&bytes).unwrap(), PackageKind::Driver, *b"DRVR")
                    .unwrap_err();
        assert!(matches!(error, Error::Owner(PoolError::RegistrationUnavailable { .. })),
                "{error}");
        assert!(error.to_string().contains("registration"), "{error}");
        assert_eq!(registry.modules.iter().count(), before);
        assert_eq!(registry.dependencies.retained_values().count(), 0);
        registry.quiesce(provider).unwrap();
        registry.finish_quiesce(provider).unwrap();
    }

    #[test]
    fn invalidated_provider_interfaces_retain_dependency_tickets_until_real_release()
    {
        use mac_object_table::registry::CleanupFailure;

        use crate::toolbox::handles::cleanup::{Cause, context::Context};
        let mut registry = fixture::Fixture::new(Target::RVA23U64);
        let provider = stage(&mut registry, &metadata(*b"test-module-uuid", &[OFFER], &[]));
        registry.commit(provider).unwrap();
        let interface = registry.find(OFFER.class, 1).next().unwrap().0;
        let declaration = mac_module::Dependency { module:   [0; 16],
                                                   class:    OFFER.class,
                                                   version:  1,
                                                   optional: false };
        let consumer = stage(&mut registry, &metadata(*b"consumer-module!", &[], &[declaration]));
        let binding = registry.dependencies.iter().next().unwrap().0;
        assert!(binding > u64::from(u32::MAX));
        let dependent = registry.module_ledger(consumer).unwrap();
        let dependency_key = registry.dependencies.key(binding).unwrap();
        let dependency_ticket = dependent.invalidate(dependency_key).unwrap();
        assert!(registry.dependencies.get(binding).is_none());
        let owner = registry.module_ledger(provider).unwrap();
        let key = registry.interfaces.key(interface).unwrap();
        let ticket = owner.invalidate(key).unwrap();
        let dispatch = registry._kinds.dispatch(key.kind()).unwrap();
        let error = dispatch.invoke(&mut *registry as &mut dyn Context, ticket).unwrap_err();
        assert!(error.to_string().contains("unload the dependent module first"));
        let (ticket, cause) = error.into_parts();
        assert_eq!(cause,
                   Cause::DriverDependencyBusy { provider,
                                                 dependent: consumer });
        assert_eq!(ticket.key(), key);
        owner.quarantine(ticket).unwrap();
        assert_eq!(owner.summary().quarantined, 1);
        assert!(registry.interface(interface).is_err());
        let waiting = registry.rollback(consumer).unwrap_err();
        assert!(waiting.to_string().contains("still pending"), "{waiting}");
        let dispatch = registry._kinds.dispatch(dependency_key.kind()).unwrap();
        dispatch.invoke(&mut *registry as &mut dyn Context, dependency_ticket).unwrap();
        registry.rollback(consumer).unwrap();
        assert!(registry.dependencies.get(binding).is_none());
        registry.quiesce(provider).unwrap();
        registry.finish_quiesce(provider).unwrap();
        assert_eq!((owner.summary().live, owner.summary().pending), (0, 0));
    }

    #[test]
    fn separate_registries_cannot_alias_live_or_retired_driver_ids()
    {
        let bytes = metadata(*b"test-module-uuid", &[OFFER], &[]);
        let mut first = fixture::Fixture::new(Target::RVA23U64);
        let mut second = fixture::Fixture::new(Target::RVA23U64);
        let a = stage(&mut first, &bytes);
        let b = stage(&mut second, &bytes);
        assert_ne!(a, b);
        assert_eq!(first.state(b), Err(Error::Stale));
        assert_eq!(second.state(a), Err(Error::Stale));
        first.commit(a).unwrap();
        second.commit(b).unwrap();
        let (ia, _) = first.find(OFFER.class, 1).next().unwrap();
        let (ib, _) = second.find(OFFER.class, 1).next().unwrap();
        assert_ne!(ia, ib);
        assert_eq!(first.interface(ib), Err(Error::Stale));
        let ta = first.begin(0, ia, 1, &[], 0).unwrap();
        let tb = second.begin(0, ib, 1, &[], 0).unwrap();
        assert_ne!(ta, tb);
        assert_eq!(first.complete(0, tb, 0), Err(Error::Stale));
    }
    #[test]
    fn kernel_selection_distinguishes_missing_capabilities_from_stale_contracts()
    {
        let mut registry = fixture::Fixture::new(Target::RVA23U64);
        assert_eq!(registry.select(OFFER.class, 2, 1), Ok(None));
        let old = Offer { flags: Offer::KERNEL_ONLY,
                          capabilities: 1,
                          ..OFFER };
        let old_id = registry.register_service(old).unwrap();
        assert_eq!(registry.select(OFFER.class, 1, 1), Ok(Some(old_id)));
        assert_eq!(registry.select(OFFER.class, 2, 1),
                   Err(Error::Contract(mac_module::Error::Version)));
        assert_eq!(registry.select(OFFER.class, 2, 2), Ok(None));
        let new_id = registry.register_service(Offer { version: 2,
                                                       ..old })
                             .unwrap();
        assert_eq!(registry.select(OFFER.class, 2, 1), Ok(Some(new_id)));
        assert_eq!(registry.select(ClassId(*b"NONE"), 2, 1), Ok(None));
        let bytes = metadata(*b"select-module!!!",
                             &[Offer { version: 2,
                                       capabilities: 2,
                                       ..OFFER }],
                             &[]);
        let module = stage(&mut registry, &bytes);
        assert_eq!(registry.select(OFFER.class, 2, 2), Ok(None));
        registry.commit(module).unwrap();
        assert!(registry.select(OFFER.class, 2, 2).unwrap().is_some());
    }
    #[test]
    fn resident_services_require_explicit_contracts_and_keep_request_ownership()
    {
        let offer = Offer { flags: Offer::KERNEL_ONLY | Offer::EXCLUSIVE | Offer::SERIALIZED,
                            ..OFFER };
        let service = Service { service:  OFFER.class,
                                version:  1,
                                optional: false };
        let bytes = metadata_with_requirements(*b"consumer-module!", &[], &[], &[service], &[]);
        let mut registry = fixture::Fixture::new(Target::RVA23U64);
        assert!(matches!(registry.stage(Contract::decode(&bytes).unwrap(),
                                        PackageKind::Driver,
                                        *b"DRVR"),
                         Err(Error::Contract(_))));
        let provider = registry.register_service(offer).unwrap();
        assert_eq!(registry.register_service(offer), Err(Error::Duplicate));
        let module = stage(&mut registry, &bytes);
        assert_eq!(registry.resolve(u64::from(module), OFFER.class, 1), Ok(provider));
        let unrelated = stage(&mut registry, &metadata(*b"unrelated-id!!!!", &[], &[]));
        assert_eq!(registry.resolve(u64::from(unrelated), OFFER.class, 1), Err(Error::Access));
        assert_eq!(registry.begin(u64::from(unrelated), provider, 1, &[], 0), Err(Error::Access));
        let ticket = registry.begin(u64::from(module), provider, 1, &[], 0).unwrap();
        assert_eq!(registry.commit(module), Err(Error::Busy));
        assert_eq!(registry.complete(0, ticket, 0), Err(Error::Access));
        registry.complete(u64::from(module), ticket, 0).unwrap();
        registry.commit(module).unwrap();
        assert_eq!(registry.request(u64::from(module), ticket), Err(Error::Stale));
        let conflicting = metadata(*b"test-module-uuid", &[OFFER], &[]);
        assert_eq!(registry.stage(Contract::decode(&conflicting).unwrap(),
                                  PackageKind::Driver,
                                  *b"DRVR"),
                   Err(Error::Exclusive));
    }
    #[test]
    fn device_authority_uses_the_admitted_bus_match_and_module_incarnation()
    {
        let matcher = mac_module::DeviceMatch { bus:   ClassId(*b"VMMI"),
                                                value: [3; 16],
                                                mask:  [255; 16] };
        let bytes = metadata_with_requirements(*b"consumer-module!", &[], &[], &[], &[matcher]);
        let mut registry = fixture::Fixture::new(Target::RVA23U64);
        let module = stage(&mut registry, &bytes);
        assert_eq!(registry.device_allowed(module, matcher.bus, matcher.value), Ok(()));
        assert_eq!(registry.device_allowed(module, matcher.bus, [4; 16]), Err(Error::Access));
        assert_eq!(registry.device_allowed(module, ClassId(*b"PCIE"), matcher.value),
                   Err(Error::Access));
        registry.rollback(module).unwrap();
        let next = stage(&mut registry, &bytes);
        assert_ne!(next, module);
        assert_eq!(registry.device_allowed(module, matcher.bus, matcher.value), Err(Error::Stale));
        assert_eq!(registry.device_allowed(next, matcher.bus, matcher.value), Ok(()));
    }
}

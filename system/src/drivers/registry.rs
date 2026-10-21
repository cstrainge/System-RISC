//! Class-neutral module admission and publication. No driver callback executes
//! while this registry is borrowed. Hardware and class adapters live elsewhere.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_module::{Available, ClassId, Contract, Offer, PackageKind, Policy, Service, Target};

use crate::toolbox::handles::{Kind, Table};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Contract(mac_module::Error),
    Duplicate,
    Exclusive,
    Exhausted,
    Stale,
    State,
    Access,
    Busy
}
impl From<mac_module::Error> for Error
{
    fn from(value: mac_module::Error) -> Self
    {
        Self::Contract(value)
    }
}
pub type Result<T> = core::result::Result<T, Error>;
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
    identity:     [u8; 16],
    kind:         PackageKind,
    state:        State,
    offers:       Vec<Offer>,
    dependencies: Vec<mac_module::Dependency>,
    services:     Vec<Service>,
    devices:      Vec<mac_module::DeviceMatch>,
    interfaces:   Vec<u32>,
    requests:     usize
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interface
{
    pub module: u32,
    pub offer:  Offer
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request
{
    pub module:    u32,
    pub interface: u32,
    pub caller:    u64,
    pub class:     ClassId,
    pub operation: u32,
    pub max_reply: u32,
    active:        bool
}
pub struct Registry
{
    modules:    Table<Module>,
    interfaces: Table<Interface>,
    requests:   Table<Request>,
    target:     Target,
    services:   Vec<Service>
}
impl Registry
{
    pub fn new(target: Target) -> Self
    {
        Self { modules: Table::new(Kind::Modules),
               interfaces: Table::new(Kind::DriverInterfaces),
               requests: Table::new(Kind::DriverRequests),
               target,
               services: Vec::new() }
    }

    /// Resident services use the same capability/request machinery as modules.
    /// Provider owner zero denotes the resident kernel, not an issued module ID.
    pub fn register_service(&mut self, offer: Offer) -> Result<u32>
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
        self.services.try_reserve(1).map_err(|_| Error::Exhausted)?;
        let id = self.interfaces
                     .insert(Interface { module: 0,
                                         offer })
                     .map_err(|_| Error::Exhausted)?;
        self.services.push(Service { service:  offer.class,
                                     version:  offer.version,
                                     optional: false });
        Ok(id)
    }

    pub fn device_allowed(&self, module: u32, bus: ClassId, key: [u8; 16]) -> Result<()>
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
        let providers =
            self.interfaces
                .iter()
                .filter_map(|(_, interface)| {
                    let module = self.modules.get(interface.module)?;
                    (module.state == State::Ready).then_some(Available { module:  module.identity,
                                           class:   interface.offer.class,
                                           version: interface.offer.version })
                })
                .collect::<Vec<_>>();
        Policy { directory,
                 file_type,
                 kernel_abi: mac_abi::ABI_VERSION,
                 target: self.target,
                 max_heap: 1024 * 1024,
                 max_stack: 64 * 1024,
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
                 -> Result<u32>
    {
        self.admit(contract, directory, file_type)?;
        let offers = contract.offers().collect::<mac_module::Result<Vec<_>>>()?;
        for offer in &offers
        {
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
        let dependencies = contract.dependencies().collect::<mac_module::Result<Vec<_>>>()?;
        let services = contract.services().collect::<mac_module::Result<Vec<_>>>()?;
        let devices = contract.devices().collect::<mac_module::Result<Vec<_>>>()?;
        self.modules
            .insert(Module { identity: contract.identity,
                             kind: contract.kind,
                             state: State::Initializing,
                             offers,
                             dependencies,
                             services,
                             devices,
                             interfaces: Vec::new(),
                             requests: 0 })
            .map_err(|_| Error::Exhausted)
    }

    /// Transactional publication: reserve every interface first and roll back
    /// temporary registrations on exhaustion. Initializing modules stay hidden.
    pub fn commit(&mut self, module: u32) -> Result<()>
    {
        let record = self.modules.get(module).ok_or(Error::Stale)?;
        if record.state != State::Initializing
        {
            return Err(Error::State);
        }
        if self.requests.iter().any(|(_, r)| r.caller == u64::from(module))
        {
            return Err(Error::Busy);
        }
        let mut published = Vec::new();
        published.try_reserve(record.offers.len()).map_err(|_| Error::Exhausted)?;
        for &offer in &record.offers
        {
            match self.interfaces.insert(Interface { module,
                                                     offer })
            {
                Ok(id) => published.push(id),
                Err(_) =>
                {
                    for id in published
                    {
                        self.interfaces.remove(id);
                    }
                    return Err(Error::Exhausted);
                }
            }
        }
        let record = self.modules.get_mut(module).ok_or(Error::Stale)?;
        record.interfaces = published;
        record.state = State::Ready;
        Ok(())
    }

    pub fn rollback(&mut self, module: u32) -> Result<()>
    {
        let record = self.modules.get(module).ok_or(Error::Stale)?;
        if record.state != State::Initializing
        {
            return Err(Error::State);
        }
        if self.requests.iter().any(|(_, r)| r.caller == u64::from(module))
        {
            self.modules.get_mut(module).ok_or(Error::Stale)?.state = State::Quiescing;
            return Err(Error::Busy);
        }
        self.modules.remove(module);
        Ok(())
    }

    pub fn state(&self, module: u32) -> Result<State>
    {
        Ok(self.modules.get(module).ok_or(Error::Stale)?.state)
    }

    pub fn kind(&self, module: u32) -> Result<PackageKind>
    {
        Ok(self.modules.get(module).ok_or(Error::Stale)?.kind)
    }

    pub fn find(&self, class: ClassId, version: u32)
                -> impl Iterator<Item = (u32, Interface)> + '_
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
    pub fn select(&self, class: ClassId, version: u32, capabilities: u64) -> Result<Option<u32>>
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

    pub fn resolve(&self, caller: u64, class: ClassId, version: u32) -> Result<u32>
    {
        if caller == 0
        {
            return self.find(class, version).next().map(|(id, _)| id).ok_or(Error::Stale);
        }
        let module = self.modules
                         .get(u32::try_from(caller).map_err(|_| Error::Access)?)
                         .ok_or(Error::Access)?;
        if !matches!(module.state, State::Initializing | State::Ready)
        {
            return Err(Error::State);
        }
        if module.services.iter().any(|s| s.service == class && s.version == version)
           && let Some((id, _)) = self.find(class, version).find(|(_, i)| i.module == 0)
        {
            return Ok(id);
        }
        for dependency in &module.dependencies
        {
            if dependency.class != class || dependency.version != version
            {
                continue;
            }
            for (id, interface) in self.find(class, version)
            {
                if interface.module == 0
                {
                    continue;
                }
                let provider = self.modules.get(interface.module).ok_or(Error::Stale)?;
                if dependency.module == [0; 16] || dependency.module == provider.identity
                {
                    return Ok(id);
                }
            }
            return Err(Error::Stale);
        }
        Err(Error::Access)
    }

    pub fn interface(&self, id: u32) -> Result<Interface>
    {
        let interface = *self.interfaces.get(id).ok_or(Error::Stale)?;
        if interface.module != 0
           && self.modules.get(interface.module).is_none_or(|m| m.state != State::Ready)
        {
            return Err(Error::State);
        }
        Ok(interface)
    }

    /// Caller 0 is resident System. Module callers are limited to dependencies
    /// declared in their own admitted contract; applications use class services.
    pub fn begin(&mut self, caller: u64, interface: u32, operation: u32, input_len: usize,
                 output_len: usize)
                 -> Result<u32>
    {
        let provider = self.interface(interface)?;
        if input_len > provider.offer.max_request as usize
           || output_len > provider.offer.max_reply as usize
        {
            return Err(Error::Contract(mac_module::Error::Limit));
        }
        if caller != 0
        {
            let owner = self.modules
                            .get(u32::try_from(caller).map_err(|_| Error::Access)?)
                            .ok_or(Error::Access)?;
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
                let identity = self.modules.get(provider.module).ok_or(Error::Stale)?.identity;
                owner.dependencies.iter().any(|d| {
                                             d.class == provider.offer.class
                                             && d.version == provider.offer.version
                                             && (d.module == [0; 16] || d.module == identity)
                                         })
            };
            if !allowed
            {
                return Err(Error::Access);
            }
        }

        if provider.offer.flags & Offer::SERIALIZED != 0
           && self.requests.iter().any(|(_, r)| r.interface == interface)
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
        let ticket = self.requests
                         .insert(Request { module: provider.module,
                                           interface,
                                           caller,
                                           class: provider.offer.class,
                                           operation,
                                           max_reply: output_len as u32,
                                           active: false })
                         .map_err(|_| Error::Exhausted)?;
        if provider.module != 0
        {
            self.modules.get_mut(provider.module).ok_or(Error::Stale)?.requests = count;
        }
        Ok(ticket)
    }

    pub fn request(&self, caller: u64, ticket: u32) -> Result<Request>
    {
        let request = *self.requests.get(ticket).ok_or(Error::Stale)?;
        if request.caller != caller
        {
            return Err(Error::Access);
        }
        if caller!=0 && self.modules.get(u32::try_from(caller).map_err(|_|Error::Access)?)
            .is_none_or(|m|!matches!(m.state,State::Initializing|State::Ready|State::Quiescing))
        {return Err(Error::Access);}

        if request.module != 0
           && self.modules
                  .get(request.module)
                  .is_none_or(|m| !matches!(m.state, State::Ready | State::Quiescing))
        {
            return Err(Error::State);
        }
        Ok(request)
    }

    pub fn interface_for_request(&self, caller: u64, ticket: u32) -> Result<Interface>
    {
        let request = self.request(caller, ticket)?;
        self.interfaces.get(request.interface).copied().ok_or(Error::Stale)
    }

    pub fn enter_request(&mut self, caller: u64, ticket: u32) -> Result<()>
    {
        let request = self.request(caller, ticket)?;
        if request.active
        {
            return Err(Error::Busy);
        }
        self.requests.get_mut(ticket).ok_or(Error::Stale)?.active = true;
        Ok(())
    }

    pub fn leave_request(&mut self, caller: u64, ticket: u32) -> Result<()>
    {
        let request = self.request(caller, ticket)?;
        if !request.active
        {
            return Err(Error::State);
        }
        self.requests.get_mut(ticket).ok_or(Error::Stale)?.active = false;
        Ok(())
    }

    /// Release on terminal success, failure or acknowledged cancellation only.
    /// PENDING retains the ticket; late or foreign completions cannot reuse it.
    pub fn complete(&mut self, caller: u64, ticket: u32, output_len: usize) -> Result<Request>
    {
        let request = self.request(caller, ticket)?;
        if request.active
        {
            return Err(Error::Busy);
        }
        if output_len > request.max_reply as usize
        {
            return Err(Error::Contract(mac_module::Error::Limit));
        }
        if request.module != 0
        {
            let record = self.modules.get_mut(request.module).ok_or(Error::Stale)?;
            record.requests = record.requests.checked_sub(1).ok_or(Error::State)?;
        }
        self.requests.remove(ticket);
        Ok(request)
    }

    /// Withdraw before asking the module to quiesce; no new requests enter.
    pub fn quiesce(&mut self, module: u32) -> Result<()>
    {
        let record = self.modules.get_mut(module).ok_or(Error::Stale)?;
        if record.state != State::Ready
        {
            return Err(Error::State);
        }
        record.state = State::Quiescing;
        Ok(())
    }

    /// Pin code until reboot even after failure. Outstanding requests require
    /// terminal cancellation before failure is finalized.
    pub fn finish_quiesce(&mut self, module: u32) -> Result<()>
    {
        let record = self.modules.get_mut(module).ok_or(Error::Stale)?;
        if record.state != State::Quiescing
        {
            return Err(Error::State);
        }
        if record.requests != 0 || self.requests.iter().any(|(_, r)| r.caller == u64::from(module))
        {
            return Err(Error::Busy);
        }
        for id in record.interfaces.drain(..)
        {
            self.interfaces.remove(id);
        }
        record.state = State::Failed;
        Ok(())
    }
}

#[cfg(test)]
mod tests
{
    use mac_module::{BROKER_ABI, Spec};

    use super::*;
    const OFFER: Offer = Offer { class:          ClassId(*b"TEST"),
                                 version:        1,
                                 request_schema: 1,
                                 reply_schema:   1,
                                 max_request:    16,
                                 max_reply:      16,
                                 flags:          0,
                                 capabilities:   0 };
    fn metadata(identity: [u8; 16], offers: &[Offer], dependencies: &[mac_module::Dependency])
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
                          target: Target::RV64IMAC,
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
    fn stage(registry: &mut Registry, bytes: &[u8]) -> u32
    {
        registry.stage(Contract::decode(bytes).unwrap(), PackageKind::Driver, *b"DRVR").unwrap()
    }
    #[test]
    fn initialization_is_hidden_and_rollback_never_reuses_ids()
    {
        let bytes = metadata(*b"test-module-uuid", &[OFFER], &[]);
        let mut registry = Registry::new(Target::RV64IMAC);
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
        assert_eq!(registry.interface(live), Err(Error::Stale));
        assert_eq!(registry.state(interface), Err(Error::Stale));
    }
    #[test]
    fn request_reuse_foreign_completions_and_quiesce_are_checked()
    {
        let bytes = metadata(*b"test-module-uuid", &[OFFER], &[]);
        let mut registry = Registry::new(Target::RV64IMAC);
        let module = stage(&mut registry, &bytes);
        registry.commit(module).unwrap();
        let (interface, _) = registry.find(OFFER.class, 1).next().unwrap();
        let stale = registry.begin(0, interface, 7, 3, 10).unwrap();
        assert_eq!(registry.complete(123, stale, 0), Err(Error::Access));
        assert_eq!(registry.complete(0, stale, 11), Err(Error::Contract(mac_module::Error::Limit)));
        registry.enter_request(0, stale).unwrap();
        assert_eq!(registry.enter_request(0, stale), Err(Error::Busy));
        assert_eq!(registry.complete(0, stale, 0), Err(Error::Busy));
        registry.leave_request(0, stale).unwrap();
        registry.complete(0, stale, 10).unwrap();
        let live = registry.begin(0, interface, 7, 3, 10).unwrap();
        assert_ne!(live, stale);
        assert_eq!(registry.complete(0, stale, 0), Err(Error::Stale));
        registry.quiesce(module).unwrap();
        assert_eq!(registry.interface_for_request(0, live).unwrap().module, module);
        assert_eq!(registry.find(OFFER.class, 1).count(), 0);
        assert_eq!(registry.finish_quiesce(module), Err(Error::Busy));
        assert_eq!(registry.begin(0, interface, 7, 0, 0), Err(Error::State));
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
        let mut registry = Registry::new(Target::RV64IMAC);
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
        assert_eq!(registry.begin(u64::from(unrelated), interface, 1, 0, 0), Err(Error::Access));
        let ticket = registry.begin(u64::from(consumer), interface, 1, 0, 0).unwrap();
        assert_eq!(registry.begin(0, interface, 1, 0, 0), Err(Error::Busy));
        registry.complete(u64::from(consumer), ticket, 0).unwrap();
        assert_eq!(BROKER_ABI, 1);
    }
    #[test]
    fn failed_initialization_retains_pending_dependency_ownership_until_cancellation()
    {
        let bytes = metadata(*b"test-module-uuid", &[OFFER], &[]);
        let mut registry = Registry::new(Target::RV64IMAC);
        let provider = stage(&mut registry, &bytes);
        registry.commit(provider).unwrap();
        let (interface, _) = registry.find(OFFER.class, 1).next().unwrap();
        let dependency = mac_module::Dependency { module:   [0; 16],
                                                  class:    OFFER.class,
                                                  version:  1,
                                                  optional: false };
        let bytes = metadata(*b"consumer-module!", &[], &[dependency]);
        let consumer = stage(&mut registry, &bytes);
        let ticket = registry.begin(u64::from(consumer), interface, 1, 0, 0).unwrap();
        assert_eq!(registry.commit(consumer), Err(Error::Busy));
        assert_eq!(registry.rollback(consumer), Err(Error::Busy));
        assert_eq!(registry.state(consumer), Ok(State::Quiescing));
        assert_eq!(registry.finish_quiesce(consumer), Err(Error::Busy));
        assert_eq!(registry.begin(u64::from(consumer), interface, 1, 0, 0), Err(Error::State));
        registry.complete(u64::from(consumer), ticket, 0).unwrap();
        registry.finish_quiesce(consumer).unwrap();
        assert_eq!(registry.state(consumer), Ok(State::Failed));
        assert_eq!(registry.request(u64::from(consumer), ticket), Err(Error::Stale));
    }

    #[test]
    fn separate_registries_cannot_alias_live_or_retired_driver_ids()
    {
        let bytes = metadata(*b"test-module-uuid", &[OFFER], &[]);
        let mut first = Registry::new(Target::RV64IMAC);
        let mut second = Registry::new(Target::RV64IMAC);
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
        let ta = first.begin(0, ia, 1, 0, 0).unwrap();
        let tb = second.begin(0, ib, 1, 0, 0).unwrap();
        assert_ne!(ta, tb);
        assert_eq!(first.complete(0, tb, 0), Err(Error::Stale));
    }
    #[test]
    fn kernel_selection_distinguishes_missing_capabilities_from_stale_contracts()
    {
        let mut registry = Registry::new(Target::RV64IMAC);
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
        let mut registry = Registry::new(Target::RV64IMAC);
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
        assert_eq!(registry.begin(u64::from(unrelated), provider, 1, 0, 0), Err(Error::Access));
        let ticket = registry.begin(u64::from(module), provider, 1, 0, 0).unwrap();
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
        let mut registry = Registry::new(Target::RV64IMAC);
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

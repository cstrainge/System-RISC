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
    pub fn new(target: Target, services: &[Service]) -> Self
    {
        Self { modules: Table::new(Kind::Modules),
               interfaces: Table::new(Kind::DriverInterfaces),
               requests: Table::new(Kind::DriverRequests),
               target,
               services: services.to_vec() }
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
            if self.modules.iter().any(|(_, m)| {
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
        self.modules
            .insert(Module { identity: contract.identity,
                             kind: contract.kind,
                             state: State::Initializing,
                             offers,
                             dependencies,
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
                && self.modules.get(i.module).is_some_and(|m| m.state == State::Ready)
            })
            .map(|(id, i)| (id, *i))
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
        for dependency in &module.dependencies
        {
            if dependency.class != class || dependency.version != version
            {
                continue;
            }
            for (id, interface) in self.find(class, version)
            {
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
        if self.modules.get(interface.module).is_none_or(|m| m.state != State::Ready)
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
            let identity = self.modules.get(provider.module).ok_or(Error::Stale)?.identity;
            if !owner.dependencies.iter().any(|d| {
                                             d.class == provider.offer.class
                                             && d.version == provider.offer.version
                                             && (d.module == [0; 16] || d.module == identity)
                                         })
            {
                return Err(Error::Access);
            }
        }
        let record = self.modules.get(provider.module).ok_or(Error::Stale)?;
        if provider.offer.flags & Offer::SERIALIZED != 0
           && self.requests.iter().any(|(_, r)| r.interface == interface)
        {
            return Err(Error::Busy);
        }
        let count = record.requests.checked_add(1).ok_or(Error::Exhausted)?;
        let ticket = self.requests
                         .insert(Request { module: provider.module,
                                           interface,
                                           caller,
                                           class: provider.offer.class,
                                           operation,
                                           max_reply: output_len as u32,
                                           active: false })
                         .map_err(|_| Error::Exhausted)?;
        self.modules.get_mut(provider.module).ok_or(Error::Stale)?.requests = count;
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

        if self.modules
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
        let record = self.modules.get_mut(request.module).ok_or(Error::Stale)?;
        record.requests = record.requests.checked_sub(1).ok_or(Error::State)?;
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
                          services: &[],
                          devices: &[] };
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
        let mut registry = Registry::new(Target::RV64IMAC, &[]);
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
        let mut registry = Registry::new(Target::RV64IMAC, &[]);
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
        let mut registry = Registry::new(Target::RV64IMAC, &[]);
        let provider = stage(&mut registry, &bytes);
        assert_eq!(registry.stage(Contract::decode(&other).unwrap(),
                                  PackageKind::Driver,
                                  *b"DRVR"),
                   Err(Error::Exclusive));
        registry.commit(provider).unwrap();
        let (interface, _) = registry.find(OFFER.class, 1).next().unwrap();
        let dep = mac_module::Dependency { module:   *b"test-module-uuid",
                                           class:    OFFER.class,
                                           version:  1,
                                           optional: false };
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
        let mut registry = Registry::new(Target::RV64IMAC, &[]);
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
        let mut first = Registry::new(Target::RV64IMAC, &[]);
        let mut second = Registry::new(Target::RV64IMAC, &[]);
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
}

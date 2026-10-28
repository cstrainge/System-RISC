//! Boundary-shape tests: these do not claim live allocator/dispatch coverage.
use core::{alloc::Layout, cell::Cell};

use mac_kernel_model::{DomainId, DomainKind,
                       policy::{BudgetExceeded, BudgetReason, MemoryClass, ModelError},
                       process::ExecutionBinding,
                       zone::{AllocationContext, ForwardingAllocator, ForwardingLease,
                              ZoneBinding}};
use mac_object_table::domain::{CapacityPolicy, KindCode, OwnerIndex, ResourceKey};

fn resource(owner: &mut OwnerIndex, kind: KindCode) -> ResourceKey
{
    owner.add_pool(kind, CapacityPolicy::GrowableUntilMemory).unwrap();
    owner.grow(kind, 1).unwrap();
    owner.allocate(kind).unwrap()
}

#[test]
fn execution_binding_checks_executor_subject_and_allocation_together()
{
    let module = OwnerIndex::issue(DomainKind::Module).unwrap();
    let process = OwnerIndex::issue(DomainKind::Process).unwrap();
    let other = OwnerIndex::issue(DomainKind::Process).unwrap();
    let own_heap = AllocationContext::Application(process.owner());
    let binding = ExecutionBinding::new(&module, &process, own_heap).unwrap();
    assert_eq!(binding.executor(), module.owner());
    assert_eq!(binding.subject(), process.owner());
    assert_eq!(binding.allocation(), own_heap);
    ExecutionBinding::new(&process, &process, own_heap).unwrap();
    ExecutionBinding::new(&module, &module, AllocationContext::Kernel(module.owner())).unwrap();
    for result in
        [ExecutionBinding::new(&process, &other, AllocationContext::Kernel(other.owner())),
         ExecutionBinding::new(&module, &process, AllocationContext::Application(other.owner())),
         ExecutionBinding::new(&module, &process, AllocationContext::Kernel(module.owner())),
         ExecutionBinding::new(&module, &process, AllocationContext::System),
         ExecutionBinding::new(&module, &module, AllocationContext::Application(module.owner()))]
    {
        assert_eq!(result.unwrap_err(), ModelError::WrongDomain);
    }
    assert!(ModelError::WrongDomain.to_string().contains("different lifetime domain"));
}

#[test]
fn control_callback_refuses_foreign_and_disposed_or_reused_resources()
{
    let module = OwnerIndex::issue(DomainKind::Module).unwrap();
    let mut process = OwnerIndex::issue(DomainKind::Process).unwrap();
    let foreign = OwnerIndex::issue(DomainKind::Process).unwrap();
    let instance = resource(&mut process, KindCode(*b"CTRL"));
    let zone = resource(&mut process, KindCode(*b"ZONE"));
    let context = AllocationContext::Control(ZoneBinding::new(instance, zone).unwrap());
    ExecutionBinding::new(&module, &process, context).unwrap();
    assert_eq!(ExecutionBinding::new(&module, &foreign, context).unwrap_err(),
               ModelError::WrongDomain);
    let released = process.invalidate(zone).unwrap();
    process.complete_release(released).unwrap();
    assert_eq!(ExecutionBinding::new(&module, &process, context).unwrap_err(),
               ModelError::InvalidBinding);
    let replacement = process.allocate(KindCode(*b"ZONE")).unwrap();
    assert_eq!(replacement.slot(), zone.slot());
    assert_ne!(replacement.generation(), zone.generation());
    assert_eq!(ExecutionBinding::new(&module, &process, context).unwrap_err(),
               ModelError::InvalidBinding);
    let context = AllocationContext::Control(ZoneBinding::new(instance, replacement).unwrap());
    ExecutionBinding::new(&module, &process, context).unwrap();
    let released = process.invalidate(instance).unwrap();
    process.complete_release(released).unwrap();
    assert_eq!(ExecutionBinding::new(&module, &process, context).unwrap_err(),
               ModelError::InvalidBinding);
    assert!(ModelError::InvalidBinding.to_string().contains("lifetime binding"));
}

#[test]
fn execution_binding_accepts_shared_authority_and_checks_its_live_generation()
{
    struct HostGate;
    impl mac_object_table::shared::InvocationGate for HostGate
    {
        fn run<R>(operation: impl FnOnce() -> R) -> R
        {
            operation()
        }
    }
    let mut owners = mac_object_table::domain::Directory::<_, HostGate>::new();
    let module =
        owners.insert(mac_object_table::shared::OwnerRef::issue(DomainKind::Module).unwrap(), ())
              .unwrap();
    let process =
        owners.insert(mac_object_table::shared::OwnerRef::issue(DomainKind::Process).unwrap(), ())
              .unwrap();
    let module = owners.get(module.incarnation()).unwrap().ledger().clone();
    let process = owners.get(process.incarnation()).unwrap().ledger().clone();
    let mut keys = Vec::new();
    for kind in [KindCode(*b"CTRL"), KindCode(*b"ZONE")]
    {
        process.add_pool(kind, CapacityPolicy::GrowableUntilMemory).unwrap();
        process.grow(kind, 1).unwrap();
        keys.push(process.allocate(kind).unwrap());
    }
    let context = AllocationContext::Control(ZoneBinding::new(keys[0], keys[1]).unwrap());
    ExecutionBinding::new(&module, &process, context).unwrap();
    let released = process.invalidate(keys[1]).unwrap();
    process.complete_release(released).unwrap();
    assert_eq!(ExecutionBinding::new(&module, &process, context).unwrap_err(),
               ModelError::InvalidBinding);
    process.stop_admission();
    let error = ExecutionBinding::new(&module, &process, context).unwrap_err();
    assert_eq!(error, ModelError::DomainStopping(process.owner()));
    assert!(error.to_string().contains(&process.owner().to_string()));
}

struct Host
{
    used:     Cell<u64>,
    releases: Cell<usize>,
    context:  AllocationContext,
    layout:   Layout
}
impl ForwardingAllocator for Host
{
    // No destructor: the concrete lease must enforce the release, rather than
    // delegating that obligation to an implementation-selected storage type.
    type Storage = u64;

    fn reserve(&self, context: AllocationContext, layout: Layout) -> Result<u64, BudgetExceeded>
    {
        assert_eq!(context, self.context);
        assert_eq!(layout, self.layout);
        if self.used.get() != 0
        {
            return Err(BudgetExceeded { owner:     DomainId::SYSTEM,
                                        class:     MemoryClass::System,
                                        used:      self.used.get(),
                                        limit:     layout.size() as u64,
                                        requested: layout.size() as u64,
                                        reason:    BudgetReason::DomainLimit });
        }
        self.used.set(layout.size() as u64);
        Ok(layout.size() as u64)
    }

    fn release(&self, context: AllocationContext, layout: Layout, storage: u64)
    {
        assert_eq!(context, self.context);
        assert_eq!(layout, self.layout);
        assert_eq!(storage, self.used.get());
        self.used.set(0);
        self.releases.set(self.releases.get() + 1);
    }
}

#[test]
fn forwarding_lease_refunds_exactly_once_on_move_drop_and_unwind()
{
    let host = Host { used:     Cell::new(0),
                      releases: Cell::new(0),
                      context:  AllocationContext::System,
                      layout:   Layout::from_size_align(64, 32).unwrap() };
    let lease = ForwardingLease::allocate(&host, host.context, host.layout).unwrap();
    assert_eq!(lease.context(), host.context);
    assert_eq!(lease.layout(), host.layout);
    assert_eq!(host.used.get(), 64);
    let error = ForwardingLease::allocate(&host, host.context, host.layout).err().unwrap();
    assert_eq!(error.reason, BudgetReason::DomainLimit);
    assert!(error.to_string().contains("64 bytes requested"));
    assert_eq!(host.releases.get(), 0);
    let moved = Some(lease);
    drop(moved);
    assert_eq!(host.used.get(), 0);
    assert_eq!(host.releases.get(), 1);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                              let _lease =
                                                  ForwardingLease::allocate(&host, host.context,
                                                                            host.layout).unwrap();
                                              panic!("exercise unwind cleanup");
                                          }));
    assert!(result.is_err());
    assert_eq!(host.used.get(), 0);
    assert_eq!(host.releases.get(), 2);
}

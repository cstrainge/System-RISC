//! Admitted dependencies bind exact provider incarnations in the module ledger.
//! A declaration cannot silently switch to a different provider after startup.
#![forbid(unsafe_code)]
use mac_object_table::{domain::{ReleasedResource, ResourceKey},
                       registry::{Release, Source, Specification}};

use super::{Error, Registry, Result};
use crate::toolbox::handles::{CapacityPolicy, KindCode,
                              cleanup::{Failure, context::Context}};

pub(super) struct Binding
{
    pub(super) dependent:     u64,
    pub(super) provider:      u64,
    pub(super) interface:     u64,
    pub(super) interface_key: ResourceKey,
    pub(super) class:         mac_module::ClassId,
    pub(super) version:       u32,
    pub(super) scope:
        Option<mac_object_table::scope::Lease<crate::toolbox::handles::InvocationGate>>
}

pub(crate) fn specification(storage: bool) -> Specification<'static>
{
    Specification { kind:     KindCode(if storage { *b"DDST" } else { *b"DDEP" }),
                    label:    if storage
                    {
                        "Module dependency storage"
                    }
                    else
                    {
                        "Module dependency claims"
                    },
                    provider: "System Module Manager",
                    capacity: if storage
                    {
                        CapacityPolicy::HardBound { slots:  1,
                                                    reason: "One dependency backing per module \
                                                             domain" }
                    }
                    else
                    {
                        CapacityPolicy::GrowableUntilMemory
                    },
                    source:   Source::Resident }
}

pub(crate) struct ReleaseDependencies(pub bool);
impl<C: Context + ?Sized> Release<C, Failure> for ReleaseDependencies
{
    fn release(&self, context: &mut C, ticket: ReleasedResource)
               -> core::result::Result<(), Failure>
    {
        let Some(registry) = context.driver_registry()
        else
        {
            return Err(Failure::context_unavailable(ticket, "driver registry"));
        };
        if self.0
        {
            registry.dependencies.release_storage(ticket).map_err(Failure::from)
        }
        else
        {
            if let Some(binding) = registry.dependencies.releasing(&ticket)
            {
                if binding.scope.as_ref().is_some_and(|scope| !scope.closed())
                {
                    return Err(Failure::driver_dependency_scope_active(ticket, binding.provider,
                                                                       binding.dependent));
                }
            }
            drop(registry.dependencies.release(ticket).map_err(Failure::from)?);
            Ok(())
        }
    }
}

impl Registry
{
    /// A private backing lease pins the exact lower provider incarnation. This
    /// grants lifetime only; declared service/resolve policy stays unchanged.
    pub(crate) fn pin_backing_provider(&mut self, dependent: u64, interface: u64,
                                       scope: &mac_object_table::scope::Lease<crate::toolbox::handles::InvocationGate>)
                                       -> Result<()>
    {
        let owner = self.module_ledger(dependent)?;
        if scope.subject() != owner.owner()
        {
            return Err(Error::Access);
        }
        let provider = self.interface(interface)?;
        if provider.module == 0
        {
            return Ok(());
        }
        let key = self.interfaces.key(interface).ok_or(Error::Stale)?;
        if self.dependencies.retained_values().any(|binding| {
                                                  binding.interface_key == key
            && binding.scope.as_ref().is_some_and(|old| old.same_scope(scope))
                                              })
        {
            return Ok(());
        }
        let binding = Binding { dependent,
                                provider: provider.module,
                                interface,
                                interface_key: key,
                                class: provider.offer.class,
                                version: provider.offer.version,
                                scope: Some(scope.clone()) };
        let allocation = self.module_allocation(dependent)?;
        crate::allocation::with_kernel_allocation(allocation, || {
            self.dependencies.insert_in_scope(scope, binding).map_err(Error::Owner)
        }).map_err(Error::AllocationScope)??;
        Ok(())
    }

    /// Only the module-scope-close notification drives this per-change walk.
    /// Source roots stay open through every real read/close and result guard.
    pub(crate) fn reap_closed_dependencies(&mut self)
    {
        self.dependencies
            .remove_where(|_, binding| binding.scope.as_ref().is_some_and(|scope| scope.closed()));
    }

    pub(super) fn bind_dependencies(&mut self, dependent: u64) -> Result<()>
    {
        let owner = self.module_ledger(dependent)?;
        let count = self.modules.get(dependent).ok_or(Error::Stale)?.dependencies.len();
        for index in 0..count
        {
            let declaration = self.modules.get(dependent).ok_or(Error::Stale)?.dependencies[index];
            let provider = self.find(declaration.class, declaration.version)
                               .find(|(_, interface)| {
                                   interface.module != 0
                                   && self.modules.get(interface.module).is_some_and(|module| {
                                                                            declaration.module
                                                                            == [0; 16]
                                                                            || module.identity
                                                                               == declaration.module
                                                                        })
                               });
            let Some((interface, provider)) = provider
            else
            {
                if declaration.optional
                {
                    continue;
                }
                return Err(Error::Contract(mac_module::Error::MissingDependency));
            };
            let binding =
                Binding { dependent,
                          provider: provider.module,
                          interface,
                          interface_key: self.interfaces.key(interface).ok_or(Error::Stale)?,
                          class: declaration.class,
                          version: declaration.version,
                          scope: None };
            let id = self.dependencies.insert(&owner, binding).map_err(Error::Owner)?;
            // Admission reserved this index for every declaration before the
            // domain was published. It contains IDs, never a second payload.
            self.modules.get_mut(dependent).ok_or(Error::Stale)?.dependency_ids.push(id);
        }
        Ok(())
    }

    pub(super) fn drain_module(&mut self, module: u64) -> Result<()>
    {
        let owner = self.module_ledger(module)?;
        #[cfg(any(test, feature = "test-support"))]
        if let Some(catalog) = self.fixture_catalog.clone()
        {
            return self.drain_module_with(module, &owner, move |_, key| {
                           catalog.dispatch(key.kind())
                       });
        }
        #[cfg(target_arch = "riscv64")]
        {
            self.drain_module_with(module, &owner, |_, key| {
                crate::toolbox::handles::registry(|registry| registry.dispatch(key.kind()))
                    .and_then(|result| result.map_err(crate::toolbox::handles::KindAccessError::Registry))
            })
        }
        #[cfg(not(target_arch = "riscv64"))]
        {
            // The host transport's Session owns its actual typed catalog and
            // drives that same walk outside this narrower Registry borrow.
            // Never fabricate a second catalog or acknowledge its resources.
            if let Some(checkpoint) = &self.modules.get(module).ok_or(Error::Stale)?.cleanup
            {
                checkpoint.ensure_settled().map_err(Error::Cleanup)?;
            }
            Self::module_drained(&owner)
        }
    }

    fn drain_module_with<E: core::fmt::Display>(&mut self, module: u64,
                                                owner: &crate::toolbox::handles::OwnerRef,
                                                mut resolve: impl FnMut(&(dyn Context + 'static), ResourceKey)
            -> core::result::Result<mac_object_table::registry::Invocation<dyn Context,
                crate::toolbox::handles::InvocationGate, Failure>, E>)
                                                -> Result<()>
    {
        let Some(checkpoint) = self.modules.get(module).ok_or(Error::Stale)?.cleanup.clone()
        else
        {
            return Self::module_drained(owner);
        };
        let mut failed = |key: ResourceKey, failure: &mac_object_table::walk::Failure<E,
            crate::toolbox::handles::cleanup::Cause>| {
            #[cfg(target_arch = "riscv64")]
            serial!("MODULE_CLEANUP_QUARANTINED owner={} kind={} slot={} generation={} {failure}",
                key.owner(), key.kind(), key.slot(), key.generation());
            #[cfg(not(target_arch = "riscv64"))]
            let _ = (key, failure);
        };
        checkpoint.run(owner, || {
                      mac_object_table::walk::drain(self as &mut dyn Context, owner, &mut resolve,
                                                    &mut failed)
                  })
                  .map_err(Error::Cleanup)?;
        checkpoint.run(owner, || {
                      mac_object_table::walk::retry(self as &mut dyn Context, owner, &mut resolve,
                                                    &mut failed)
                  })
                  .map_err(Error::Cleanup)?;
        checkpoint.ensure_settled().map_err(Error::Cleanup)?;
        #[cfg(target_arch = "riscv64")]
        crate::kernel::finish_module_images();
        Self::module_drained(owner)
    }

    /// The account barrier follows real cleanup and disposal of every retained
    /// charged metadata buffer. A drained retry cannot reopen or rerun cleanup.
    pub(super) fn drain_module_metadata(&mut self, module: u64) -> Result<()>
    {
        let owner = self.module_ledger(module)?;
        Self::module_drained(&owner)?;
        if owner.accepting()
        {
            return Err(Error::State);
        }
        {
            let record = self.modules.get_mut(module).ok_or(Error::Stale)?;
            if let Some(checkpoint) = &record.cleanup
            {
                checkpoint.ensure_settled().map_err(Error::Cleanup)?;
            }
            drop(record.cleanup.take());
            drop(core::mem::take(&mut record.offers));
            drop(core::mem::take(&mut record.dependencies));
            drop(core::mem::take(&mut record.dependency_ids));
            drop(core::mem::take(&mut record.services));
            drop(core::mem::take(&mut record.devices));
            drop(core::mem::take(&mut record.file_handlers));
        }
        owner.drain_stopped().map_err(Error::Domain)?;
        let account = &self.modules.get(module).ok_or(Error::Stale)?.account;
        if !account.close_and_ready()
        {
            let remaining = account.snapshot();
            return Err(Error::AccountPending { owner:                 account.owner(),
                                               kernel_committed:      remaining.kernel.committed(),
                                               kernel_reserved:       remaining.kernel.reserved(),
                                               application_committed: remaining.application
                                                                               .committed(),
                                               application_reserved:  remaining.application
                                                                               .reserved(),
                                               retained_pins:         remaining.retained_pins });
        }
        Ok(())
    }

    fn module_drained(owner: &crate::toolbox::handles::OwnerRef) -> Result<()>
    {
        let summary = owner.summary();
        if summary.live != 0
        {
            return Err(Error::Domain(mac_object_table::domain::DirectoryError::LiveResources {
                owner: owner.owner(), live: summary.live }));
        }
        if summary.pending != 0
        {
            return Err(Error::Domain(mac_object_table::domain::DirectoryError::PendingResources {
                owner: owner.owner(), pending: summary.pending }));
        }
        if summary.completion_faults != 0
        {
            return Err(Error::Domain(mac_object_table::domain::DirectoryError::CompletionFaults {
                owner: owner.owner(), faults: summary.completion_faults }));
        }
        Ok(())
    }
}

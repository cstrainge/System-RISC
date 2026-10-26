//! Published service registrations and backing are owned by their real domain.
#![forbid(unsafe_code)]
use mac_object_table::{domain::ReleasedResource,
                       registry::{CleanupFailure, Release, Source, Specification}};

use crate::toolbox::handles::{CapacityPolicy, KindCode,
                              cleanup::{Failure, context::Context}};

pub(crate) fn specification(storage: bool) -> Specification<'static>
{
    Specification { kind:     KindCode(if storage { *b"DIFS" } else { *b"DIFC" }),
                    label:    if storage
                    {
                        "Driver interface storage"
                    }
                    else
                    {
                        "Driver service interfaces"
                    },
                    provider: "System Module Manager",
                    capacity: if storage
                    {
                        CapacityPolicy::HardBound { slots:  1,
                                                    reason: "One interface backing per provider \
                                                             domain" }
                    }
                    else
                    {
                        CapacityPolicy::GrowableUntilMemory
                    },
                    source:   Source::Resident }
}

pub(crate) struct ReleaseInterfaces(pub bool);
impl<C: Context + ?Sized> Release<C, Failure> for ReleaseInterfaces
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        let Some(registry) = context.driver_registry()
        else
        {
            return Err(Failure::context_unavailable(ticket, "driver registry"));
        };
        if self.0
        {
            registry.interfaces.release_storage(ticket).map_err(Failure::from)
        }
        else
        {
            let key = ticket.key();
            let Some(interface) = registry.interfaces.releasing(&ticket)
            else
            {
                return Err(Failure::registration_mismatch(ticket));
            };
            let pending = registry.requests.iter().filter(|(_, r)| r.interface_key == key).count();
            if pending != 0
            {
                return Err(Failure::driver_interface_busy(ticket, interface.module, pending));
            }
            if let Some(binding) =
                registry.dependencies.retained_values().find(|b| b.interface_key == key)
            {
                return Err(Failure::driver_dependency_busy(ticket, interface.module,
                                                           binding.dependent));
            }
            drop(registry.interfaces.release(ticket).map_err(Failure::from)?);
            Ok(())
        }
    }
}

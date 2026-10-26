//! The backing-file capability and its backing share the normal public catalog.
#![forbid(unsafe_code)]
use mac_object_table::registry::{Source, Specification};

use crate::toolbox::handles::{CapacityPolicy, KindCode};

pub(crate) fn specification(storage: bool) -> Specification<'static>
{
    Specification { kind:     KindCode(if storage { *b"FSSB" } else { *b"FSRC" }),
                    label:    if storage
                    {
                        "Backing file lease storage"
                    }
                    else
                    {
                        "Backing file leases"
                    },
                    provider: "System Backing File Service",
                    source:   Source::Resident,
                    capacity: CapacityPolicy::GrowableUntilMemory }
}

#[cfg(target_arch = "riscv64")]
pub(crate) struct ReleaseSources(pub bool);
#[cfg(target_arch = "riscv64")]
impl
    mac_object_table::registry::Release<dyn crate::toolbox::handles::cleanup::context::Context,
                                        crate::toolbox::handles::cleanup::Failure>
    for ReleaseSources
{
    fn release(&self, _: &mut (dyn crate::toolbox::handles::cleanup::context::Context + 'static),
               ticket: mac_object_table::domain::ReleasedResource)
               -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        super::runtime::release(ticket, self.0)
    }
}

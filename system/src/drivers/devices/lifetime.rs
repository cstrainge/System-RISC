//! Six registered kinds make physical targets, module aliases and mappings
//! inspectable through the same catalog and invalidate-first cleanup walk.
#![forbid(unsafe_code)]
use mac_object_table::registry::{Source, Specification};
#[cfg(target_arch = "riscv64")]
use mac_object_table::{domain::ReleasedResource, registry::Release};

#[cfg(target_arch = "riscv64")]
use crate::toolbox::handles::cleanup::{Failure, context::Context};
use crate::toolbox::handles::{CapacityPolicy, KindCode};

#[derive(Clone, Copy)]
pub(crate) enum Registration
{
    Alias,
    AliasStorage,
    Object,
    ObjectStorage,
    Map,
    MapStorage
}
impl Registration
{
    pub(crate) const ALL: [Self; 6] = [Self::Alias,
                                       Self::AliasStorage,
                                       Self::Object,
                                       Self::ObjectStorage,
                                       Self::Map,
                                       Self::MapStorage];

    pub(crate) fn specification(self) -> Specification<'static>
    {
        let (code, label) = match self
        {
            Self::Alias => (*b"DVAL", "Driver device claims"),
            Self::AliasStorage => (*b"DVAB", "Driver claim backing"),
            Self::Object => (*b"DVOB", "System physical devices"),
            Self::ObjectStorage => (*b"DVOS", "System device backing"),
            Self::Map => (*b"DMAP", "Driver DMA mappings"),
            Self::MapStorage => (*b"DMST", "Driver DMA backing")
        };
        Specification { kind: KindCode(code),
                        label,
                        provider: "System Device Manager",
                        capacity: CapacityPolicy::GrowableUntilMemory,
                        source: Source::Resident }
    }
}
#[cfg(target_arch = "riscv64")]
impl Release<dyn Context, Failure> for Registration
{
    fn release(&self, _: &mut (dyn Context + 'static), ticket: ReleasedResource)
               -> Result<(), Failure>
    {
        crate::machine::driver_devices::cleanup(ticket, *self)
    }
}

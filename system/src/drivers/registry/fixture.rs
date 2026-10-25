//! An isolated host-test world owns its registration catalog for the whole
//! lifetime of the real production driver registry. No alternative ID issuer.
use core::ops::{Deref, DerefMut};

use super::{Registry, interfaces};
use crate::toolbox::handles::{OwnerRef, cleanup::context::Context};

pub(crate) struct Fixture
{
    registry:          Registry,
    pub(crate) _kinds: crate::toolbox::handles::Registry<dyn Context>
}

impl Fixture
{
    pub(crate) fn new(target: mac_module::Target) -> Self
    {
        let owner = OwnerRef::try_new(mac_object_table::domain::OwnerIndex::issue(
            mac_object_table::domain::DomainKind::Module).unwrap()).unwrap();
        let mut kinds = crate::toolbox::handles::Registry::<dyn Context>::new();
        let interfaces = kinds.register(&owner,
                                        interfaces::specification(false),
                                        interfaces::ReleaseInterfaces(false))
                              .unwrap();
        let storage = kinds.register(&owner,
                                     interfaces::specification(true),
                                     interfaces::ReleaseInterfaces(true))
                           .unwrap();
        let registry = Registry::new(target, owner, (interfaces, storage)).unwrap();
        Self { registry,
               _kinds: kinds }
    }
}

impl Deref for Fixture
{
    type Target = Registry;

    fn deref(&self) -> &Registry
    {
        &self.registry
    }
}
impl DerefMut for Fixture
{
    fn deref_mut(&mut self) -> &mut Registry
    {
        &mut self.registry
    }
}

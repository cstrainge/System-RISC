//! An isolated host-test world owns its registration catalog for the whole
//! lifetime of the real production driver registry. No alternative ID issuer.
use core::ops::{Deref, DerefMut};

use super::{Registry, dependencies, interfaces};
use crate::toolbox::handles::{OwnerRef, cleanup::context::Context};

pub(crate) struct Fixture
{
    registry:          Registry,
    pub(crate) _kinds: alloc::sync::Arc<crate::toolbox::handles::Registry<dyn Context>>
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
        let dependency = kinds.register(&owner,
                                        dependencies::specification(false),
                                        dependencies::ReleaseDependencies(false))
                              .unwrap();
        let dependency_storage = kinds.register(&owner,
                                                dependencies::specification(true),
                                                dependencies::ReleaseDependencies(true))
                                      .unwrap();
        let kinds = alloc::sync::Arc::try_new(kinds).unwrap();
        let mut registry = Registry::new(target, owner,
            [interfaces, storage, dependency, dependency_storage]).unwrap();
        registry.fixture_catalog = Some(kinds.clone());
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

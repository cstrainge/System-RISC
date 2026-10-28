//! An isolated host-test world owns its registration catalog for the whole
//! lifetime of the real production driver registry. No alternative ID issuer.
use core::ops::{Deref, DerefMut};

use super::{Registry, dependencies, interfaces, requests};
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
        let request = kinds.register(&owner,
                                     requests::specification(false),
                                     requests::ReleaseRequests(false))
                           .unwrap();
        let request_storage =
            kinds.register(&owner, requests::specification(true), requests::ReleaseRequests(true))
                 .unwrap();
        let root = kinds.register(&owner,
                                  crate::drivers::jobs::subjects::specification(false),
                                  ReleaseIo(false))
                        .unwrap();
        let root_storage = kinds.register(&owner,
                                          crate::drivers::jobs::subjects::specification(true),
                                          ReleaseIo(true))
                                .unwrap();
        let operations =
            crate::drivers::jobs::subjects::Operations::new((root, root_storage)).unwrap();
        let kinds = alloc::sync::Arc::try_new(kinds).unwrap();
        let mut registry = Registry::new(target,
                                         owner,
                                         [interfaces, storage, dependency, dependency_storage,
                                          request, request_storage],
                                         16 << 20).unwrap();
        registry.fixture_catalog = Some(kinds.clone());
        registry.fixture_operations = Some(operations);
        Self { registry,
               _kinds: kinds }
    }
}
pub(crate) struct ReleaseIo(pub bool);
impl<C: Context + ?Sized>
    mac_object_table::registry::Release<C, crate::toolbox::handles::cleanup::Failure> for ReleaseIo
{
    fn release(&self, context: &mut C, ticket: mac_object_table::domain::ReleasedResource)
               -> core::result::Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        use crate::toolbox::handles::cleanup::Failure;
        let Some(operations) =
            context.driver_registry().and_then(|r| r.fixture_operations.as_ref())
        else
        {
            return Err(Failure::context_unavailable(ticket, "fixture I/O operations"));
        };
        operations.release(ticket, self.0)
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

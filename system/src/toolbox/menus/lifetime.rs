//! Registered menu payload and per-application menu-bar backing lifetimes.
#![forbid(unsafe_code)]
use mac_object_table::{domain::{CompletionError, ReleasedResource},
                       registry::{Release, Source, Specification}};

use crate::toolbox::handles::{CapacityPolicy, KindCode};
pub(crate) fn specification(storage: bool) -> Specification<'static>
{
    Specification { kind:     KindCode(if storage { *b"MNST" } else { *b"MENU" }),
                    label:    if storage { "Menu Manager state" } else { "Menus" },
                    provider: "System Menu Manager",
                    source:   Source::Resident,
                    capacity: if storage
                    {
                        CapacityPolicy::HardBound { slots:  1,
                                                    reason: "One menu-bar backing per application \
                                                             domain" }
                    }
                    else
                    {
                        CapacityPolicy::GrowableUntilMemory
                    } }
}
pub(crate) trait MenuHost
{
    fn release_menu(&mut self, ticket: ReleasedResource) -> Result<(), CompletionError>;
    fn release_menu_storage(&mut self, ticket: ReleasedResource) -> Result<(), CompletionError>;
}
pub(crate) struct ReleaseMenus;
pub(crate) struct ReleaseStorage;
impl<C: MenuHost> Release<C> for ReleaseMenus
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), CompletionError>
    {
        context.release_menu(ticket)
    }
}
impl<C: MenuHost> Release<C> for ReleaseStorage
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), CompletionError>
    {
        context.release_menu_storage(ticket)
    }
}

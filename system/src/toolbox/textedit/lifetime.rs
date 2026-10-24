//! Registered editor payloads and retained per-domain backing.
#![forbid(unsafe_code)]
use mac_object_table::{domain::{CompletionError, ReleasedResource},
                       registry::{Release, Source, Specification}};

use crate::toolbox::handles::{CapacityPolicy, KindCode};

pub(crate) fn specification(storage: bool) -> Specification<'static>
{
    Specification { kind:     KindCode(if storage { *b"TESV" } else { *b"TEDT" }),
                    label:    if storage { "TextEdit state" } else { "TextEdit editors" },
                    provider: "System TextEdit",
                    source:   Source::Resident,
                    capacity: if storage
                    {
                        CapacityPolicy::HardBound { slots:  1,
                                                    reason: "One TextEdit backing per domain" }
                    }
                    else
                    {
                        CapacityPolicy::GrowableUntilMemory
                    } }
}
pub(crate) trait EditorHost
{
    fn release_editor(&mut self, ticket: ReleasedResource, storage: bool)
                      -> Result<(), CompletionError>;
}
pub(crate) struct ReleaseEditors;
pub(crate) struct ReleaseStorage;
impl<C: EditorHost> Release<C> for ReleaseEditors
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), CompletionError>
    {
        context.release_editor(ticket, false)
    }
}
impl<C: EditorHost> Release<C> for ReleaseStorage
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), CompletionError>
    {
        context.release_editor(ticket, true)
    }
}

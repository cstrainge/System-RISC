//! Registry-owned drag protocol lifetimes, shared by both trap boundaries.
#![forbid(unsafe_code)]
use mac_object_table::{domain::{CapacityPolicy, CompletionError, KindCode, ReleasedResource},
                       registry::{Release, Source, Specification}};

#[derive(Clone, Copy)]
pub(crate) enum Registration
{
    Sessions,
    SessionStorage,
    Receivers,
    ReceiverStorage
}
impl Registration
{
    pub(crate) fn specification(self) -> Specification<'static>
    {
        let (code, label, slots, reason) = match self
        {
            Self::Sessions =>
            {
                (*b"DRAG",
                 "Drag sessions",
                 mac_object_table::Kind::Drags.capacity(),
                 "Existing 32-bit DragRef projection namespace")
            }
            Self::Receivers =>
            {
                (*b"DRCV",
                 "Drag receiver registrations",
                 mac_object_table::Kind::DragReceivers.capacity(),
                 "Existing 32-bit drag receiver projection namespace")
            }
            Self::SessionStorage =>
            {
                (*b"DRST", "Drag session backing", 1, "One Drag Manager session backing per domain")
            }
            Self::ReceiverStorage =>
            {
                (*b"DRVS",
                 "Drag receiver backing",
                 1,
                 "One Drag Manager receiver backing per domain")
            }
        };
        Specification { kind: KindCode(code),
                        label,
                        provider: "System Drag Manager",
                        capacity: CapacityPolicy::HardBound { slots,
                                                              reason },
                        source: Source::Resident }
    }
}
pub(crate) trait Host
{
    fn release_drag(&mut self, ticket: ReleasedResource, kind: Registration)
                    -> Result<(), CompletionError>;
}
pub(crate) struct ReleaseKind(pub(crate) Registration);
impl<C: Host> Release<C> for ReleaseKind
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), CompletionError>
    {
        context.release_drag(ticket, self.0)
    }
}

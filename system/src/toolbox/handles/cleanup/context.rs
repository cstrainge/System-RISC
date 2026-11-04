//! One resident cleanup catalog serves boot-time modules and GUI processes.
//! These borrowed Rust contexts never cross an independently linked ABI.
#![forbid(unsafe_code)]
use mac_object_table::domain::ReleasedResource;
#[cfg(guest)]
use mac_object_table::registry::Release;

use super::Failure;

pub(crate) trait Context
{
    fn driver_registry(&mut self) -> Option<&mut crate::drivers::Registry>;

    fn release_scrap_extension(&mut self, ticket: ReleasedResource,
                               registration: crate::toolbox::scrap::extension::Registration)
                               -> Result<(), Failure>
    {
        #[cfg(guest)]
        {
            crate::kernel::with_scrap_owned(ticket, |ticket, manager| {
                manager.extension_release(ticket, registration)
            }).unwrap_or_else(|(cause, ticket)| Err(Failure::scrap_unavailable(ticket, cause)))
        }
        #[cfg(not(guest))]
        {
            let _ = registration;
            Err(Failure::context_unavailable(ticket, "Scrap extension manager"))
        }
    }

    #[cfg(guest)]
    fn toolbox(&mut self) -> Option<&mut crate::toolbox::runtime::Toolbox>;
}

impl Context for crate::drivers::Registry
{
    fn driver_registry(&mut self) -> Option<&mut crate::drivers::Registry>
    {
        Some(self)
    }

    #[cfg(guest)]
    fn toolbox(&mut self) -> Option<&mut crate::toolbox::runtime::Toolbox>
    {
        None
    }
}

#[cfg(guest)]
impl Context for crate::toolbox::runtime::Toolbox
{
    fn driver_registry(&mut self) -> Option<&mut crate::drivers::Registry>
    {
        None
    }

    fn toolbox(&mut self) -> Option<&mut crate::toolbox::runtime::Toolbox>
    {
        Some(self)
    }
}

/// Retain the unique ticket if a callback requires a context that is not
/// available at this point in boot/recovery. Never fabricate a desktop or ACK.
#[cfg(guest)]
pub(crate) struct Toolbox<R>(pub R);

#[cfg(guest)]
impl<R: Release<crate::toolbox::runtime::Toolbox, Failure>> Release<dyn Context, Failure>
    for Toolbox<R>
{
    fn release(&self, context: &mut (dyn Context + 'static), ticket: ReleasedResource)
               -> Result<(), Failure>
    {
        let Some(toolbox) = context.toolbox()
        else
        {
            return Err(Failure::context_unavailable(ticket, "desktop"));
        };
        self.0.release(toolbox, ticket)
    }
}

//! One resident cleanup catalog serves boot-time modules and GUI processes.
//! These borrowed Rust contexts never cross an independently linked ABI.
#![forbid(unsafe_code)]
use mac_object_table::domain::ReleasedResource;
#[cfg(target_arch = "riscv64")]
use mac_object_table::registry::Release;

use super::Failure;

pub(crate) trait Context
{
    fn driver_registry(&mut self) -> Option<&mut crate::drivers::Registry>;

    fn release_scrap_extension(&mut self, ticket: ReleasedResource,
                               registration: crate::toolbox::scrap::extension::Registration)
                               -> Result<(), Failure>
    {
        #[cfg(target_arch = "riscv64")]
        {
            crate::kernel::with_scrap_owned(ticket, |ticket, manager| {
                manager.extension_release(ticket, registration)
            }).unwrap_or_else(|(cause, ticket)| Err(Failure::scrap_unavailable(ticket, cause)))
        }
        #[cfg(not(target_arch = "riscv64"))]
        {
            let _ = registration;
            Err(Failure::context_unavailable(ticket, "Scrap extension manager"))
        }
    }

    #[cfg(target_arch = "riscv64")]
    fn desktop(&mut self) -> Option<&mut crate::toolbox::runtime::Desktop>;
}

impl Context for crate::drivers::Registry
{
    fn driver_registry(&mut self) -> Option<&mut crate::drivers::Registry>
    {
        Some(self)
    }

    #[cfg(target_arch = "riscv64")]
    fn desktop(&mut self) -> Option<&mut crate::toolbox::runtime::Desktop>
    {
        None
    }
}

#[cfg(target_arch = "riscv64")]
impl Context for crate::toolbox::runtime::Desktop
{
    fn driver_registry(&mut self) -> Option<&mut crate::drivers::Registry>
    {
        None
    }

    fn desktop(&mut self) -> Option<&mut crate::toolbox::runtime::Desktop>
    {
        Some(self)
    }
}

/// Retain the unique ticket if a callback requires a context that is not
/// available at this point in boot/recovery. Never fabricate a desktop or ACK.
#[cfg(target_arch = "riscv64")]
pub(crate) struct Desktop<R>(pub R);

#[cfg(target_arch = "riscv64")]
impl<R: Release<crate::toolbox::runtime::Desktop, Failure>> Release<dyn Context, Failure>
    for Desktop<R>
{
    fn release(&self, context: &mut (dyn Context + 'static), ticket: ReleasedResource)
               -> Result<(), Failure>
    {
        let Some(desktop) = context.desktop()
        else
        {
            return Err(Failure::context_unavailable(ticket, "desktop"));
        };
        self.0.release(desktop, ticket)
    }
}

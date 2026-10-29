//! Replaceable scheduler policy. Core owns real admission, handover, frames,
//! claims, maintenance and quiescence. No policy callback performs a host call.
#![no_std]
#![forbid(unsafe_code)]
use mac_abi::{BAD_ABI, NOT_SUPPORTED, SERVICE_BUSY};
use mac_driver::{Client, Completion, Driver, Error, Request, Result, lifecycle::Startup,
                 scheduler::Controller, sync::Mutex};
use mac_scheduler_interface::{Decision, Round};
use mac_scheduler_policy::PriorityBands;
static CONTROL: Mutex<Option<Controller>> = Mutex::new(None);
pub struct Extension;
pub static EXTENSION: Extension = Extension;
impl Driver for Extension
{
    fn initialize(&self, client: Client) -> Result<()>
    {
        if CONTROL.with(|slot| slot.is_some())?
        {
            return Err(Error(SERVICE_BUSY));
        }
        let startup = Startup::defer(client)?;
        let prepared = (|| {
            let mut controller = Controller::new(client)?;
            let limits = controller.limits()?;
            // Request only exposed mechanisms. Their presence is not SMP
            // qualification; core still enforces every actual admission gate.
            controller.prepare(limits.features)?;
            controller.select_harts(limits.maximum_active_harts)?;
            controller.arm()?;
            Ok::<_, mac_driver::scheduler::ControlError>(controller)
        })();
        let controller = match prepared
        {
            Ok(controller) => controller,
            Err(cause) =>
            {
                let _ = startup.report(cause.status(), cause.cause());
                return Err(Error(cause.status()));
            }
        };
        // Pending startup persists until core binds the validated Instance,
        // performs a shadow decision from its own boundary and commits policy.
        CONTROL.with(|slot| *slot = Some(controller))?;
        Ok(())
    }

    fn request(&self, _: Request<'_>, _: &mut [u8]) -> Result<Completion>
    {
        Err(Error(NOT_SUPPORTED))
    }

    fn schedule(&self, round: Round<'_>) -> Result<Decision>
    {
        PriorityBands::choose(round).map_err(|_| Error(BAD_ABI))
    }

    fn quiesce(&self) -> Result<()>
    {
        let Some(mut controller) = CONTROL.with(Option::take)?
        else
        {
            return Ok(());
        };
        match controller.retire()
        {
            Ok(true) => Ok(()),
            Ok(false) =>
            {
                CONTROL.with(|slot| *slot = Some(controller))?;
                Err(Error(SERVICE_BUSY))
            }
            Err(cause) =>
            {
                let status = cause.status();
                CONTROL.with(|slot| *slot = Some(controller))?;
                Err(Error(status))
            }
        }
    }
}

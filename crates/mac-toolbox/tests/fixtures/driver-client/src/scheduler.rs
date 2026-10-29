//! This independent INIT declares SCTL/MLIF but offers no scheduler. Refusal
//! must come from the resident owner/declaration gate, not missing resolution.
use core::time::Duration;

use mac_driver::{Client, Completion, Driver, Error, Request, Result, lifecycle::Startup,
                 scheduler::Controller, sync::Mutex};
use mac_driver_alloc::{Worker, Workers};
use mac_scheduler_interface::{self as wire, control};

use crate::{Device,
            scheduler_support::{self as probe, Failure}};
static COORDINATOR: Mutex<Option<Worker>> = Mutex::new(None);
impl Driver for Device
{
    fn initialize(&self, client: Client) -> Result<()>
    {
        match initialize(client)
        {
            Ok(()) => Ok(()),
            Err(cause) =>
            {
                probe::log(format_args!("SCHEDULER_CALLER_FAILURE module={} cause={cause}\n",
                                        client.module()));
                Err(Error(cause.status()))
            }
        }
    }

    fn request(&self, _: Request<'_>, _: &mut [u8]) -> Result<Completion>
    {
        Err(Error(mac_abi::NOT_SUPPORTED))
    }

    fn quiesce(&self) -> Result<()>
    {
        let worker = COORDINATOR.with(Option::take)?;
        if let Some(worker) = worker
        {
            match worker.try_join()
            {
                Ok(Some(_)) => (),
                result =>
                {
                    let cancelled = worker.cancel();
                    COORDINATOR.with(|slot| *slot = Some(worker))?;
                    let _ = result?;
                    cancelled?;
                    return Err(Error(mac_abi::SERVICE_BUSY));
                }
            }
        }
        Ok(())
    }
}
fn initialize(client: Client) -> probe::Result<()>
{
    // All legitimate preparation preconditions other than the SCHD offer are
    // real: initializing, explicitly deferred, no dependencies, services valid.
    let startup = probe::service("defer unqualified caller", Startup::defer(client))?;
    let mut controller = Controller::new(client)?;
    let limits = controller.limits()?;
    let provider = probe::service("resolve genuine SCTL service",
                                  client.resolve(wire::CONTROL_CLASS, wire::VERSION))?;
    let requirements = control::Requirements { features:           limits.features,
                                               candidate_capacity: limits.maximum_candidates,
                                               topology_epoch:     limits.topology_epoch }.encode();
    probe::short_reply(provider, control::PREPARE, &requirements)?;
    probe::refusal(provider,
                   control::PREPARE,
                   &requirements,
                   mac_abi::ACCESS_DENIED,
                   "caller has no SCHD offer")?;
    probe::refusal(provider,
                   control::LIMITS,
                   &[1],
                   mac_abi::BAD_ARGUMENT,
                   "unqualified malformed limits")?;
    probe::require(controller.limits()? == limits, "refused caller damaged live limits")?;
    let worker =
        client.prepare_worker("unqualified scheduler caller", move |context| {
                  let outcome = (|| {
                      probe::service("confirm ordinary caller startup", startup.confirm())?;
                      let lifecycle =
                          probe::service("resolve genuine caller MLIF",
                                         client.resolve(mac_module::lifecycle::CLASS,
                                                        mac_module::lifecycle::VERSION))?;
                      probe::require(probe::startup_status(lifecycle)?.0
                                     == mac_module::lifecycle::ACTIVE,
                                     "ordinary caller startup did not become Active")?;
                      let before =
                          probe::service("caller clock", context.monotonic_microseconds())?;
                      probe::service("caller timed progress",
                                     context.park_for(Duration::from_millis(30)))?;
                      let after = probe::service("caller clock after refusal",
                                                 context.monotonic_microseconds())?;
                      probe::require(after.checked_sub(before)
                                          .is_some_and(|elapsed| elapsed >= 30_000),
                                     "caller lost native timed progress after refusal")?;
                      let mut controller = Controller::new(client)?;
                      probe::require(controller.limits()? == limits,
                                     "limits changed after caller refusal")?;
                      probe::log(format_args!("SCHEDULER_NATIVE_CALLER module={} \
                                               no_offer=refused malformed=refused limits=usable \
                                               native_worker_progress=observed\n",
                                              client.module()));
                      Ok::<_, Failure>(())
                  })();
                  if let Err(cause) = &outcome
                  {
                      probe::log(format_args!("SCHEDULER_CALLER_FAILURE module={} \
                                               cause={cause}\n",
                                              client.module()));
                  }
                  outcome.map_err(|cause| Error(cause.status()))
              })
              .map_err(|failure| {
                  Failure::Observation("prepare unqualified caller worker", failure.cause.0)
              })?;
    let worker = worker.start()
                       .map_err(|failure| {
                           Failure::Observation("arm unqualified caller worker", failure.cause.0)
                       })?;
    probe::service("retain unqualified caller worker",
                   COORDINATOR.with(|slot| *slot = Some(worker)))?;
    Ok(())
}

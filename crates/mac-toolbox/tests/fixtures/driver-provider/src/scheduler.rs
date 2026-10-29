//! Native diagnostic variation over the real SCHD/SCTL/MLIF and worker SDK.
//! Normal policy performs no allocation, broker call or diagnostic I/O. Named
//! guard features deliberately exercise one forbidden gateway/envelope only.
use core::{sync::atomic::{AtomicBool, AtomicU64, Ordering},
           time::Duration};

use mac_driver::{Client, Completion, Driver, Error, Request, Result, lifecycle::Startup,
                 scheduler::Controller, sync::Mutex};
use mac_driver_alloc::{Worker, WorkerContext, Workers};
use mac_scheduler_interface::{self as wire, Decision, Round,
                              control::{self, State}};
use mac_scheduler_policy::PriorityBands;

use crate::{Device,
            scheduler_support::{self as probe, Failure}};
const LIMIT_US: u64 = 15_000_000;
const ACTIVE_US: u64 = 3_000_000;
static CALLBACKS: AtomicU64 = AtomicU64::new(0);
static LAST_ROUND: AtomicU64 = AtomicU64::new(0);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static PRIMARY: AtomicU64 = AtomicU64::new(0);
static CALLBACK_FAULT: AtomicBool = AtomicBool::new(false);
static PINGS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "scheduler-deny")]
static DENY_CLIENT: spin::Once<Client> = spin::Once::new();
#[cfg(feature = "scheduler-deny")]
static DENY_STATUS: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(0);
static COORDINATOR: Mutex<Option<Worker>> = Mutex::new(None);
static RETAINED: Mutex<Option<Controller>> = Mutex::new(None);
impl Driver for Device
{
    fn initialize(&self, client: Client) -> Result<()>
    {
        match initialize(client)
        {
            Ok(()) => Ok(()),
            Err(cause) =>
            {
                probe::log(format_args!("SCHEDULER_FIXTURE_FAILURE module={} cause={cause}\n",
                                        client.module()));
                Err(Error(cause.status()))
            }
        }
    }

    fn request(&self, _: Request<'_>, _: &mut [u8]) -> Result<Completion>
    {
        Err(Error(mac_abi::NOT_SUPPORTED))
    }

    fn schedule(&self, round: Round<'_>) -> Result<Decision>
    {
        let context = round.context();
        let previous = LAST_ROUND.swap(context.round, Ordering::Relaxed);
        let count = CALLBACKS.fetch_add(1, Ordering::Relaxed);
        if context.generation != GENERATION.load(Ordering::Relaxed)
           || context.hart != PRIMARY.load(Ordering::Relaxed)
           || context.round <= previous
           || count == u64::MAX
        {
            CALLBACK_FAULT.store(true, Ordering::Relaxed);
            return Err(Error(mac_abi::BAD_ABI));
        }
        #[cfg(feature = "scheduler-deny")]
        {
            let client = DENY_CLIENT.get().ok_or(Error(mac_abi::BAD_ABI))?;
            // Intentionally ignore the safe gateway's refusal and return a
            // valid PriorityBands decision. Core must retain the scope's sticky
            // violation even though this author does not propagate the error.
            let status = match client.resolve(wire::CONTROL_CLASS, wire::VERSION)
            {
                Ok(_) => mac_abi::OK,
                Err(Error(status)) => status
            };
            DENY_STATUS.store(status, Ordering::Relaxed);
        }
        PriorityBands::choose(round).map_err(|_| {
                                        CALLBACK_FAULT.store(true, Ordering::Relaxed);
                                        Error(mac_abi::BAD_ABI)
                                    })
    }

    fn quiesce(&self) -> Result<()>
    {
        if cfg!(feature = "scheduler-no-defer")
        {
            let callbacks = CALLBACKS.load(Ordering::Relaxed);
            probe::log(format_args!("SCHEDULER_NATIVE_NO_DEFER_QUIESCE callbacks={callbacks}\n"));
            return if callbacks == 0 { Ok(()) } else { Err(Error(mac_abi::BAD_ABI)) };
        }
        // No module mutex is held across cancel, SCTL, status or park.
        let worker = COORDINATOR.with(Option::take)?;
        if let Some(worker) = worker
        {
            match worker.try_join()
            {
                Ok(Some(_)) => drop(worker),
                result =>
                {
                    let cancel = worker.cancel();
                    COORDINATOR.with(|slot| *slot = Some(worker))?;
                    let _ = result?;
                    cancel?;
                    return Err(Error(mac_abi::SERVICE_BUSY));
                }
            }
        }
        let controller = RETAINED.with(Option::take)?;
        let Some(mut controller) = controller
        else
        {
            return Ok(());
        };
        let status = match controller.retire()
        {
            Ok(true) => return Ok(()),
            Ok(false) => mac_abi::SERVICE_BUSY,
            Err(cause) => cause.status()
        };
        RETAINED.with(|slot| *slot = Some(controller))?;
        Err(Error(status))
    }
}
fn initialize(client: Client) -> probe::Result<()>
{
    if cfg!(feature = "scheduler-no-defer")
    {
        // Deliberately return success with an exclusive SCHD declaration but
        // without DEFER/PREPARE. Real synchronous registry commit must reject it.
        probe::log(format_args!("SCHEDULER_NATIVE_NO_DEFER_RETURN_OK module={} callbacks={}\n",
                                client.module(),
                                CALLBACKS.load(Ordering::Relaxed)));
        return Ok(());
    }
    #[cfg(feature = "scheduler-deny")]
    DENY_CLIENT.call_once(|| client);
    let startup = probe::service("defer scheduler startup", Startup::defer(client))?;
    let mut controller = Controller::new(client)?;
    let limits = controller.limits()?;
    probe::require(limits.features == control::TAKEOVER && limits.maximum_active_harts == 1,
                   "diagnostic requires real primary-only takeover limits")?;
    let provider = probe::service("resolve declared SCTL",
                                  client.resolve(wire::CONTROL_CLASS, wire::VERSION))?;
    let requirements = control::Requirements { features:           limits.features,
                                               candidate_capacity: limits.maximum_candidates,
                                               topology_epoch:     limits.topology_epoch }.encode();
    probe::short_reply(provider, control::PREPARE, &requirements)?;
    probe::refusal(provider, control::PREPARE, &[], mac_abi::BAD_ARGUMENT, "malformed prepare")?;
    let prepared = controller.prepare(limits.features)?;
    let generation = prepared.generation;
    GENERATION.store(generation, Ordering::Relaxed);
    PRIMARY.store(limits.boot_hart, Ordering::Relaxed);
    let wrong =
        generation.checked_add(1).ok_or(Failure::Observation("generation diagnostic overflow",
                                                              mac_abi::BAD_ABI))?;
    probe::refusal(provider,
                   control::STATUS,
                   &wrong.to_le_bytes(),
                   mac_abi::ACCESS_DENIED,
                   "mismatched generation")?;
    let two = control::HartRequest { generation,
                                     count: 2 }.encode()
                                               .map_err(|_| {
                                                   Failure::Observation("encode count two",
                                                                        mac_abi::BAD_ABI)
                                               })?;
    probe::refusal(provider,
                   control::HARTS,
                   &two,
                   mac_abi::NOT_SUPPORTED,
                   "two harts unsupported")?;
    let still_prepared = controller.status()?;
    probe::require(still_prepared == prepared, "refusals changed prepared grant")?;
    let selected = controller.select_harts(1)?;
    probe::short_reply(provider, control::ARM, &generation.to_le_bytes())?;
    probe::require(controller.status()? == selected, "short ARM reply had an effect")?;
    let worker = client.prepare_worker("scheduler lifecycle coordinator", move |context| {
                           let mut controller = controller;
                           let result = coordinator(&context, client, provider, generation,
                                                    &startup, &mut controller);
                           if let Err(cause) = &result
                           {
                               probe::log(format_args!("SCHEDULER_FIXTURE_FAILURE module={} \
                                                        cause={cause}\n",
                                                       client.module()));
                           }
                           // On refusal keep the same actual controller for real module cleanup;
                           // dropping a controller is not treated as a quiescence acknowledgment.
                           let retained = RETAINED.with(|slot| *slot = Some(controller));
                           match retained
                           {
                               Ok(()) => (),
                               Err(cause) =>
                               {
                                   probe::log(format_args!("SCHEDULER_FIXTURE_FAILURE \
                                                            controller retention status={} \
                                                            resident owner cleanup remains \
                                                            authoritative\n",
                                                           cause.0));
                                   return Err(cause);
                               }
                           }
                           result.map_err(|cause| Error(cause.status()))
                       })
                       .map_err(|failure| {
                           Failure::Observation("prepare genuine coordinator", failure.cause.0)
                       })?;
    let worker =
        worker.start().map_err(|failure| {
                           Failure::Observation("arm genuine coordinator", failure.cause.0)
                       })?;
    probe::service("retain coordinator", COORDINATOR.with(|slot| *slot = Some(worker)))?;
    Ok(())
}
fn now(context: &WorkerContext) -> probe::Result<u64>
{
    probe::service("authenticated worker clock", context.monotonic_microseconds())
}
fn deadline(context: &WorkerContext) -> probe::Result<u64>
{
    now(context)?.checked_add(LIMIT_US)
                 .ok_or(Failure::Observation("deadline overflow", mac_abi::BAD_ARGUMENT))
}
fn pause(context: &WorkerContext, limit: u64, stage: &'static str) -> probe::Result<()>
{
    probe::require(now(context)? < limit, stage)?;
    probe::service("native timed progress wait", context.park_for(Duration::from_millis(1)))
}
fn coordinator(context: &WorkerContext, client: Client, provider: mac_driver::Provider,
               generation: u64, startup: &Startup, controller: &mut Controller)
               -> probe::Result<()>
{
    let lifecycle = probe::service("resolve declared MLIF",
                                   client.resolve(mac_module::lifecycle::CLASS,
                                                  mac_module::lifecycle::VERSION))?;
    // The actual worker is committed and Ready, but no policy was armed in
    // initialize. Author CONFIRM must not bypass core shadow publication.
    let before = controller.status()?;
    probe::require(before.state == State::Prepared
                   && probe::startup_status(lifecycle)? == (mac_module::lifecycle::PENDING, 0)
                   && CALLBACKS.load(Ordering::Relaxed) == 0,
                   "unarmed scheduler was published or called early")?;
    probe::require(startup.confirm() == Err(Error(mac_abi::SERVICE_BUSY)),
                   "early author MLIF CONFIRM bypassed core handover")?;
    probe::require(controller.status()? == before
                   && probe::startup_status(lifecycle)? == (mac_module::lifecycle::PENDING, 0)
                   && matches!(client.resolve(wire::POLICY_CLASS, wire::VERSION),
                               Err(Error(mac_abi::ACCESS_DENIED)))
                   && CALLBACKS.load(Ordering::Relaxed) == 0,
                   "early CONFIRM changed registration/readiness/resolution")?;
    probe::log(format_args!("SCHEDULER_NATIVE_EARLY_CONFIRM module={} generation={} status={} \
                             phase=pending callbacks=0\n",
                            client.module(),
                            generation,
                            mac_abi::SERVICE_BUSY));
    controller.arm()?;
    if cfg!(any(feature = "scheduler-deny", feature = "scheduler-malformed"))
    {
        return refused_coordinator(context, client, provider, generation, lifecycle, controller);
    }
    let limit = deadline(context)?;
    loop
    {
        let (phase, startup_status) = probe::startup_status(lifecycle)?;
        let status = controller.status()?;
        if startup_status < 0
        {
            return Err(Failure::Observation("MLIF refused startup", startup_status));
        }
        if status.last_status < 0
        {
            return Err(Failure::Observation("SCTL refused startup", status.last_status));
        }
        if phase == mac_module::lifecycle::ACTIVE && status.state == State::Active
        {
            probe::require(status.generation == generation && status.active_harts == 1,
                           "actual Active identity or hart count")?;
            break;
        }
        probe::require(matches!(phase,
                                mac_module::lifecycle::PENDING | mac_module::lifecycle::ACTIVE)
                       && matches!(status.state, State::Armed | State::Active),
                       "startup refused before actual policy activation")?;
        pause(context, limit, "policy startup timed out")?;
    }
    probe::log(format_args!("SCHEDULER_NATIVE_ACTIVE module={} generation={} callbacks={}\n",
                            client.module(),
                            generation,
                            CALLBACKS.load(Ordering::Relaxed)));
    let began = now(context)?;
    let end = began.checked_add(ACTIVE_US)
                   .ok_or(Failure::Observation("active deadline overflow", mac_abi::BAD_ARGUMENT))?;
    while now(context)? < end
    {
        probe::require(!CALLBACK_FAULT.load(Ordering::Relaxed),
                       "policy observed invalid generation/round/hart/candidate")?;
        probe::service("active policy timed workload", context.park_for(Duration::from_millis(2)))?;
    }
    probe::require(CALLBACKS.load(Ordering::Relaxed) >= 128
                   && LAST_ROUND.load(Ordering::Relaxed) != 0,
                   "insufficient actual callback observations")?;
    let (stopped, observed_retiring) =
        retire_registration(context, client, provider, generation, lifecycle, controller, 0)?;
    probe::log(format_args!("SCHEDULER_NATIVE_RETIRED module={} generation={} callbacks={} \
                             retiring_observed={} recreated_workers=2 stale=refused \
                             bootstrap_progress=observed\n",
                            client.module(),
                            generation,
                            stopped,
                            observed_retiring));
    Ok(())
}
fn refused_coordinator(context: &WorkerContext, client: Client, provider: mac_driver::Provider,
                       generation: u64, lifecycle: mac_driver::Provider,
                       controller: &mut Controller)
                       -> probe::Result<()>
{
    let (kind, expected) = if cfg!(feature = "scheduler-deny")
    {
        ("gateway-denial", mac_abi::ACCESS_DENIED)
    }
    else
    {
        ("malformed-envelope", mac_abi::BAD_ABI)
    };
    let limit = deadline(context)?;
    loop
    {
        let (phase, cause) = probe::startup_status(lifecycle)?;
        let state = controller.status()?;
        probe::require(state.state != State::Active && phase != mac_module::lifecycle::ACTIVE,
                       "invalid policy entered Active instead of refusing")?;
        if phase == mac_module::lifecycle::REFUSED
        {
            probe::require(cause == expected
                           && state.last_status == expected
                           && matches!(state.state, State::Quarantined | State::Retired)
                           && CALLBACKS.load(Ordering::Relaxed) != 0
                           && !CALLBACK_FAULT.load(Ordering::Relaxed),
                           "policy guard did not retain its exact refusal")?;
            break;
        }
        pause(context, limit, "policy guard refusal timed out")?;
    }
    #[cfg(feature = "scheduler-deny")]
    probe::require(DENY_STATUS.load(Ordering::Relaxed) == mac_abi::ACCESS_DENIED,
                   "safe resolve did not return the native policy denial")?;
    #[cfg(feature = "scheduler-malformed")]
    probe::require(crate::worker_probe::malformed_count() != 0,
                   "real SDK policy return was not mutated at the fixture ABI boundary")?;
    let (stopped, observed_retiring) = retire_registration(context, client, provider, generation,
                                                           lifecycle, controller, expected)?;
    probe::log(format_args!("SCHEDULER_NATIVE_GUARD module={} generation={} kind={} status={} \
                             callbacks={} retiring_observed={} recreated_workers=2 \
                             stale=refused bootstrap_progress=observed\n",
                            client.module(),
                            generation,
                            kind,
                            expected,
                            stopped,
                            observed_retiring));
    Ok(())
}
fn retire_registration(context: &WorkerContext, client: Client, provider: mac_driver::Provider,
                       generation: u64, lifecycle: mac_driver::Provider,
                       controller: &mut Controller, expected_status: i32)
                       -> probe::Result<(u64, bool)>
{
    let limit = deadline(context)?;
    let mut observed_retiring = false;
    loop
    {
        match controller.retire()
        {
            Ok(true) => break,
            Ok(false) =>
            {
                let status = controller.status()?;
                probe::require((matches!(status.state, State::Retiring | State::Retired)
                                || expected_status < 0 && status.state == State::Quarantined)
                               && status.last_status == expected_status,
                               "retirement entered an unexpected state or retained a failure")?;
                // Real timer maintenance may finish between the false RETIRE
                // reply and this status read. Require the actual final RETIRE
                // acknowledgement/removal, rather than freezing its progress.
                if status.state == State::Retired
                {
                    continue;
                }
                observed_retiring = true;
            }
            Err(cause) if cause.status() == mac_abi::SERVICE_BUSY => (),
            Err(cause) => return Err(cause.into())
        }
        pause(context, limit, "policy retirement timed out")?;
    }
    // The canonical row has really been removed. A stale generation is refused
    // through the resident service, not an SDK-local missing-generation check.
    probe::refusal(provider,
                   control::STATUS,
                   &generation.to_le_bytes(),
                   mac_abi::NOT_FOUND,
                   "disposed generation")?;
    let (phase, startup_status) = probe::startup_status(lifecycle)?;
    probe::require(phase == mac_module::lifecycle::REFUSED
                   && startup_status
                      == if expected_status == 0
                      {
                          mac_abi::OPERATION_CANCELLED
                      }
                      else
                      {
                          expected_status
                      },
                   "withdrawn policy still advertises active MLIF startup")?;
    let stopped = CALLBACKS.load(Ordering::Relaxed);
    probe::service("bootstrap timed progress", context.park_for(Duration::from_millis(30)))?;
    probe::require(CALLBACKS.load(Ordering::Relaxed) == stopped
                   && !CALLBACK_FAULT.load(Ordering::Relaxed),
                   "callback continued after retired withdrawal")?;
    for index in 1..=2
    {
        ping(context, client, index)?;
    }
    probe::require(CALLBACKS.load(Ordering::Relaxed) == stopped,
                   "callback resumed during bootstrap worker recreation")?;
    Ok((stopped, observed_retiring))
}
fn ping(context: &WorkerContext, client: Client, index: u64) -> probe::Result<()>
{
    let limit = deadline(context)?;
    let mut body = move |_: WorkerContext| {
        PINGS.store(index, Ordering::Release);
        Ok(())
    };
    let prepared = loop
    {
        match client.prepare_worker("post-policy bootstrap worker", body)
        {
            Ok(prepared) => break prepared,
            Err(failure) =>
            {
                let cause = failure.cause;
                body = failure.body;
                if cause != Error(mac_abi::NO_MEMORY)
                {
                    return Err(Failure::Observation("bootstrap worker recreate", cause.0));
                }
                pause(context, limit, "bootstrap worker backing was not reclaimed")?;
            }
        }
    };
    let worker =
        prepared.start().map_err(|failure| {
                             Failure::Observation("start recreated worker", failure.cause.0)
                         })?;
    loop
    {
        if let Some(result) = probe::service("recreated worker join", worker.try_join())?
        {
            probe::service("recreated worker body", result)?;
            break;
        }
        pause(context, limit, "recreated worker did not exit")?;
    }
    probe::require(PINGS.load(Ordering::Acquire) == index, "recreated worker body did not run")?;
    let stale = worker.waker();
    drop(worker);
    loop
    {
        match stale.wake()
        {
            Err(Error(mac_abi::NOT_FOUND)) => break,
            Ok(()) => pause(context, limit, "disposed worker token was not retired")?,
            Err(cause) => return Err(Failure::Observation("disposed worker waker", cause.0))
        }
    }
    Ok(())
}

//! Instrumented real SCHD/SCTL owner: all CPUs, demotion, repromotion, retirement.
//! Callback membership and SCTL status do not establish author placement or
//! target-owned drain receipts. The separate body/host observations remain required.
use core::{sync::atomic::{AtomicBool, AtomicU64, Ordering},
           time::Duration};

use mac_driver::{Client, Completion, Driver, Error, Provider, Request, Result, lifecycle::Startup,
                 scheduler::Controller, sync::Mutex};
use mac_driver_alloc::{Worker, WorkerContext, Workers};
use mac_scheduler_interface::{self as wire, Decision, Round,
                              control::{self, Registration, State}};
use mac_scheduler_policy::PriorityBands;

use crate::{Device,
            scheduler_support::{self as probe, Failure}};

const ROWS: usize = control::MAX_HARTS as usize;
const LIMIT_US: u64 = 15_000_000;
const ACTIVE_US: u64 = 6_000_000;
// Distinct cache lines avoid a global diagnostic RMW on every policy decision.
// This fixed observation storage is part of the real diagnostic module image.
#[repr(align(64))]
struct Row
{
    key:     AtomicU64,
    round:   AtomicU64,
    calls:   AtomicU64,
    entered: AtomicBool
}
impl Row
{
    const fn new() -> Self
    {
        Self { key:     AtomicU64::new(0),
               round:   AtomicU64::new(0),
               calls:   AtomicU64::new(0),
               entered: AtomicBool::new(false) }
    }
}
static OBSERVATIONS: [Row; ROWS] = [const { Row::new() }; ROWS];
// Zero is an empty key; ordinary rows encode hardware_id+1. This separate row
// handles u64::MAX too, without truncating sparse/full-width hardware IDs or
// manufacturing a CPU ordinal. At most ROWS discovered CPUs may be active.
static MAX_ID: Row = Row::new();
static GENERATION: AtomicU64 = AtomicU64::new(0);
static FAULT: AtomicU64 = AtomicU64::new(0);
static PINGS: AtomicU64 = AtomicU64::new(0);
static COORDINATOR: Mutex<Option<Worker>> = Mutex::new(None);
static RETAINED: Mutex<Option<Controller>> = Mutex::new(None);

fn fault(code: u64) -> Error
{
    let _ = FAULT.compare_exchange(0, code, Ordering::Relaxed, Ordering::Relaxed);
    Error(mac_abi::BAD_ABI)
}
fn row(hardware_id: u64) -> Result<&'static Row>
{
    let Some(key) = hardware_id.checked_add(1)
    else
    {
        return Ok(&MAX_ID);
    };
    for row in &OBSERVATIONS
    {
        let current = row.key.load(Ordering::Relaxed);
        if current == key
        {
            return Ok(row);
        }
        if current == 0
        {
            match row.key.compare_exchange(0, key, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return Ok(row),
                Err(existing) if existing == key => return Ok(row),
                Err(_) => ()
            }
        }
    }
    Err(fault(1))
}
impl Driver for Device
{
    fn initialize(&self, client: Client) -> Result<()>
    {
        initialize(client).map_err(|cause| {
                              probe::log(format_args!("SCHEDULER_SMP_FAILURE module={} \
                                                       cause={cause} fault={}\n",
                                                      client.module(),
                                                      FAULT.load(Ordering::Relaxed)));
                              Error(cause.status())
                          })
    }

    fn schedule(&self, round: Round<'_>) -> Result<Decision>
    {
        let context = round.context();
        if context.generation != GENERATION.load(Ordering::Relaxed)
        {
            return Err(fault(2));
        }
        let row = row(context.hart)?;
        if row.entered.swap(true, Ordering::Relaxed)
        {
            return Err(fault(3));
        }
        // Only this physical CPU's nonreentrant callback writes these atomics;
        // the worker samples them separately. No non-atomic state is published,
        // no global success-path counter, fence, lock, service or I/O is added.
        let result = (|| {
            if context.round <= row.round.load(Ordering::Relaxed)
            {
                return Err(fault(4));
            }
            let next = row.calls.load(Ordering::Relaxed).checked_add(1).ok_or_else(|| fault(5))?;
            let decision = PriorityBands::choose(round).map_err(|_| fault(6))?;
            row.round.store(context.round, Ordering::Relaxed);
            row.calls.store(next, Ordering::Relaxed);
            Ok(decision)
        })();
        row.entered.store(false, Ordering::Relaxed);
        result
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
                Ok(Some(_)) => drop(worker),
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
        let Some(mut controller) = RETAINED.with(Option::take)?
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
    let startup = probe::service("defer all-CPU policy startup", Startup::defer(client))?;
    let mut controller = Controller::new(client)?;
    let limits = controller.limits()?;
    if limits.features & (control::TAKEOVER | control::MULTI_HART)
       != control::TAKEOVER | control::MULTI_HART
       || limits.physical_harts < 2
       || limits.maximum_active_harts != limits.physical_harts
    {
        return Err(Failure::Observation("all-CPU mechanism unavailable; no SMP acceptance",
                                        mac_abi::NOT_SUPPORTED));
    }
    let provider = probe::service("resolve original all-CPU SCTL",
                                  client.resolve(wire::CONTROL_CLASS, wire::VERSION))?;
    let requirements = control::Requirements { features:           limits.features,
                                               candidate_capacity: limits.maximum_candidates,
                                               topology_epoch:     limits.topology_epoch }.encode();
    probe::short_reply(provider, control::PREPARE, &requirements)?;
    let prepared = controller.prepare(limits.features)?;
    let generation = prepared.generation;
    GENERATION.store(generation, Ordering::Relaxed);
    controller.select_harts(limits.physical_harts)?;
    let prepared =
        client.prepare_worker("all-CPU policy coordinator", move |context| {
                  let mut controller = controller;
                  let result = coordinate(&context, client, provider, startup, &mut controller,
                                          generation, limits.boot_hart, limits.physical_harts);
                  if let Err(cause) = &result
                  {
                      probe::log(format_args!("SCHEDULER_SMP_FAILURE module={} cause={cause} \
                                               fault={}\n",
                                              client.module(),
                                              FAULT.load(Ordering::Relaxed)));
                  }
                  // This same real controller survives any failed/uncertain retirement.
                  // Resident owner cleanup remains authoritative if SDK retention refuses.
                  if let Err(cause) = RETAINED.with(|slot| *slot = Some(controller))
                  {
                      probe::log(format_args!("SCHEDULER_SMP_RETAIN_REFUSED module={} \
                                               status={}\n",
                                              client.module(),
                                              cause.0));
                      return Err(cause);
                  }
                  let worker = COORDINATOR.with(Option::take)?;
                  drop(worker); // outside every diagnostic mutex/critical guard
                  result.map_err(|cause| Error(cause.status()))
              })
              .map_err(|failure| {
                  Failure::Observation("prepare actual all-CPU coordinator", failure.cause.0)
              })?;
    let worker = prepared.start()
                         .map_err(|failure| {
                             Failure::Observation("arm actual all-CPU coordinator", failure.cause.0)
                         })?;
    probe::service("retain all-CPU coordinator before commit",
                   COORDINATOR.with(|slot| *slot = Some(worker)))
}
fn now(context: &WorkerContext) -> probe::Result<u64>
{
    probe::service("actual all-CPU worker clock", context.monotonic_microseconds())
}
fn deadline(context: &WorkerContext) -> probe::Result<u64>
{
    now(context)?.checked_add(LIMIT_US)
                 .ok_or(Failure::Observation("all-CPU deadline overflow", mac_abi::BAD_ARGUMENT))
}
fn pause(context: &WorkerContext, limit: u64) -> probe::Result<()>
{
    probe::require(now(context)? < limit, "all-CPU lifecycle progress timed out")?;
    // No mutex, critical, SCTL request borrow or policy guard survives this call.
    probe::service("all-CPU native timer wait", context.park_for(Duration::from_millis(10)))
}
fn healthy() -> probe::Result<()>
{
    probe::require(FAULT.load(Ordering::Relaxed) == 0, "per-hart policy invariant failed")
}
fn status(context: &WorkerContext, controller: &mut Controller, limit: u64)
          -> probe::Result<Registration>
{
    loop
    {
        healthy()?;
        match controller.status()
        {
            Ok(status) => return Ok(status),
            Err(cause) if cause.status() == mac_abi::SERVICE_BUSY => pause(context, limit)?,
            Err(cause) => return Err(cause.into())
        }
    }
}
fn active(context: &WorkerContext, controller: &mut Controller, generation: u64, harts: u32)
          -> probe::Result<()>
{
    let limit = deadline(context)?;
    loop
    {
        let current = status(context, controller, limit)?;
        probe::require(current.generation == generation
                       && current.last_status == 0
                       && matches!(current.state, State::Armed | State::Active),
                       "all-CPU activation lost its real registration")?;
        if current.state == State::Active && current.active_harts == harts
        {
            return Ok(());
        }
        pause(context, limit)?;
    }
}
fn counts() -> [u64; ROWS + 1]
{
    let mut result = [0; ROWS + 1];
    for (index, row) in OBSERVATIONS.iter().enumerate()
    {
        // A newly claimed row may still be entering its first callback. Ignore
        // it until its immutable key is observed; no release/acquire fence is
        // added merely to publish diagnostic counters. Later reads on this
        // coordinator cannot forget an already observed immutable key.
        if row.key.load(Ordering::Relaxed) != 0
        {
            result[index] = row.calls.load(Ordering::Relaxed);
        }
    }
    result[ROWS] = MAX_ID.calls.load(Ordering::Relaxed);
    result
}
fn hardware(index: usize) -> Option<u64>
{
    if index == ROWS
    {
        Some(u64::MAX)
    }
    else
    {
        OBSERVATIONS[index].key.load(Ordering::Relaxed).checked_sub(1)
    }
}
fn await_callbacks(context: &WorkerContext, before: &[u64; ROWS + 1], expected: u32)
                   -> probe::Result<()>
{
    let limit = deadline(context)?;
    loop
    {
        healthy()?;
        let after = counts();
        let seen = after.iter().filter(|count| **count != 0).count();
        probe::require(seen <= expected as usize,
                       "policy observed more hardware IDs than discovered CPUs")?;
        if seen == expected as usize && after.iter().zip(before).all(|(a, b)| *a == 0 || a > b)
        {
            return Ok(());
        }
        pause(context, limit)?;
    }
}
fn hold(context: &WorkerContext, microseconds: u64) -> probe::Result<()>
{
    let end =
        now(context)?.checked_add(microseconds)
                     .ok_or(Failure::Observation("all-CPU phase overflow",
                                                 mac_abi::BAD_ARGUMENT))?;
    while now(context)? < end
    {
        healthy()?;
        pause(context, end.saturating_add(LIMIT_US))?;
    }
    Ok(())
}
fn report(module: u64, generation: u64, phase: &str)
{
    let snapshot = counts();
    for (index, calls) in snapshot.iter().enumerate()
    {
        if *calls != 0
        {
            if let Some(hart) = hardware(index)
            {
                probe::log(format_args!("SCHEDULER_SMP_ROW module={module} \
                                         generation={generation} phase={phase} hart={hart} \
                                         callbacks={calls}\n"));
            }
        }
    }
}
fn select(context: &WorkerContext, controller: &mut Controller, harts: u32) -> probe::Result<()>
{
    let limit = deadline(context)?;
    loop
    {
        match controller.select_harts(harts)
        {
            Ok(_) => return Ok(()),
            Err(cause) if cause.status() == mac_abi::SERVICE_BUSY => pause(context, limit)?,
            Err(cause) => return Err(cause.into())
        }
    }
}
fn coordinate(context: &WorkerContext, client: Client, provider: Provider, startup: Startup,
              controller: &mut Controller, generation: u64, boot: u64, harts: u32)
              -> probe::Result<()>
{
    let lifecycle = probe::service("resolve original policy MLIF",
                                   client.resolve(mac_module::lifecycle::CLASS,
                                                  mac_module::lifecycle::VERSION))?;
    probe::require(startup.confirm() == Err(Error(mac_abi::SERVICE_BUSY)),
                   "all-CPU author CONFIRM bypassed actual handover")?;
    controller.arm()?;
    active(context, controller, generation, harts)?;
    probe::require(probe::startup_status(lifecycle)? == (mac_module::lifecycle::ACTIVE, 0),
                   "all-CPU policy not actually published")?;
    await_callbacks(context, &[0; ROWS + 1], harts)?;
    hold(context, ACTIVE_US)?;
    report(client.module(), generation, "all");
    probe::log(format_args!("SCHEDULER_SMP_ALL module={} generation={generation} active={harts} \
                             placement=requires-host-evidence\n",
                            client.module()));

    select(context, controller, 1)?;
    active(context, controller, generation, 1)?;
    let before = counts();
    probe::require((0..=ROWS).any(|index| before[index] != 0 && hardware(index) == Some(boot)),
                   "all-CPU callback observations omitted actual primary")?;
    hold(context, 1_000_000)?;
    let after = counts();
    for index in 0..=ROWS
    {
        let primary = hardware(index) == Some(boot);
        probe::require(if primary
                       {
                           after[index] > before[index]
                       }
                       else
                       {
                           after[index] == before[index]
                       },
                       "demoted callback continued or primary callback stopped")?;
    }
    report(client.module(), generation, "demoted");
    probe::log(format_args!("SCHEDULER_SMP_DEMOTED module={} generation={generation} active=1 \
                             target_receipts=host-required\n",
                            client.module()));

    // HARTS changes live intent on the same genuine Active registration. This
    // is not another ARM or a re-PREPARE of a withdrawn/refused module owner.
    select(context, controller, harts)?;
    active(context, controller, generation, harts)?;
    await_callbacks(context, &after, harts)?;
    hold(context, ACTIVE_US)?;
    report(client.module(), generation, "repromoted");
    probe::log(format_args!("SCHEDULER_SMP_REPROMOTED module={} generation={generation} \
                             active={harts} placement=requires-host-evidence\n",
                            client.module()));

    let limit = deadline(context)?;
    loop
    {
        match controller.retire()
        {
            Ok(true) => break,
            Ok(false) => pause(context, limit)?,
            Err(cause) if cause.status() == mac_abi::SERVICE_BUSY => pause(context, limit)?,
            Err(cause) => return Err(cause.into())
        }
    }
    probe::refusal(provider,
                   control::STATUS,
                   &generation.to_le_bytes(),
                   mac_abi::NOT_FOUND,
                   "disposed all-CPU generation")?;
    probe::require(probe::startup_status(lifecycle)?
                   == (mac_module::lifecycle::REFUSED, mac_abi::OPERATION_CANCELLED),
                   "retired all-CPU policy still published")?;
    probe::require(controller.limits()?.active_harts == 1,
                   "retired policy left author peers eligible")?;
    let stopped = counts();
    hold(context, 100_000)?;
    probe::require(stopped == counts(), "callback after acknowledged all-CPU retirement")?;
    for index in 1..=2
    {
        ping(context, client, index)?;
    }
    probe::require(stopped == counts(), "callback resumed during bootstrap worker reuse")?;
    report(client.module(), generation, "retired");
    probe::log(format_args!("SCHEDULER_SMP_RETIRED module={} generation={generation} \
                             stale=refused recreated_workers=2 target_receipts=host-required \
                             placement=requires-host-evidence\n",
                            client.module()));
    Ok(())
}
fn ping(context: &WorkerContext, client: Client, index: u64) -> probe::Result<()>
{
    let limit = deadline(context)?;
    let prepared =
        client.prepare_worker("post-SMP bootstrap worker", move |_| {
                  PINGS.store(index, Ordering::Relaxed);
                  Ok(())
              })
              .map_err(|failure| {
                  Failure::Observation("prepare post-SMP worker", failure.cause.0)
              })?;
    let worker =
        prepared.start().map_err(|failure| {
                             Failure::Observation("start post-SMP worker", failure.cause.0)
                         })?;
    loop
    {
        if let Some(result) = probe::service("post-SMP worker join", worker.try_join())?
        {
            probe::service("post-SMP worker body", result)?;
            break;
        }
        pause(context, limit)?;
    }
    probe::require(PINGS.load(Ordering::Relaxed) == index, "post-SMP body did not execute")?;
    let stale = worker.waker();
    drop(worker);
    loop
    {
        match stale.wake()
        {
            Err(Error(mac_abi::NOT_FOUND)) => return Ok(()),
            Ok(()) => pause(context, limit)?,
            Err(cause) => return Err(Failure::Observation("post-SMP stale token", cause.0))
        }
    }
}

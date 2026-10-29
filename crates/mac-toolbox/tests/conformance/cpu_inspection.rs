//! Disk-booted public CPU API observations; no replacement services or timer.
#![forbid(unsafe_code)]
use core::fmt::Write;

use mac_abi::inspection::cpus as wire;
use mac_toolbox::{Error, EventMask, System,
                  inspection::{Activity, Cpu, CpuState}};

use crate::{LogBuffer, conformance::Failure};

fn require(value: bool, message: &'static str) -> Result<(), Failure>
{
    if value { Ok(()) } else { Err(Failure::Mismatch(message)) }
}
fn service(error: mac_toolbox::inspection::Failure) -> Failure
{
    Failure::Service(error.cause.status())
}
fn detail(call: &wire::Call, fragment: &str) -> bool
{
    usize::try_from(call.detail_length).ok()
                                       .and_then(|n| call.detail.get(..n))
                                       .and_then(|bytes| core::str::from_utf8(bytes).ok())
                                       .is_some_and(|text| {
                                           !text.is_empty() && text.contains(fragment)
                                       })
}
fn residency(cpu: Cpu) -> Result<mac_toolbox::inspection::Residency, Failure>
{
    match cpu.activity
    {
        Activity::Residency(value) => Ok(value),
        Activity::Unavailable(_) =>
        {
            Err(Failure::Mismatch("admitted native CPU residency unavailable"))
        }
    }
}
fn log_row(system: &System<'_>, cpu: Cpu, index: u64) -> Result<(), Failure>
{
    let counters = residency(cpu)?;
    let mut line = LogBuffer::new();
    let _ = writeln!(line,
                     "CPU_INSPECTION_ROW ordinal={index} hardware={} primary={} admitted={} \
                      eligible={} state={:?} idle={} executing={} sampled={}",
                     cpu.hardware_id,
                     cpu.primary,
                     cpu.admitted,
                     cpu.dispatch_eligible,
                     cpu.state,
                     counters.idle_ticks,
                     counters.executing_ticks,
                     counters.sampled_at);
    system.log(line.text())?;
    Ok(())
}
pub fn run(system: &System<'_>) -> Result<(), Failure>
{
    // Packaging this run MUST omit the optional System digest-grant argument.
    match system.inspection().system_memory()
    {
        Err(error) if error.cause == Error::AccessDenied => (),
        _ => return Err(Failure::Mismatch("CPU fixture unexpectedly has inspector authority"))
    }
    let cpus = system.inspection().cpus().map_err(service)?;
    require(!cpus.is_empty() && cpus.ticks_per_second() != 0, "real discovered CPU count/clock")?;
    let mut primary = None;
    let mut previous = None;
    for index in 0..cpus.len()
    {
        let cpu = cpus.at(index).map_err(service)?;
        if let Some(last) = previous
        {
            require(cpu.hardware_id > last, "strictly sorted unique hardware labels")?;
        }
        previous = Some(cpu.hardware_id);
        require(cpu.admitted && matches!(cpu.state, CpuState::Idle | CpuState::Executing),
                "actual warm admission and selected-state observation")?;
        require(!cpu.dispatch_eligible || cpu.admitted, "dispatch eligibility requires admission")?;
        if cpu.primary
        {
            require(primary.is_none(), "exactly one actual primary")?;
            primary = Some(index);
        }
        let repeated = cpus.at(index).map_err(service)?;
        require(cpu.hardware_id == repeated.hardware_id && cpu.primary == repeated.primary,
                "immutable CPU membership across repeated reads")?;
        let first = residency(cpu)?;
        let second = residency(repeated)?;
        require(if second.sampled_at == first.sampled_at
                {
                    first.idle_ticks == second.idle_ticks
                    && first.executing_ticks == second.executing_ticks
                }
                else
                {
                    repeated.activity_since(cpu).is_some()
                },
                "monotonic coherent residency sample")?;
        log_row(system, repeated, index)?;
    }
    let primary = primary.ok_or(Failure::Mismatch("actual primary missing from CPU topology"))?;
    require(system.inspection().cpus().map_err(service)?.len() == cpus.len(),
            "CPU count independent of policy activity")?;

    let mut invalid = wire::Call { operation: wire::READ,
                                   index: cpus.len(),
                                   ..Default::default() };
    require(system.client().inspect_cpus(&mut invalid) == Err(Error::NotFound)
            && detail(&invalid, "outside"),
            "out-of-range CPU row returns informative native refusal")?;
    let mut reserved = wire::Call { reserved: 1,
                                    ..Default::default() };
    require(system.client().inspect_cpus(&mut reserved) == Err(Error::BadArgument)
            && detail(&reserved, "reserved"),
            "CPU reserved header returns informative native refusal")?;
    let mut tail = wire::Call { operation: wire::READ,
                                reserved_tail: 1,
                                ..Default::default() };
    require(system.client().inspect_cpus(&mut tail) == Err(Error::BadArgument)
            && detail(&tail, "reserved"),
            "CPU reserved tail returns informative native refusal")?;
    let mut unknown = wire::Call { operation: u32::MAX,
                                   ..Default::default() };
    require(system.client().inspect_cpus(&mut unknown) == Err(Error::BadArgument)
            && detail(&unknown, "operation"),
            "unknown CPU operation reaches informative native family refusal")?;
    let mut begin_index = wire::Call { index: 1,
                                       ..Default::default() };
    require(system.client().inspect_cpus(&mut begin_index) == Err(Error::BadArgument)
            && detail(&begin_index, "index"),
            "CPU begin index returns informative native refusal")?;
    let mut line = LogBuffer::new();
    let _ = writeln!(line,
                     "CPU_INSPECTION_ENUM count={} ticks_per_second={} no_inspector=true \
                      negative=5",
                     cpus.len(),
                     cpus.ticks_per_second());
    system.log(line.text())?;

    let before = cpus.at(primary).map_err(service)?;
    let started = system.events().monotonic_nanoseconds()?;
    let until = started.checked_add(150_000_000)
                       .ok_or(Failure::Mismatch("compute phase clock overflow"))?;
    let mut value = 1u64;
    loop
    {
        // Real ordinary-app compute between public clock observations. No
        // per-operation output or host acknowledgement serializes this loop.
        for i in 0..8192u64
        {
            value = value.rotate_left(7).wrapping_mul(0x9e3779b97f4a7c15).wrapping_add(i);
        }
        core::hint::black_box(value);
        if system.events().monotonic_nanoseconds()? >= until
        {
            break;
        }
    }
    let after = cpus.at(primary).map_err(service)?;
    let delta = after.activity_since(before)
                     .ok_or(Failure::Mismatch("compute residency delta unavailable"))?;
    require(delta.executing_ticks > 0, "actual primary executing residency increases")?;
    require(delta.executing_fraction().is_some_and(|fraction| (0.0..=1.0).contains(&fraction)),
            "checked CPU utilization fraction")?;
    let mut line = LogBuffer::new();
    let _ = writeln!(line,
                     "CPU_INSPECTION_COMPUTE hardware={} idle_delta={} executing_delta={}",
                     after.hardware_id, delta.idle_ticks, delta.executing_ticks);
    system.log(line.text())?;

    let mut idle_observed = false;
    for index in 0..cpus.len()
    {
        let before = cpus.at(index).map_err(service)?;
        system.events().wait_event(EventMask::NONE, 6)?;
        let after = cpus.at(index).map_err(service)?;
        let delta = after.activity_since(before)
                         .ok_or(Failure::Mismatch("wait residency delta unavailable"))?;
        idle_observed |= delta.idle_ticks != 0;
        let mut line = LogBuffer::new();
        let _ = writeln!(line,
                         "CPU_INSPECTION_WAIT hardware={} idle_delta={} executing_delta={} \
                          eligible={}",
                         after.hardware_id,
                         delta.idle_ticks,
                         delta.executing_ticks,
                         after.dispatch_eligible);
        system.log(line.text())?;
    }
    // Competing runnable work can legitimately consume the wait interval.
    // The capture must inspect this flag to claim an idle transition observation.
    let mut line = LogBuffer::new();
    let _ = writeln!(line,
                     "CPU_INSPECTION_PASS count={} idle_observed={} no_inspector=true",
                     cpus.len(),
                     idle_observed);
    system.log(line.text())?;
    Ok(())
}

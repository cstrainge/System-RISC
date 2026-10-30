//! Assertions run in the real prelaunched APPL, using public SDK traps. Native
//! heap/account snapshots are observations, not allocation-event instrumentation.
use core::{fmt::Write,
           sync::atomic::{AtomicBool, Ordering}};

use mac_toolbox::{Error, System, VolumeId,
                  files::{Fork, ROOT_ID},
                  inspection::{self, MemoryAccounting, Scope}};
const SECOND: u64 = 1_000_000_000;
static INSPECTION_RETRY_LOGGED: AtomicBool = AtomicBool::new(false);
#[track_caller]
fn require(ok: bool) -> Result<(), Error>
{
    assert!(ok, "native pressure invariant failed");
    Ok(())
}
fn log(system: &System<'_>, arguments: core::fmt::Arguments<'_>) -> Result<(), Error>
{
    let mut text = crate::LogBuffer::new();
    text.write_fmt(arguments).map_err(|_| Error::BadAbi)?;
    system.log(text.text())
}
fn now(system: &System<'_>) -> Result<u64, Error>
{
    system.events().monotonic_nanoseconds()
}
fn wait(system: &System<'_>) -> Result<(), Error>
{
    let deadline = now(system)?.saturating_add(8 * SECOND);
    loop
    {
        match system.client().wait_event(0, 1)
        {
            Ok(_) => return Ok(()),
            // WAIT_EVENT refuses before queue access when the Desktop guard is
            // busy. Retrying that refusal neither consumes nor duplicates work.
            Err(Error::ServiceBusy) if now(system)? < deadline => (),
            Err(error) => return Err(error)
        }
    }
}
fn stage(system: &System<'_>, name: &str) -> Result<(), Error>
{
    INSPECTION_RETRY_LOGGED.store(false, Ordering::Relaxed);
    log(system, format_args!("PRESSURE_ACTOR_STAGE {name}\n"))
}
fn inspect<T>(system: &System<'_>, name: &str, mut query: impl FnMut() -> inspection::Result<T>)
              -> Result<T, Error>
{
    let deadline = now(system)?.saturating_add(8 * SECOND);
    loop
    {
        match query()
        {
            Ok(value) => return Ok(value),
            Err(failure) if failure.cause == Error::ServiceBusy && now(system)? < deadline =>
            {
                if !INSPECTION_RETRY_LOGGED.swap(true, Ordering::Relaxed)
                {
                    log(system,
                        format_args!("PRESSURE_INSPECTION_RETRY stage={name} cause={:?} \
                                      detail={failure}\n",
                                     failure.cause))?;
                }
                // Inspection is read-only; a busy guard has not admitted work.
                match system.client().wait_event(0, 1)
                {
                    Ok(_) | Err(Error::ServiceBusy) => (),
                    Err(error) => return Err(error)
                }
            }
            // A stale row needs a fresh membership snapshot, not a retry of
            // the same row. The finisher walk handles that separately below.
            Err(failure) if failure.cause == Error::ScanStale => return Err(failure.cause),
            Err(failure) =>
            {
                log(system,
                    format_args!("PRESSURE_INSPECTION_FAIL stage={name} cause={:?} \
                                  detail={failure}\n",
                                 failure.cause))?;
                return Err(failure.cause);
            }
        }
    }
}
fn memory(system: &System<'_>) -> Result<inspection::MemorySummary, Error>
{
    inspect(system, "system-memory", || system.inspection().system_memory()).map(|m| m.summary())
}
fn read(system: &System<'_>, volume: VolumeId, name: &str) -> Result<[u8; 32], Error>
{
    let entry = system.files().lookup(volume, ROOT_ID, name)?;
    let mut file = system.files().open(volume, entry.id, Fork::Data)?;
    let mut bytes = [0; 32];
    require(file.read(&mut bytes)? == bytes.len())?;
    file.close()?;
    Ok(bytes)
}
fn word(bytes: &[u8], offset: usize) -> u64
{
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}
fn finisher(system: &System<'_>) -> Result<(u64, u64, u64), Error>
{
    for _ in 0..64
    {
        let snapshot =
            inspect(system, "domain-snapshot", || system.inspection().snapshot(Scope::All))?;
        let mut stale = false;
        for index in 0..snapshot.count
        {
            let domain = match inspect(system, "domain-row", || snapshot.at(index))
            {
                Ok(domain) => domain,
                Err(Error::ScanStale) =>
                {
                    stale = true;
                    break;
                }
                Err(error) => return Err(error)
            };
            if domain.name.as_str() == "Pressure Holder 43"
            {
                if let MemoryAccounting::ExecutionAndOwnedMemory { kernel,
                                                                   .. } = domain.accounting
                {
                    return Ok((kernel.limit, kernel.committed, kernel.reserved));
                }
                return Err(Error::BadAbi);
            }
        }
        if !stale
        {
            return Err(Error::NotFound);
        }
        wait(system)?;
    }
    Err(Error::ScanStale)
}
fn ordinary_refusal(system: &System<'_>, ordinary: &mac_toolbox::memory::Handle<'_>, phase: &str)
                    -> Result<(), Error>
{
    let own = inspect(system, "own-pressure-account", || system.inspection().snapshot(Scope::Own))?;
    require(own.count == 1)?;
    let own = inspect(system, "own-pressure-row", || own.at(0))?;
    let MemoryAccounting::ExecutionAndOwnedMemory { kernel,
                                                    .. } = own.accounting
    else
    {
        return Err(Error::BadAbi);
    };
    require(kernel.limit.saturating_sub(kernel.committed + kernel.reserved) > 2 * 1024 * 1024)?;
    require(matches!(ordinary.resize(1024 * 1024), Err(Error::MemoryFull)))?;
    require(ordinary.len()? == 16)?;
    let mut retained = [0; 16];
    require(ordinary.read(0, &mut retained)? == 16 && retained == [b'W'; 16])?;
    log(system,
        format_args!("PRESSURE_ORDINARY_REFUSAL phase={phase} bytes=1048576 cause=MemoryFull \
                      retained_bytes=16 kernel_headroom={}\n",
                     kernel.limit - kernel.committed - kernel.reserved))
}
fn await_pressure(system: &System<'_>, ordinary: &mac_toolbox::memory::Handle<'_>, phase: &str)
                  -> Result<inspection::MemorySummary, Error>
{
    let limit = now(system)? + 15 * SECOND;
    loop
    {
        let current = memory(system)?;
        if current.heap_capacity - current.heap_used < 1024 * 1024
        {
            ordinary_refusal(system, ordinary, phase)?;
            return Ok(current);
        }
        // Rendering may legitimately return ordinary cache backing. Independent
        // native holders keep applying demand; do not disable real OS recovery
        // merely to hold an assumed free-byte value constant for this workload.
        require(now(system)? < limit)?;
        wait(system)?;
    }
}
fn modal(system: &System<'_>) -> Result<(), Error>
{
    use mac_toolbox::standard_file::{Location, StandardFile};
    let service = StandardFile::new(system.client());
    let limit = now(system)? + 8 * SECOND;
    for _ in 0..2
    {
        let dialog = loop
        {
            match service.begin_put(Location::default(), "Pressure Save", "Pressure.txt")
            {
                Ok(dialog) => break dialog,
                Err(Error::ServiceBusy) if now(system)? < limit => wait(system)?,
                Err(error) => return Err(error)
            }
        };
        require(matches!(service.begin_put(Location::default(), "Concurrent", "Other.txt"),
                         Err(Error::ServiceBusy)))?;
        require(matches!(system.dialogs().out_of_memory_owned("Concurrent core alert"),
                         Err(Error::ServiceBusy)))?;
        drop(dialog);
        wait(system)?;
    }
    let alert = loop
    {
        match system.dialogs().out_of_memory_owned("Ordinary memory is exhausted. Protected core \
                                                    alert remains available.")
        {
            Ok(alert) => break alert,
            Err(Error::ServiceBusy) if now(system)? < limit => wait(system)?,
            Err(error) => return Err(error)
        }
    };
    drop(alert);
    system.log("PRESSURE_MODAL_BEGIN_PASS sessions=2 concurrent=busy core-alert=opened \
                disposed=RAII\n")
}
pub fn run(system: &System<'_>) -> Result<(), Error>
{
    // Warm lazy mapping inventory and the actual cross-domain inspection path.
    stage(system, "memory-warmup")?;
    let baseline = memory(system)?;
    require(baseline.heap_capacity == 480 * 1024 * 1024)?;
    stage(system, "finisher-warmup")?;
    let _ = finisher(system)?;
    stage(system, "mount-pressure-volume")?;
    let mut volume = None;
    for index in 0..system.files().host_export_count()?
    {
        let mounted = system.files().mount_host_export(index)?;
        if mounted.name() == "Pressure"
        {
            volume = Some(mounted.id);
            break;
        }
    }
    let volume = volume.ok_or(Error::NotFound)?;
    stage(system, "await-holder-registration")?;
    let began = now(system)?;
    let mut last_registered = None;
    loop
    {
        let registered = word(&read(system, volume, "Status")?, 0);
        // Registration is monotonic and bounded by the 44 genuine modules.
        // Emit only the first observation and the final complete observation.
        if last_registered.is_none() || registered == 44
        {
            log(system, format_args!("PRESSURE_REGISTRATION observed={registered} expected=44\n"))?;
            last_registered = Some(registered);
        }
        if registered == 44
        {
            break;
        }
        require(now(system)? - began < 60 * SECOND)?;
        wait(system)?;
    }
    // Also exercise the preadmitted UI once while healthy, before collecting baseline.
    stage(system, "healthy-modal")?;
    modal(system)?;
    // Admit this real handle while healthy. Resizing its existing payload later
    // needs ordinary resident storage, rather than the new APPL page allocator.
    // A valid 1 MiB request is below this zone's 4 MiB bound; its original owner
    // must also retain genuine Kernel headroom when the request is refused.
    let ordinary = system.memory().new_handle_clear(16)?;
    ordinary.write(0, &[b'W'; 16])?;
    stage(system, "baseline-memory")?;
    let baseline = memory(system)?;
    stage(system, "trigger-common-deadlines")?;
    let deadlines = read(system, volume, "Begin")?;
    require(word(&deadlines, 8) != 0)?;
    log(system,
        format_args!("PRESSURE_ACTOR_ARMED ordinary_used={} capacity={} holders=44 start={} \
                      release={}\n",
                     baseline.heap_used,
                     baseline.heap_capacity,
                     word(&deadlines, 8),
                     word(&deadlines, 24)))?;
    let limit = now(system)? + 75 * SECOND;
    stage(system, "await-ordinary-exhaustion")?;
    let mut recorded_after_finish = false;
    let witness = loop
    {
        let memory = memory(system)?;
        let observed = now(system)?;
        let finished = observed >= word(&deadlines, 16).saturating_mul(100) + SECOND;
        if finished && !recorded_after_finish
        {
            let budget = finisher(system)?;
            log(system,
                format_args!("PRESSURE_AFTER_FINISH used={} capacity={} free={} \
                              finisher_limit={} committed={} reserved={}\n",
                             memory.heap_used,
                             memory.heap_capacity,
                             memory.heap_capacity - memory.heap_used,
                             budget.0,
                             budget.1,
                             budget.2))?;
            recorded_after_finish = true;
        }
        if finished && memory.heap_capacity - memory.heap_used < 1024 * 1024
        {
            let budget = finisher(system)?;
            require(budget.0.saturating_sub(budget.1 + budget.2) > 1024 * 1024)?;
            // A real valid request, preserved old data and owner headroom
            // establish this bounded refusal; total unused bytes alone do not.
            ordinary_refusal(system, &ordinary, "before-modal")?;
            break (memory, budget);
        }
        if observed >= limit
        {
            log(system,
                format_args!("PRESSURE_WITNESS_TIMEOUT used={} capacity={} free={} \
                              finished={}\n",
                             memory.heap_used,
                             memory.heap_capacity,
                             memory.heap_capacity - memory.heap_used,
                             finished))?;
            require(false)?;
        }
        wait(system)?;
    };
    log(system,
        format_args!("PRESSURE_GLOBAL_WITNESS used={} capacity={} free={} \
                      physical_free_pages={} finisher_limit={} committed={} reserved={}\n",
                     witness.0.heap_used,
                     witness.0.heap_capacity,
                     witness.0.heap_capacity - witness.0.heap_used,
                     witness.0.free_pages,
                     witness.1.0,
                     witness.1.1,
                     witness.1.2))?;
    require(witness.0.free_pages > 1024)?;
    stage(system, "pressured-modal")?;
    modal(system)?;
    let _after_modal = await_pressure(system, &ordinary, "before-report")?;
    // The immutable full payload uses existing report stock; no per-chunk serial logs.
    stage(system, "full-report")?;
    let crashes = system.crashes();
    let deadline = now(system)? + 8 * SECOND;
    loop
    {
        match crashes.begin()
        {
            Ok(_) => break,
            Err(Error::ServiceBusy) if now(system)? < deadline => wait(system)?,
            Err(error) => return Err(error)
        }
    }
    let bytes = [b'P'; 512];
    let text = core::str::from_utf8(&bytes).map_err(|_| Error::BadAbi)?;
    for _ in 0..2048
    {
        crashes.append(text)?;
    }
    require(crashes.info()?.report_bytes == 1024 * 1024)?;
    let mut actual = [0; 512];
    for index in 0..2048
    {
        require(crashes.read(index * bytes.len(), &mut actual)? == bytes.len() && actual == bytes)?;
    }
    let _ = crashes.show(mac_toolbox::crashes::ReportFlags::NONE)?;
    let _after_report = await_pressure(system, &ordinary, "after-report")?;
    system.log("PRESSURE_FULL_REPORT_PASS bytes=1048576 content=exact report=shown\n")?;
    crashes.dismiss()?;
    // Leave actual Finder foreground for independent physical menu/launch observations.
    stage(system, "activate-finder")?;
    for index in 0..system.processes().count()?
    {
        let process = system.processes().at(index)?;
        if process.name() == "Finder"
        {
            system.processes().activate(process.id)?;
            break;
        }
    }
    system.log("PRESSURE_UI_INTERVAL_READY ordinary-tail=observed many-runnable-workers=true \
                timing-unqualified\n")?;
    let limit = word(&deadlines, 24).saturating_mul(100).saturating_add(60 * SECOND);
    stage(system, "await-holder-release")?;
    loop
    {
        let memory = memory(system)?;
        if memory.heap_used < baseline.heap_used + 8 * 1024 * 1024
        {
            break;
        }
        require(now(system)? < limit)?;
        wait(system)?;
    }
    system.log("MODULE_ORDINARY_PRESSURE_PASS public-heap=observed domain-headroom=observed \
                modal=bounded full-report=1048576 holders=released timing=unqualified\n")?;
    // Remain a genuine live observer; no fixture return turns it into a panic path.
    loop
    {
        wait(system)?;
    }
}

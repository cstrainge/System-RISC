//! Existing native APPL fixture, ordinary `alloc`, and the platform allocator.
//! Phase markers describe guest observations, not allocator/SMP qualification.
use alloc::{boxed::Box, vec::Vec};
use core::{alloc::Layout,
           fmt::{self, Write},
           hint::black_box};

use mac_toolbox::{Error, EventMask, ProcessId, System, ToolboxResult,
                  inspection::{AccountUsage, Domain, MemoryAccounting, OwnerKind, Pool, Scope}};

const KIB: usize = 1024;
const MIB: usize = 1024 * KIB;
const PHASES: usize = 8;
const OPERATIONS: usize = 128;
const LIVE_BLOCKS: usize = 8;

fn log(system: &System<'_>, arguments: fmt::Arguments<'_>) -> ToolboxResult<()>
{
    let mut text = crate::LogBuffer::new();
    text.write_fmt(arguments).map_err(|_| Error::BadAbi)?;
    system.log(text.text())
}
fn require(system: &System<'_>, condition: bool, stage: &str, detail: &str) -> ToolboxResult<()>
{
    if condition
    {
        return Ok(());
    }
    log(system, format_args!("HEAP_PROBE_FAIL stage={stage} detail={detail}\n"))?;
    Err(Error::BadAbi)
}
fn pattern(index: usize, seed: u64) -> u8
{
    (index as u64).wrapping_mul(0x9e37_79b9)
                  .rotate_left((index % 61) as u32)
                  .wrapping_add(seed ^ (index as u64 >> 3)) as u8
}
fn verify(system: &System<'_>, bytes: &[u8], seed: u64, stage: &str) -> ToolboxResult<()>
{
    // Force actual post-allocation loads in an optimized guest. This is local
    // compiler opacity, not a fence, barrier, host ACK, or kernel test hook.
    for (index, &actual) in black_box(bytes).iter().enumerate()
    {
        let expected = pattern(index, seed);
        if actual != expected
        {
            log(system,
                format_args!("HEAP_PROBE_FAIL stage={stage} index={index} expected={expected} \
                              actual={actual} length={}\n",
                             bytes.len()))?;
            return Err(Error::BadAbi);
        }
    }
    Ok(())
}
fn grow(system: &System<'_>, bytes: &mut Vec<u8>, length: usize, seed: u64, stage: &str)
        -> ToolboxResult<()>
{
    let previous = bytes.len();
    require(system, length >= previous, stage, "invalid workload growth length")?;
    bytes.try_reserve_exact(length - previous).map_err(|cause| {
                                                   let _ = log(system,
                                                               format_args!("HEAP_PROBE_FAIL \
                                                                             stage={stage} \
                                                                             requested={length} \
                                                                             allocation={cause}\\
                                                                             \
                                                                             n"));
                                                   Error::NoMemory
                                               })?;
    // Successful reservation covers this resize; initializing u8 cannot fail.
    bytes.resize(length, 0);
    for (index, byte) in bytes.iter_mut().enumerate().skip(previous)
    {
        *byte = black_box(pattern(index, seed));
    }
    verify(system, bytes, seed, stage)
}

fn own(system: &System<'_>, id: ProcessId) -> ToolboxResult<Domain>
{
    for _ in 0..8
    {
        let snapshot = match system.inspection().snapshot(Scope::Own)
        {
            Ok(value) => value,
            Err(cause) if cause.cause == Error::ScanStale => continue,
            Err(cause) =>
            {
                log(system, format_args!("HEAP_PROBE_FAIL stage=own-snapshot detail={cause}\n"))?;
                return Err(cause.cause);
            }
        };
        require(system, snapshot.count == 1, "own-snapshot", "expected one own process domain")?;
        match snapshot.at(0)
        {
            Ok(domain) =>
            {
                require(system,
                        domain.incarnation == id.0 && domain.kind == OwnerKind::Process,
                        "own-snapshot",
                        "public snapshot returned a foreign domain")?;
                return Ok(domain);
            }
            Err(cause) if cause.cause == Error::ScanStale => continue,
            Err(cause) =>
            {
                log(system, format_args!("HEAP_PROBE_FAIL stage=own-domain detail={cause}\n"))?;
                return Err(cause.cause);
            }
        }
    }
    log(system,
        format_args!("HEAP_PROBE_FAIL stage=own-snapshot detail=membership churn exhausted \
                      eight retries\n"))?;
    Err(Error::ScanStale)
}
fn application(system: &System<'_>, domain: &Domain) -> ToolboxResult<AccountUsage>
{
    if let MemoryAccounting::ExecutionAndOwnedMemory { application,
                                                       closing: false,
                                                       .. } = domain.accounting
    {
        return Ok(application);
    }
    log(system,
        format_args!("HEAP_PROBE_FAIL stage=own-account app={} detail=missing or closing \
                      actual account\n",
                     domain.incarnation))?;
    Err(Error::BadAbi)
}
fn account(system: &System<'_>, id: ProcessId, stage: &str) -> ToolboxResult<AccountUsage>
{
    let domain = own(system, id)?;
    let usage = application(system, &domain)?;
    log(system,
        format_args!("HEAP_PROBE_ACCOUNT stage={stage} app={} limit={} committed={} \
                      reserved={} births={} deaths={} pending={} quarantine={} faults={}\n",
                     id.0,
                     usage.limit,
                     usage.committed,
                     usage.reserved,
                     domain.births,
                     domain.deaths,
                     domain.pending,
                     domain.quarantined,
                     domain.completion_faults))?;
    Ok(usage)
}
fn allocations(system: &System<'_>, id: ProcessId) -> ToolboxResult<Pool>
{
    'snapshot: for _ in 0..8
    {
        let attempt: core::result::Result<Option<Pool>, mac_toolbox::inspection::Failure> =
            (|| {
                let snapshot = system.inspection().snapshot(Scope::Own)?;
                if snapshot.count != 1
                {
                    return Ok(None);
                }
                let ledger = snapshot.ledger(0)?;
                if ledger.domain().incarnation != id.0
                {
                    return Ok(None);
                }
                for index in 0..ledger.len()
                {
                    let pool = ledger.at(index)?;
                    if pool.kind == *b"AALC"
                    {
                        return Ok(Some(pool));
                    }
                }
                Ok(None)
            })();
        match attempt
        {
            Ok(Some(pool)) => return Ok(pool),
            Ok(None) =>
            {
                log(system,
                    format_args!("HEAP_PROBE_FAIL stage=own-pool detail=own AALC pool absent\n"))?;
                return Err(Error::BadAbi);
            }
            Err(cause) if cause.cause == Error::ScanStale => continue 'snapshot,
            Err(cause) =>
            {
                log(system, format_args!("HEAP_PROBE_FAIL stage=own-pool detail={cause}\n"))?;
                return Err(cause.cause);
            }
        }
    }
    log(system,
        format_args!("HEAP_PROBE_FAIL stage=own-pool detail=ledger churn exhausted eight \
                      retries\n"))?;
    Err(Error::ScanStale)
}
/// One valid low-level request preserves the typed resident diagnostic that
/// GlobalAlloc necessarily reduces to null / TryReserveError for collections.
fn typed_refusal(system: &System<'_>, id: ProcessId, requested: usize) -> ToolboxResult<()>
{
    let layout = Layout::from_size_align(requested, 8).map_err(|_| Error::BadArgument)?;
    // SAFETY: this is a nonzero valid Layout. Failure publishes no ownership;
    // unexpected success is exclusively held and disposed with the same exact
    // Layout below, without dereference or aliases. No IDs or account grants
    // are supplied: actual execution context remains the sole authority.
    match unsafe { system.client().allocate_application(layout, false) }
    {
        Err(cause) =>
        {
            // The SDK returns a Resident NoMemory only after checking nonempty
            // bounded UTF-8 detail. Missing/invalid detail is Protocol/BadAbi,
            // so it cannot accidentally satisfy this typed check.
            if cause.cause() != Error::NoMemory
            {
                log(system,
                    format_args!("HEAP_PROBE_FAIL stage=typed-budget-refusal app={} \
                                  detail={cause}\n",
                                 id.0))?;
                return Err(cause.cause());
            }
            log(system,
                format_args!("HEAP_PROBE_TYPED_REFUSAL app={} requested={requested} \
                              detail={cause}\n",
                             id.0))
        }
        Ok(pointer) =>
        {
            // SAFETY: the preceding successful call gave this sole live block
            // with this original exact Layout; no reference was created.
            if let Err(cause) = unsafe { system.client().deallocate_application(pointer, layout) }
            {
                log(system,
                    format_args!("HEAP_PROBE_FAIL stage=unexpected-budget-allocation-cleanup \
                                  app={} detail={cause}\n",
                                 id.0))?;
                return Err(cause.cause());
            }
            log(system,
                format_args!("HEAP_PROBE_FAIL stage=typed-budget-refusal app={} \
                              detail=over-budget allocation succeeded; exact block disposed\n",
                             id.0))?;
            Err(Error::BadAbi)
        }
    }
}
fn growth_and_refusal(system: &System<'_>, id: ProcessId, budget: u64, seed: u64)
                      -> ToolboxResult<()>
{
    let mut bytes = Vec::new();
    grow(system, &mut bytes, 96 * KIB + 17, seed, "initial-extent")?;
    let initial_capacity = bytes.capacity();
    grow(system, &mut bytes, 640 * KIB + 31, seed, "growth")?;
    require(system,
            bytes.capacity() > initial_capacity && bytes.capacity() > 64 * KIB,
            "growth",
            "workload did not grow beyond the minimum extent")?;
    log(system,
        format_args!("HEAP_PROBE_GROW_DONE app={} initial_capacity={initial_capacity} \
                      grown_capacity={} retained={}\n",
                     id.0,
                     bytes.capacity(),
                     bytes.len()))?;
    let old_capacity = bytes.capacity();
    bytes.truncate(32 * KIB + 7);
    bytes.try_shrink_to_fit().map_err(|cause| {
                                  let _ = log(system,
                                              format_args!("HEAP_PROBE_FAIL stage=shrink \
                                                            allocation={cause}\n"));
                                  Error::NoMemory
                              })?;
    require(system,
            bytes.capacity() < old_capacity && bytes.capacity() >= bytes.len(),
            "shrink",
            "capacity did not shrink or fell below live length")?;
    verify(system, &bytes, seed, "shrink")?;
    log(system,
        format_args!("HEAP_PROBE_SHRINK_DONE app={} old_capacity={old_capacity} capacity={} \
                      retained={}\n",
                     id.0,
                     bytes.capacity(),
                     bytes.len()))?;

    let before = account(system, id, "before-budget-refusal")?;
    let requested = budget.checked_add(MIB as u64).and_then(|value| usize::try_from(value).ok());
    let requested = match requested
    {
        Some(value) if value <= isize::MAX as usize && value > bytes.len() => value,
        _ =>
        {
            log(system,
                format_args!("HEAP_PROBE_FAIL stage=budget-request detail=budget does not \
                              permit valid over-limit byte layout\n"))?;
            return Err(Error::BadAbi);
        }
    };
    let old_pointer = bytes.as_ptr();
    let old_capacity = bytes.capacity();
    let old_length = bytes.len();
    match bytes.try_reserve_exact(requested - bytes.len())
    {
        Err(cause) =>
        {
            log(system,
                format_args!("HEAP_PROBE_EXPECTED_REFUSAL app={} requested={requested} \
                              limit={budget} detail={cause}\n",
                             id.0))?
        }
        Ok(()) =>
        {
            log(system,
                format_args!("HEAP_PROBE_FAIL stage=budget-refusal detail=valid over-budget \
                              reservation succeeded\n"))?;
            return Err(Error::BadAbi);
        }
    }
    require(system,
            bytes.len() == old_length
            && bytes.capacity() == old_capacity
            && bytes.as_ptr() == old_pointer,
            "budget-refusal",
            "failed reservation changed old ownership")?;
    verify(system, &bytes, seed, "budget-refusal-retained-data")?;
    typed_refusal(system, id, requested)?;
    verify(system, &bytes, seed, "typed-refusal-retained-data")?;
    let after = account(system, id, "after-budget-refusal")?;
    require(system,
            before == after,
            "budget-refusal",
            "failed reservation changed Application accounting")?;
    // A separate valid allocation exercises continued use before the old block
    // is disposed. This deliberately does not force the infallible OOM handler.
    let mut following = Vec::new();
    grow(system, &mut following, 192 * KIB + 19, seed ^ 0x1964, "after-refusal-allocation")?;
    verify(system, &bytes, seed, "old-data-after-following-allocation")?;
    drop(following);
    drop(bytes);
    log(system,
        format_args!("HEAP_PROBE_REFUSAL_DONE app={} preserved=pointer-capacity-data-account \
                      subsequent_allocation=done\n",
                     id.0))
}
fn zeroed(system: &System<'_>, id: ProcessId, seed: u64) -> ToolboxResult<()>
{
    // This toolchain exposes the fallible zeroed Box API under allocator_api.
    let uninitialized = Box::<[u64]>::try_new_zeroed_slice(8195).map_err(|cause| {
                                                                    let _ = log(system,
                                        format_args!("HEAP_PROBE_FAIL stage=zeroed-box \
                                                      allocation={cause}\n"));
                                                                    Error::NoMemory
                                                                })?;
    // SAFETY: every bit pattern is a valid u64; the fallible zeroed constructor
    // initializes every byte of every element. No raw pointer or service request
    // is authored here; Box retains sole disposal ownership and exact layout.
    let mut words = unsafe { uninitialized.assume_init() };
    for (index, &word) in black_box(&*words).iter().enumerate()
    {
        if word != 0
        {
            log(system,
                format_args!("HEAP_PROBE_FAIL stage=zeroed-box index={index} expected=0 \
                              actual={word}\n"))?;
            return Err(Error::BadAbi);
        }
    }
    for (index, word) in words.iter_mut().enumerate()
    {
        *word = black_box(seed.wrapping_add(index as u64 * 0x10001));
    }
    for (index, &word) in black_box(&*words).iter().enumerate()
    {
        let expected = seed.wrapping_add(index as u64 * 0x10001);
        if word != expected
        {
            log(system,
                format_args!("HEAP_PROBE_FAIL stage=zeroed-box-write index={index} \
                              expected={expected} actual={word}\n"))?;
            return Err(Error::BadAbi);
        }
    }
    drop(words);
    log(system,
        format_args!("HEAP_PROBE_ZEROED_DONE app={} words=8195 alignment={} bytes={} drop=done\n",
                     id.0,
                     core::mem::align_of::<u64>(),
                     8195 * core::mem::size_of::<u64>()))
}
struct Block
{
    bytes: Vec<u8>,
    seed:  u64
}
fn overlapping(system: &System<'_>, id: ProcessId, seed: u64) -> ToolboxResult<()>
{
    let sizes = [17, 4093, 65_537, 131_101, 262_153, 1_048_583, 3, 32_777];
    let mut blocks: [Option<Block>; LIVE_BLOCKS] = core::array::from_fn(|_| None);
    let mut state = seed;
    let mut allocated = 0u64;
    let mut total_bytes = 0u64;
    for phase in 0..PHASES
    {
        for operation in 0..OPERATIONS
        {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let index = (state >> 32) as usize % LIVE_BLOCKS;
            if let Some(previous) = blocks[index].take()
            {
                verify(system, &previous.bytes, previous.seed, "reuse-old-live-data")?;
                drop(previous);
            }
            let length =
                sizes[(state >> 19) as usize % sizes.len()] + ((state >> 11) as usize & 31);
            let mut bytes = Vec::new();
            grow(system, &mut bytes, length, state, "overlapping-new-block")?;
            allocated += 1;
            total_bytes += length as u64;
            blocks[index] = Some(Block { bytes,
                                         seed: state });
            // Retain other allocations through this replacement. Check another
            // actual live block rather than serializing through a host driver.
            let other = (index + 3) % LIVE_BLOCKS;
            if let Some(retained) = &blocks[other]
            {
                verify(system, &retained.bytes, retained.seed, "overlapping-retained-block")?;
            }
            if operation % 16 == 15
            {
                let _ = system.wait_event(EventMask::NONE, 0)?;
            }
        }
        log(system,
            format_args!("HEAP_PROBE_PHASE app={} phase={phase} allocations={allocated} \
                          bytes={total_bytes} live_slots={} seed={state}\n",
                         id.0,
                         blocks.iter().filter(|block| block.is_some()).count()))?;
    }
    for block in blocks.into_iter().flatten()
    {
        verify(system, &block.bytes, block.seed, "final-live-data")?;
        drop(block);
    }
    log(system,
        format_args!("HEAP_PROBE_REUSE_DONE app={} phases={PHASES} allocations={allocated} \
                      bytes={total_bytes} retained_blocks=dropped\n",
                     id.0))
}
fn settled(system: &System<'_>, id: ProcessId, baseline: AccountUsage) -> ToolboxResult<()>
{
    let started = system.events().monotonic_nanoseconds()?;
    for _ in 0..180
    {
        let domain = own(system, id)?;
        let usage = application(system, &domain)?;
        let pool = allocations(system, id)?;
        if usage == baseline
           && pool.live == 0
           && pool.pending == 0
           && pool.quarantined == 0
           && pool.completion_faults == 0
        {
            require(system,
                    pool.births == pool.deaths && pool.births > 0 && !pool.counters_saturated,
                    "settlement",
                    "AALC births/deaths did not balance after actual drops")?;
            log(system,
                format_args!("HEAP_PROBE_SETTLED app={} committed={} reserved={} aalcs_live={} \
                              pending={} quarantine={} faults={} births={} deaths={}\n",
                             id.0,
                             usage.committed,
                             usage.reserved,
                             pool.live,
                             pool.pending,
                             pool.quarantined,
                             pool.completion_faults,
                             pool.births,
                             pool.deaths))?;
            return Ok(());
        }
        if system.events().monotonic_nanoseconds()?.saturating_sub(started) >= 3_000_000_000
        {
            break;
        }
        // Native thread waiting only. Other apps run freely; no host ACK or
        // test-only global barrier serializes the cleanup workload.
        let _ = system.wait_event(EventMask::NONE, 1)?;
    }
    let usage = account(system, id, "settlement-refused")?;
    let pool = allocations(system, id)?;
    log(system,
        format_args!("HEAP_PROBE_FAIL stage=settlement app={} expected_committed={} \
                      actual_committed={} expected_reserved={} actual_reserved={} live={} \
                      pending={} quarantine={} faults={} births={} deaths={}\n",
                     id.0,
                     baseline.committed,
                     usage.committed,
                     baseline.reserved,
                     usage.reserved,
                     pool.live,
                     pool.pending,
                     pool.quarantined,
                     pool.completion_faults,
                     pool.births,
                     pool.deaths))?;
    Err(Error::BadAbi)
}
/// Leave one genuine registered allocation for process teardown, deliberately
/// suppressing its Rust destructor. This private workload checks the System's
/// ownership walk after the real thread is quiescent, not only Vec's Drop.
fn leave_for_exit(system: &System<'_>, id: ProcessId, baseline: AccountUsage, seed: u64)
                  -> ToolboxResult<()>
{
    let mut bytes = Vec::new();
    grow(system, &mut bytes, 160 * KIB + 23, seed, "exit-owned-allocation")?;
    let usage = application(system, &own(system, id)?)?;
    let pool = allocations(system, id)?;
    require(system,
            pool.live == 1 && usage.committed > baseline.committed,
            "exit-owned-allocation",
            "actual registered payload was not retained")?;
    let bytes = black_box(bytes);
    core::mem::forget(bytes);
    log(system,
        format_args!("HEAP_PROBE_EXIT_RETAINED app={} live={} committed={} reserved={} \
                      rust_drop=suppressed system_walk=required\n",
                     id.0, pool.live, usage.committed, usage.reserved))
}

pub(crate) fn run(system: &System<'_>) -> ToolboxResult<()>
{
    let me = system.processes().current()?;
    let baseline = account(system, me.id, "baseline")?;
    let mut text = [0; 32];
    require(system,
            system.resources().application().string(2000, &mut text)? == "heap-probe",
            "package",
            "expected resources.heap.json with platform application runtime")?;
    log(system,
        format_args!("HEAP_PROBE_BEGIN app={} thread={} cnid={} phases={PHASES} \
                      operations_per_phase={OPERATIONS} baseline_application={} budget={}\n",
                     me.id.0, me.thread.0, me.cnid.0, baseline.committed, baseline.limit))?;
    let seed = me.id.0 ^ 0x9a73_d54f_c210_6be8;
    growth_and_refusal(system, me.id, baseline.limit, seed)?;
    zeroed(system, me.id, seed)?;
    overlapping(system, me.id, seed)?;
    settled(system, me.id, baseline)?;
    leave_for_exit(system, me.id, baseline, seed ^ 0x6578_6974)?;
    log(system,
        format_args!("HEAP_PROBE_DONE app={} thread={} outcome=observed \
                      coverage=vec-growth-shrink-budget-zeroed-box-drop-reuse-owner-walk\n",
                     me.id.0, me.thread.0))
}

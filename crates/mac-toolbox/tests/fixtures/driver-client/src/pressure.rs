//! Real module-owned ordinary allocations. The clock edge is the existing
//! private QEMU fixture boundary; 44 runnable holders are not a timing claim.
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use mac_driver::{ClassId, Client, Completion, Driver, Error, Phase, Request, Result, sync::Mutex};
use mac_driver_alloc::{Worker, WorkerContext, Workers};

use crate::{Device, machine};
const SECOND: u64 = 10_000_000;
const COUNT: usize = 44;
const FINISHER_PAYLOAD: usize = 8 * 1024 * 1024;
static CONTROLLER: Mutex<Option<Worker>> = Mutex::new(None);
static INITIALIZING: AtomicBool = AtomicBool::new(true);
fn ordinal() -> usize
{
    option_env!("SYSTEM7_PRESSURE_ORDINAL").unwrap_or("0")
                                           .bytes()
                                           .fold(0usize, |n, b| n * 10 + usize::from(b - b'0'))
}
impl Driver for Device
{
    fn initialize(&self, host: Client) -> Result<()>
    {
        let provider = host.resolve(ClassId(*b"PCTL"), 1)?;
        let prepared = host.prepare_worker("ordinary pressure holder", move |context| {
                               let result = run(context, host, provider);
                               if let Err(error) = result
                               {
                                   machine::log(format_args!("PRESSURE_WORKER_FAIL module={} \
                                                              ordinal={} cause={error:?}\n",
                                                             host.module(),
                                                             ordinal()));
                               }
                               result
                           })
                           .map_err(|failure| failure.cause)?;
        let worker = prepared.start().map_err(|failure| failure.cause)?;
        CONTROLLER.with(|slot| *slot = Some(worker))?;
        INITIALIZING.store(false, Ordering::Release);
        Ok(())
    }

    fn request(&self, _: Request<'_>, _: &mut [u8]) -> Result<Completion>
    {
        Err(Error(mac_abi::NOT_SUPPORTED))
    }

    fn quiesce(&self) -> Result<()>
    {
        let worker = CONTROLLER.with(Option::take)?;
        drop(worker);
        Ok(())
    }
}
fn exchange(provider: mac_driver::Provider, operation: u32, input: &[u8]) -> Result<[u8; 32]>
{
    let limit = machine::now() + 180 * SECOND;
    let mut ticket = 0;
    loop
    {
        let phase = if ticket == 0 { Phase::Start } else { Phase::Poll };
        let mut output = [0; 32];
        let before = ticket;
        let delivered_input = if phase == Phase::Start { input } else { &[] };
        let result = provider.invoke(&mut ticket, phase, operation, delivered_input, &mut output);
        match result
        {
            Ok(Completion::Complete(32)) if ticket == 0 => return Ok(output),
            Ok(Completion::Pending) => (),
            Err(Error(mac_abi::SERVICE_BUSY)) => (),
            _ =>
            {
                // One terminal-unhappy diagnostic, never a log on each retry.
                // Preserve the actual phase and ticket transition rather than
                // guessing whether the provider entered before contention.
                machine::log(format_args!("PRESSURE_EXCHANGE_FAIL ordinal={} \
                                           operation={operation} phase={phase:?} \
                                           ticket_before={before} ticket_after={ticket} \
                                           input_bytes={} output_bytes={} result={result:?}\n",
                                          ordinal(),
                                          delivered_input.len(),
                                          output.len()));
                return match result
                {
                    Err(error) if ticket == 0 => Err(error),
                    _ => Err(Error(mac_abi::BAD_ABI))
                };
            }
        }
        // An outstanding ticket is never abandoned as a fictitious ACK. This
        // diagnostic's controller completes synchronously; a broken protocol is
        // a fatal fixture failure rather than a silent retained continuation.
        assert!(machine::now() < limit, "pressure controller did not complete its real request");
    }
}
fn word(bytes: &[u8], offset: usize) -> u64
{
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}
fn fill_tail(chunks: &mut [Vec<u8>], used: &mut usize, bytes: &mut usize, refused: &mut usize,
             ordinal: usize)
             -> bool
{
    let mut request = 64 * 1024;
    while *used < chunks.len() && *bytes < FINISHER_PAYLOAD
    {
        request = request.min(FINISHER_PAYLOAD - *bytes);
        let chunk = &mut chunks[*used];
        if chunk.try_reserve_exact(request).is_err()
        {
            *refused += 1;
            if request == 1
            {
                return true;
            }
            request = (request / 16).max(1);
            continue;
        }
        chunk.resize(request, ordinal as u8);
        *bytes += chunk.capacity();
        *used += 1;
    }
    false
}
fn run(context: WorkerContext, host: Client, provider: mac_driver::Provider) -> Result<()>
{
    assert!(!INITIALIZING.load(Ordering::Acquire), "pressure worker entered before module commit");
    let ordinal = ordinal();
    assert!(ordinal < COUNT);
    let mut registration = [0; 16];
    registration[..8].copy_from_slice(&host.module().to_le_bytes());
    registration[8..].copy_from_slice(&(ordinal as u64).to_le_bytes());
    exchange(provider, 1, &registration)?;
    let began = machine::now();
    let mut broker_ready = false;
    let deadlines = loop
    {
        let reply = exchange(provider, 2, &[])?;
        if !broker_ready
        {
            // Both real Start-only operations have returned their full reply
            // and cleared their canonical tickets before this one-shot marker.
            machine::log(format_args!("BROKER_CLIENT_READY ordinal={ordinal} module={}\n",
                                      host.module()));
            broker_ready = true;
        }
        if word(&reply, 8) != 0
        {
            break reply;
        }
        assert!(machine::now() - began < 180 * SECOND, "pressure actor never triggered start");
        let next = machine::now() + SECOND / 10;
        while machine::now() < next
        {
            core::hint::spin_loop();
        }
    };
    let start = word(&deadlines, 8);
    let finish = word(&deadlines, 16);
    let release = word(&deadlines, 24);
    assert!(machine::now() < start && start < finish && finish < release,
            "missed common pressure deadline");
    // No broker operation is needed after this point, including release at a
    // completely exhausted ordinary heap. Only bounded worker cancellation is queried.
    while machine::now() < start
    {
        core::hint::spin_loop();
    }
    let mut chunks: [Vec<u8>; 2048] = core::array::from_fn(|_| Vec::new());
    let mut used = 0;
    let mut bytes = 0;
    let mut refused = 0;
    let mut minimum_refused = false;
    if ordinal + 1 == COUNT
    {
        while machine::now() < finish
        {
            core::hint::spin_loop();
        }
        minimum_refused = fill_tail(&mut chunks, &mut used, &mut bytes, &mut refused, ordinal);
    }
    else
    {
        for request in [4 * 1024 * 1024, 4 * 1024 * 1024, 3 * 1024 * 1024]
        {
            let chunk = &mut chunks[used];
            if chunk.try_reserve_exact(request).is_err()
            {
                refused += 1;
                continue;
            }
            chunk.resize(request, ordinal as u8);
            bytes += chunk.capacity();
            used += 1;
        }
    }
    machine::log(format_args!("PRESSURE_HOLD module={} ordinal={ordinal} bytes={bytes} \
                               chunks={used} refused={refused} \
                               minimum_refused={minimum_refused} release={release}\n",
                              host.module()));
    let mut next_cancel = machine::now();
    let mut next_fill = machine::now() + SECOND / 4;
    while machine::now() < release
    {
        // Real recovery may release ordinary caches. Keep independent native
        // demand active rather than require the OS to preserve those caches or
        // serialize each allocation through a host acknowledgment.
        if ordinal + 1 == COUNT && machine::now() >= next_fill
        {
            minimum_refused |= fill_tail(&mut chunks, &mut used, &mut bytes, &mut refused, ordinal);
            next_fill = machine::now() + SECOND / 4;
        }
        if machine::now() >= next_cancel
        {
            if context.cancelled()?
            {
                return Err(Error(mac_abi::OPERATION_CANCELLED));
            }
            next_cancel = machine::now() + SECOND;
        }
        core::hint::spin_loop();
    }
    let checksum =
        chunks[..used].iter().fold(0usize, |sum, chunk| {
                                 sum + usize::from(*core::hint::black_box(chunk.last().unwrap()))
                             });
    drop(chunks);
    machine::log(format_args!("PRESSURE_RELEASE module={} ordinal={ordinal} bytes={bytes} \
                               checksum={checksum} refused={refused} \
                               minimum_refused={minimum_refused}\n",
                              host.module()));
    // This fixture no longer needs a finished-worker status record. Release
    // its real controller before returning; EXIT alone reclaims execution
    // backing but deliberately preserves a controller's queryable identity.
    let controller = CONTROLLER.with(Option::take)?;
    drop(controller);
    Ok(())
}

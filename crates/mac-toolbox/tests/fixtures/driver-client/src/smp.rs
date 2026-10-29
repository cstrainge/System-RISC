//! Real Automatic workers; body completion alone is not placement evidence.
use alloc::vec::Vec;
use core::{fmt::{Arguments, Write},
           sync::atomic::{AtomicBool, AtomicU64, Ordering},
           time::Duration};

use mac_driver::{Client, Completion, Driver, Error, Request, Result, sync::Mutex};
use mac_driver_alloc::{Worker, WorkerContext, Workers};

#[path = "../../../riscv_smp_body.rs"]
mod body;
core::arch::global_asm!(include_str!("../../../riscv_smp_body.S"),
                        "SMP_BODY __smp_worker_a_body, __smp_worker_a_progress",
                        "SMP_BODY __smp_worker_b_body, __smp_worker_b_progress");
#[used]
#[unsafe(no_mangle)]
static __smp_worker_a_progress: AtomicU64 = AtomicU64::new(0);
#[used]
#[unsafe(no_mangle)]
static __smp_worker_b_progress: AtomicU64 = AtomicU64::new(0);
unsafe extern "C" {
    fn __smp_worker_a_body(ticks: u64, seed: u64, ecalls: usize, output: *mut body::Capture);
    fn __smp_worker_b_body(ticks: u64, seed: u64, ecalls: usize, output: *mut body::Capture);
}
static PEER_ENTERED: AtomicBool = AtomicBool::new(false);
static SHARED: Mutex<u64> = Mutex::new(0);
static CONTROLLERS: Mutex<Option<[Worker; 2]>> = Mutex::new(None);

// This diagnostic targets the existing QEMU virt 10 MHz clock, just like the
// retained native worker fixture. It is not a platform timebase API.
const HOLD_TICKS: u64 = 20_000_000;

fn log(arguments: Arguments<'_>) -> Result<()>
{
    struct Text
    {
        bytes:  [u8; 512],
        length: usize
    }
    impl Write for Text
    {
        fn write_str(&mut self, value: &str) -> core::fmt::Result
        {
            let end = self.length.checked_add(value.len()).ok_or(core::fmt::Error)?;
            let destination = self.bytes.get_mut(self.length..end).ok_or(core::fmt::Error)?;
            destination.copy_from_slice(value.as_bytes());
            self.length = end;
            Ok(())
        }
    }
    let mut text = Text { bytes:  [0; 512],
                          length: 0 };
    text.write_fmt(arguments).map_err(|_| Error(mac_abi::BAD_ARGUMENT))?;
    let mut call = mac_abi::traps::LogCall { bytes:  text.bytes.as_ptr(),
                                             length: text.length };
    let mut result = core::ptr::from_mut(&mut call).addr();
    // SAFETY: synchronous production LOG borrows live, bounded stack storage.
    unsafe {
        core::arch::asm!("ecall", in("a7") mac_abi::traps::LOG,
                        inlateout("a0") result, in("a1") core::mem::size_of_val(&call),
                        clobber_abi("C"));
    }
    if result as isize == 0 { Ok(()) } else { Err(Error(result as i32)) }
}

fn require(condition: bool, cause: &str) -> Result<()>
{
    if condition
    {
        return Ok(());
    }
    log(format_args!("SMP_WORKER_INVARIANT_FAIL cause={cause}\n"))?;
    Err(Error(mac_abi::BAD_ABI))
}

fn cancellation(context: &WorkerContext, host: Client, second: bool, phase: u64) -> Result<()>
{
    require(context.cancelled()?, "park reported cancellation without sticky state")?;
    require(context.park() == Err(Error(mac_abi::OPERATION_CANCELLED)),
            "cancelled worker parked instead of returning its typed cause")?;
    log(format_args!("SMP_WORKER_CANCEL_OBSERVED actor={} module={} phase={phase}\n",
                     u8::from(second),
                     host.module()))
}

fn run(context: WorkerContext, host: Client, second: bool) -> Result<()>
{
    if second
    {
        PEER_ENTERED.store(true, Ordering::Release);
    }
    let symbol = if second { "__smp_worker_b_body" } else { "__smp_worker_a_body" };
    let probe: body::Probe = if second { __smp_worker_b_body } else { __smp_worker_a_body };
    log(format_args!("SMP_WORKER_BEGIN actor={} module={} body={symbol} body_entry=0x{:x} \
                      hold_ticks={HOLD_TICKS} target_timebase=10000000\n",
                     u8::from(second),
                     host.module(),
                     probe as usize))?;
    for phase in 0..16u64
    {
        if context.cancelled()?
        {
            return cancellation(&context, host, second, phase);
        }
        let seed = host.module()
                   ^ (phase << 40)
                   ^ if second { 0x2345_6789_abcd_ef01 } else { 0xfedc_ba98_7654_3210 };
        // SAFETY: one adjacent assembly instance per actual worker, with one
        // live invocation. The assembly preserves its LP64 callee state.
        let vlenb = match unsafe { body::hold(probe, HOLD_TICKS, seed) }
        {
            Ok(value) => value,
            Err(failure) =>
            {
                log(format_args!("SMP_WORKER_REGISTER_FAIL actor={} phase={phase} \
                                  cause={failure}\n",
                                 u8::from(second)))?;
                return Err(Error(mac_abi::BAD_ABI));
            }
        };
        // Original module authority, real forwarding allocator and Host gateway.
        // No scope is retained across a park or assembly invocation.
        host.with_module_allocation(|| -> Result<()> {
                let mut bytes = Vec::new();
                bytes.try_reserve_exact(4096).map_err(|_| Error(mac_abi::NO_MEMORY))?;
                bytes.resize(4096, phase as u8);
                require(bytes.iter().all(|byte| *byte == phase as u8),
                        "module allocation content changed")
            })??;
        let shared = SHARED.with(|value| {
                               *value += 1;
                               *value
                           })?;
        let before = context.monotonic_microseconds()?;
        context.waker().wake()?;
        // A retained self wake must be consumed without waiting for this long
        // deadline; the next positive park uses the actual native timer path.
        match context.park_for(Duration::from_secs(60))
        {
            Ok(()) => (),
            Err(Error(mac_abi::OPERATION_CANCELLED)) =>
            {
                return cancellation(&context, host, second, phase);
            }
            Err(error) => return Err(error)
        }
        match context.park_for(Duration::from_millis(1))
        {
            Ok(()) => (),
            Err(Error(mac_abi::OPERATION_CANCELLED)) =>
            {
                return cancellation(&context, host, second, phase);
            }
            Err(error) => return Err(error)
        }
        require(context.monotonic_microseconds()? >= before, "worker monotonic clock regressed")?;
        log(format_args!("SMP_WORKER_PHASE_DONE actor={} module={} phase={phase} fv=preserved \
                          vlenb={vlenb} shared={shared} \
                          services=allocation-critical-wake-park\n",
                         u8::from(second),
                         host.module()))?;
        if !second && phase == 11
        {
            require(PEER_ENTERED.load(Ordering::Acquire), "peer never actually entered")?;
            CONTROLLERS.with(|controllers| {
                           controllers.as_ref().ok_or(Error(mac_abi::NOT_FOUND))?[1].cancel()
                       })??;
            log(format_args!("SMP_WORKER_CANCEL_SENT actor=0 module={}\n", host.module()))?;
            let deadline = context.monotonic_microseconds()?
                                  .checked_add(10_000_000)
                                  .ok_or(Error(mac_abi::BAD_ARGUMENT))?;
            loop
            {
                if let Some(result) =
                    CONTROLLERS.with(|controllers| {
                        controllers.as_ref().ok_or(Error(mac_abi::NOT_FOUND))?[1].try_join()
                    })??
                {
                    result?;
                    break;
                }
                require(context.monotonic_microseconds()? < deadline,
                        "peer did not finish after cancellation")?;
                match context.park_for(Duration::from_millis(1))
                {
                    Ok(()) => (),
                    Err(Error(mac_abi::OPERATION_CANCELLED)) =>
                    {
                        return cancellation(&context, host, second, phase);
                    }
                    Err(error) => return Err(error)
                }
            }
            log(format_args!("SMP_WORKER_JOIN_OBSERVED actor=1 module={} result=ok\n",
                             host.module()))?;
            log(format_args!("SMP_WORKER_BODY_DONE actor=0 module={} phases=12 \
                              placement=requires-host-evidence\n",
                             host.module()))?;
            // Dispose both issued controllers outside the SDK mutex. The real
            // current worker stack must remain pinned until its return/claim
            // release; the peer's completed result must remain generation-safe.
            let controllers = CONTROLLERS.with(Option::take)?;
            drop(controllers);
            return Ok(());
        }
    }
    // The peer must observe the genuine cancellation rather than silently
    // finishing its bounded workload before the coordinator could exercise it.
    require(false, "peer exhausted phases without observing cancellation")
}

impl Driver for crate::Device
{
    fn initialize(&self, host: Client) -> Result<()>
    {
        let a = host.prepare_worker("SMP worker A", move |context| run(context, host, false))
                    .map_err(|failure| failure.cause)?;
        let b = host.prepare_worker("SMP worker B", move |context| run(context, host, true))
                    .map_err(|failure| failure.cause)?;
        let a = a.start().map_err(|failure| failure.cause)?;
        let b = b.start().map_err(|failure| failure.cause)?;
        // Actual module initialization keeps these Armed until commit; neither
        // body may race this publication. Two real 4 MiB stacks fit its budget.
        CONTROLLERS.with(|controllers| *controllers = Some([a, b]))?;
        log(format_args!("SMP_WORKERS_ARMED module={} actors=2 affinity=automatic\n",
                         host.module()))
    }

    fn request(&self, _: Request<'_>, _: &mut [u8]) -> Result<Completion>
    {
        Err(Error(mac_abi::NOT_SUPPORTED))
    }

    fn quiesce(&self) -> Result<()>
    {
        let finished = CONTROLLERS.with(|controllers| -> Result<Option<[Worker; 2]>> {
                                      let Some(workers) = controllers.as_ref()
                                      else
                                      {
                                          return Ok(None);
                                      };
                                      for worker in workers
                                      {
                                          worker.cancel()?;
                                      }
                                      for worker in workers
                                      {
                                          match worker.try_join()?
                                          {
                                              None => return Err(Error(mac_abi::SERVICE_BUSY)),
                                              Some(result) => result?
                                          }
                                      }
                                      Ok(controllers.take())
                                  })??;
        // Disposal, unlike inspecting a token, may release real owned backing.
        // Never drop controllers while the fixture mutex/critical guard is held.
        drop(finished);
        Ok(())
    }
}

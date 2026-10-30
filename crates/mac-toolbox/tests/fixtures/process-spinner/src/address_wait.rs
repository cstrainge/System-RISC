//! Finite native wait-path exercise. Real scheduler deadlines and real backing
//! are used; this does not establish concurrent child wake/timeout correctness.
use alloc::boxed::Box;
use core::{sync::atomic::{AtomicU32, Ordering},
           time::Duration};

use mac_toolbox::{System, ToolboxResult, abi::address_wait as wire, address_wait::WaitOutcome};

static INITIALIZED: AtomicU32 = AtomicU32::new(7);
static ZEROED: AtomicU32 = AtomicU32::new(0);
static READ_ONLY: u32 = 0x1234_5678;

fn malformed(address: usize, flags: u32, seconds: u64, expected: i32)
{
    let mut call = wire::Call { address,
                                flags,
                                seconds,
                                ..Default::default() };
    let mut result = core::ptr::from_mut(&mut call).addr();
    // SAFETY: this deliberately malformed *numeric* metadata is passed to the
    // actual trap, never made into an invalid Rust AtomicU32 reference. The
    // request itself is live/exclusive for the complete invocation.
    unsafe {
        core::arch::asm!("ecall", inlateout("a0") result,
            in("a1") core::mem::size_of::<wire::Call>(),
            in("a7") mac_toolbox::abi::traps::ADDRESS_WAIT, options(nostack));
    }
    assert_eq!(result as isize, expected as isize, "malformed native wait status");
    assert!(call.detail_length > 0 && call.detail_length as usize <= call.detail.len(),
            "native wait refusal retains bounded diagnostic");
    assert_eq!(call.outcome, 0, "refusal cannot claim a completed wait");
    assert_eq!(call.awakened, 0, "refusal cannot claim a wake");
}

pub fn run(system: &System<'static>) -> ToolboxResult<()>
{
    let client = system.client();
    let stack = AtomicU32::new(19);
    let heap = Box::new(AtomicU32::new(23));
    for atomic in [&INITIALIZED, &ZEROED, &stack, heap.as_ref()]
    {
        let value = atomic.load(Ordering::Relaxed);
        assert_eq!(client.wait_u32(atomic, value.wrapping_add(1), None).map_err(|e| e.cause())?,
                   WaitOutcome::ValueChanged);
        assert_eq!(client.wait_u32(atomic, value, Some(Duration::ZERO)).map_err(|e| e.cause())?,
                   WaitOutcome::TimedOut);
        assert_eq!(client.wait_u32(atomic, value, Some(Duration::from_millis(2)))
                         .map_err(|e| e.cause())?,
                   WaitOutcome::TimedOut);
        assert!(!client.wake_u32(atomic).map_err(|e| e.cause())?);
        assert_eq!(client.wake_all_u32(atomic).map_err(|e| e.cause())?, 0);
    }
    for address in [0, 1, usize::MAX - 1]
    {
        malformed(address, 0, 0, mac_toolbox::abi::BAD_ARGUMENT);
    }
    malformed(core::ptr::addr_of!(READ_ONLY).addr(), 0, 0, mac_toolbox::abi::ACCESS_DENIED);
    malformed(stack.as_ptr().addr(), u32::MAX, 0, mac_toolbox::abi::BAD_ARGUMENT);
    malformed(stack.as_ptr().addr(), wire::FINITE, u64::MAX, mac_toolbox::abi::BAD_ARGUMENT);
    assert_eq!(client.wait_u32(&stack, 20, None).map_err(|e| e.cause())?,
               WaitOutcome::ValueChanged,
               "refusals leave following wait usable");
    drop(heap);
    system.log("ADDRESS_WAIT_PROBE stack=true heap=true data=true bss=true deadlines=true \
                refusals=true reuse=true\n")?;
    Ok(())
}

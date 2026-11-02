//! Native register workload. Only external PC/SP observations can establish
//! actual CPU placement or migration; these completion logs cannot.
use std::{sync::atomic::{AtomicU64, Ordering},
          thread};

use mac_toolbox::System;

#[path = "../../../riscv_smp_body.rs"]
mod registers;

core::arch::global_asm!(include_str!("../../../riscv_smp_body.S"),
                        "SMP_BODY __smp_std_a_body, __smp_std_a_progress",
                        "SMP_BODY __smp_std_b_body, __smp_std_b_progress",
                        "SMP_BODY __smp_std_c_body, __smp_std_c_progress",
                        "SMP_BODY __smp_std_d_body, __smp_std_d_progress");

#[repr(C, align(64))]
struct Progress(AtomicU64);

#[unsafe(no_mangle)]
static __smp_std_a_progress: Progress = Progress(AtomicU64::new(0));
#[unsafe(no_mangle)]
static __smp_std_b_progress: Progress = Progress(AtomicU64::new(0));
#[unsafe(no_mangle)]
static __smp_std_c_progress: Progress = Progress(AtomicU64::new(0));
#[unsafe(no_mangle)]
static __smp_std_d_progress: Progress = Progress(AtomicU64::new(0));

unsafe extern "C" {
    fn __smp_std_a_body(ticks: u64, seed: u64, traps: usize, result: *mut registers::Capture);
    fn __smp_std_b_body(ticks: u64, seed: u64, traps: usize, result: *mut registers::Capture);
    fn __smp_std_c_body(ticks: u64, seed: u64, traps: usize, result: *mut registers::Capture);
    fn __smp_std_d_body(ticks: u64, seed: u64, traps: usize, result: *mut registers::Capture);
}

// Exact QEMU virt timebase used by the existing native register workload. This
// diagnostic constant is neither a portable clock API nor a measured latency.
const HOLD_TICKS: u64 = 20_000_000;
const ROUNDS: u64 = 24;

pub(super) fn run(actor: usize, thread_id: u64)
{
    let bodies: [registers::Probe; 4] =
        [__smp_std_a_body, __smp_std_b_body, __smp_std_c_body, __smp_std_d_body];
    let names = ["__smp_std_a_body", "__smp_std_b_body", "__smp_std_c_body", "__smp_std_d_body"];
    let progress = [&__smp_std_a_progress, &__smp_std_b_progress, &__smp_std_c_progress,
                    &__smp_std_d_progress][actor];
    let system = System::new().unwrap();
    let body = bodies[actor];
    system.log(&format!("STD_RESIDENCY_BEGIN actor={actor} thread={thread_id} body={} \
                         body_entry={:#x} rounds={ROUNDS} hold_ticks={HOLD_TICKS} \
                         target_timebase=10000000\n",
                        names[actor], body as usize))
          .unwrap();
    let mut vlenb = 0;
    for round in 0..ROUNDS
    {
        let seed = 0x1234_5600 | ((actor as u64 + 1) << 4) | round;
        // SAFETY: each entry is the adjacent macro with its exact Capture ABI.
        vlenb = unsafe { registers::hold(body, HOLD_TICKS, seed) }.unwrap();
        assert!(vlenb >= 16);
        assert_eq!(system.client().current_native_thread().unwrap().0, thread_id);
        let mut bytes = vec![actor as u8 + 1; 16_384];
        bytes.resize(32_768, round as u8);
        assert!(std::hint::black_box(&bytes[..16_384]).iter().all(|&b| b == actor as u8 + 1));
        assert!(std::hint::black_box(&bytes[16_384..]).iter().all(|&b| b == round as u8));
        drop(bytes);
        thread::yield_now();
    }
    assert_eq!(progress.0.load(Ordering::Acquire), ROUNDS * 2);
    system.log(&format!("STD_RESIDENCY_END actor={actor} thread={thread_id} rounds={ROUNDS} \
                         vlenb={vlenb} registers=true heap=true\n"))
          .unwrap();
}

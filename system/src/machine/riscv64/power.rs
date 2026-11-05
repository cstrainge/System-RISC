//! Terminal-only board failure/restart, independent of ordinary power drivers.
//! Provenance: QEMU v10.1.0 hw/riscv/virt.c VIRT_TEST and
//! include/hw/misc/sifive_test.h / hw/misc/sifive_test.c FINISHER_FAIL/RESET;
//! the normal external qemu-power-device uses the same documented reset command.
//! Values/addresses come from the checked platform record. No callbacks, ROM
//! logger, scheduler context, heap or locks are consulted. These terminal paths
//! abandon execution; they do not claim M-D04 privileged-fault recovery.
pub(super) mod platform
{
    include!(concat!(env!("OUT_DIR"), "/platform.rs"));
}
pub(crate) fn fatal_shutdown() -> !
{
    // Terminal execution cannot return to an interrupted ordinary service.
    unsafe {
        core::arch::asm!("csrci sstatus, 2", options(nostack));
    }
    super::emergency::serial(format_args!("SYSTEM_SHUTDOWN: fatal privileged fault; recovery is \
                                           not proven\n"));
    // SAFETY: the current platform is QEMU virt in S mode. Mask local
    // interrupts and request its mapped finisher's guest-panic shutdown/code 1.
    // No heap, driver registry or borrowed desktop service is involved.
    unsafe {
        core::ptr::write_volatile(platform::FATAL_FINISHER_ADDRESS as *mut u32,
                                  platform::FATAL_FINISHER_FAILURE);
        core::arch::asm!("fence iorw, iorw", options(nostack));
    }
    // The emulator completes shutdown asynchronously. This is quiescence after
    // an explicit, reported shutdown request, never an author-owned silent spin.
    loop
    {
        unsafe {
            core::arch::asm!("wfi", options(nostack));
        }
    }
}

/// Request a platform reset only after the emergency terminal has abandoned all
/// ordinary execution. Graceful application restart/shutdown continues through
/// the normal driver-selected System power service.
pub(crate) fn emergency_restart() -> !
{
    unsafe {
        core::arch::asm!("csrci sstatus, 2", options(nostack));
    }
    super::emergency::serial(format_args!("SYSTEM_RESTART: emergency terminal requested reset\n"));
    // SAFETY: terminal admission retains the mapped checked board register and
    // execution never resumes. Mask interrupts before submitting its reset value.
    unsafe {
        core::ptr::write_volatile(platform::FATAL_FINISHER_ADDRESS as *mut u32,
                                  platform::FATAL_FINISHER_RESTART);
        core::arch::asm!("fence iorw, iorw", options(nostack));
    }
    loop
    {
        unsafe {
            core::arch::asm!("wfi", options(nostack));
        }
    }
}

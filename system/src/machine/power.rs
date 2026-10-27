//! QEMU virt board fatal shutdown. Hardware facts stay in this machine adapter.
//! Provenance: QEMU v10.1.0 hw/riscv/virt.c VIRT_TEST and
//! include/hw/misc/sifive_test.h / hw/misc/sifive_test.c FINISHER_FAIL.
pub(super) mod platform
{
    include!(concat!(env!("OUT_DIR"), "/platform.rs"));
}
pub(crate) fn fatal_shutdown() -> !
{
    serial!("SYSTEM_SHUTDOWN: fatal privileged fault; recovery is not proven");
    // SAFETY: the current platform is QEMU virt in S mode. Mask local
    // interrupts and request its mapped finisher's guest-panic shutdown/code 1.
    // No heap, driver registry or borrowed desktop service is involved.
    unsafe {
        core::arch::asm!("csrci sstatus, 2", options(nostack));
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

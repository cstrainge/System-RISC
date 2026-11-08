//! The machine's interrupt controller: a GICv2 distributor and CPU interface.
//!
//! This is machine bring-up, not a driver. A tick has to arrive before any
//! module can load, and nothing here is bound to a device the way a driver is:
//! the controller is part of the CPU's interrupt path. It therefore lives under
//! `machine/` rather than in `drivers/`, and the only thing it takes from the
//! platform is the two register blocks, which come from the machine's own tree.
//!
//! What this module does is small and deliberate: it enables the distributor and
//! the CPU interface, puts the interrupt sources in the group a non-secure kernel
//! can receive, gives the timer a priority and a target, and enables that one
//! source. It then answers the two questions the trap path asks - which source is
//! pending, and what that source means - and completes it.
//!
//! Two limits are stated rather than hidden. The kernel root maps every bootstrap
//! window with the normal-memory attribute, because the page model carries no
//! device attribute; QEMU does not model that, and on silicon these windows need
//! `Device-nGnRnE`. And only GICv2 is driven: a machine that declares a GICv3 is
//! refused by name rather than programmed through a register map it does not have.

use crate::machine::traps::Interrupt;

/// Offsets inside the distributor's register block.
const GICD_CTLR: u64 = 0x000;
const GICD_TYPER: u64 = 0x004;
const GICD_IGROUPR0: u64 = 0x080;
const GICD_ISENABLER0: u64 = 0x100;
const GICD_IPRIORITYR: u64 = 0x400;
const GICD_ITARGETSR: u64 = 0x800;

/// Offsets inside the CPU interface's register block.
const GICC_CTLR: u64 = 0x000;
const GICC_PMR: u64 = 0x004;
const GICC_IAR: u64 = 0x00c;
const GICC_EOIR: u64 = 0x010;

/// `GICD_TYPER.SECURITYEXTN`: the controller implements the security
/// extensions, so this kernel - which the bootloader enters at non-secure EL1 -
/// programs group 1 and reads the non-secure view of the enable registers.
const TYPER_SECURITY: u32 = 1 << 10;
/// `GICD_TYPER.ITLinesNumber`: the number of 32-interrupt banks, minus one.
const TYPER_LINES: u32 = 0x1f;

/// `GICC_IAR` carries the interrupt number in bits 9:0 and the source CPU in
/// bits 12:10, so the number has to be masked out of the word.
const IAR_NUMBER: u32 = 0x3ff;

/// Interrupts 0..15 are software-generated, which is the only way one hart
/// signals another.
pub(crate) const SGI_COUNT: u32 = 16;
/// The number the acknowledge register reports when nothing is pending.
pub(crate) const SPURIOUS: u32 = 1023;

/// The priority every interrupt this kernel enables is given. Any value below
/// the CPU interface's threshold is delivered; this is the same default Linux
/// gives a PPI.
const PRIORITY: u8 = 0xa0;
/// The CPU interface's priority threshold: interrupts with a higher priority
/// value than this are held back.
const THRESHOLD: u8 = 0xf0;
/// The target byte for a bank interrupt: CPU 0 of the distributor's own domain.
/// A bank whose target byte is zero is never forwarded to anyone, which is why
/// this is written rather than left at reset.
const TARGET_PRIMARY: u8 = 0x01;

/// The controller this hart's interrupt path runs through, once it has been
/// admitted. Read on the trap path, so it is a plain atomic rather than
/// anything that can allocate.
struct Admitted
{
    distributor:   u64,
    cpu_interface: u64,
    /// The interrupt the machine states for the non-secure physical timer.
    timer:         u32
}
static ADMITTED: spin::Once<Admitted> = spin::Once::new();

fn read(address: u64) -> u32
{
    // SAFETY: the address was resolved from the machine's tree and the kernel
    // root maps the window containing it. A volatile read creates no reference.
    unsafe { core::ptr::read_volatile(address as *const u32) }
}

fn write(address: u64, value: u32)
{
    // SAFETY: as `read`; the register is word-wide and the window is mapped
    // writable.
    unsafe { core::ptr::write_volatile(address as *mut u32, value) };
}

fn write_byte(address: u64, value: u8)
{
    // SAFETY: as `read`; the priority and target registers are byte-wide.
    unsafe { core::ptr::write_volatile(address as *mut u8, value) };
}

/// Enables the controller and the one source this boot needs.
///
/// The order is the architecture's, not a preference: the distributor is
/// configured while it is disabled, the interface is enabled before any source
/// can be forwarded to it, and the source itself is enabled last.
pub(crate) fn admit(distributor: u64, cpu_interface: u64, timer: u32)
{
    write(distributor + GICD_CTLR, 0);
    // Every interrupt in group 1. A non-secure kernel can only be delivered
    // group 1, and a controller without the security extensions ignores the
    // register, so this is the same write on both machines.
    write(distributor + GICD_IGROUPR0, !0);
    write_byte(distributor + GICD_IPRIORITYR + u64::from(timer), PRIORITY);
    write_byte(distributor + GICD_ITARGETSR + u64::from(timer), TARGET_PRIMARY);
    write(distributor + GICD_CTLR, 1);
    write(cpu_interface + GICC_PMR, u32::from(THRESHOLD));
    write(cpu_interface + GICC_CTLR, 1);
    // Only now can the timer reach the interface that is running.
    write(distributor + GICD_ISENABLER0, 1 << timer);
    ADMITTED.call_once(|| Admitted { distributor,
                                     cpu_interface,
                                     timer });
    // The configuration has to be visible before an `eret` can take an
    // interrupt, and the stores above cross a device window.
    crate::machine::cpu::barrier();
    let typer = read(distributor + GICD_TYPER);
    crate::machine::emergency::serial(format_args!("INTERRUPTS_READY: controller=gic-v2 \
                                                    distributor={distributor:#x} \
                                                    cpu_interface={cpu_interface:#x} timer={} \
                                                    security={} lines={}\n",
                                                   timer,
                                                   u32::from(typer & TYPER_SECURITY != 0),
                                                   (typer & TYPER_LINES) + 1));
}

/// Makes the inter-hart signalling source deliverable.
///
/// Nothing on this machine posts one: the resident scheduler is single-hart and
/// the monitor refuses a controller route. A hart that has been told it may be
/// woken still has to have the source enabled, or the wake it is waiting for
/// cannot arrive, so this enables the software-generated bank rather than
/// leaving a promise the hardware cannot keep.
pub(crate) fn admit_wake_source()
{
    let Some(admitted) = ADMITTED.get()
    else
    {
        crate::machine::emergency::serial(format_args!("SYSTEM_WAKE_SOURCE_REFUSED: no \
                                                        interrupt controller is admitted, so no \
                                                        source can be enabled\n"));
        return;
    };
    write(admitted.distributor + GICD_ISENABLER0, (1 << SGI_COUNT) - 1);
}

/// Completes the interrupt the controller reports and says what it was.
///
/// The acknowledgement is read before anything else is decided, because that
/// word is the only statement of which source fired: an AArch64 IRQ writes no
/// cause register, so a dispatcher that did not ask would have to guess.
///
/// A level-sensitive source is cleared before it is completed. Completing it
/// first would let the still-asserted level be delivered again before the
/// scheduler has re-armed the timer, which is a storm rather than a tick.
pub(crate) fn service() -> Interrupt
{
    let Some(admitted) = ADMITTED.get()
    else
    {
        // An interrupt from a controller this kernel never enabled is not a
        // tick and not a fault. There is no interface to acknowledge through,
        // so there is nothing to complete either.
        return Interrupt::Unhandled { number: SPURIOUS };
    };
    let number = read(admitted.cpu_interface + GICC_IAR) & IAR_NUMBER;
    if number >= SPURIOUS
    {
        return Interrupt::Unhandled { number: SPURIOUS };
    }
    if number == admitted.timer
    {
        crate::machine::cpu::disable_timer_interrupt();
    }
    write(admitted.cpu_interface + GICC_EOIR, number);
    if number == admitted.timer
    {
        Interrupt::Preempt
    }
    else if number < SGI_COUNT
    {
        Interrupt::Wake
    }
    else
    {
        Interrupt::Unhandled { number }
    }
}

//! The BCM2836 local interrupt controller: the per-core half of a Raspberry Pi's
//! interrupt path.
//!
//! The board has no GIC. Each core has a small bank of level-sensitive sources
//! of its own - the four generic-timer interrupts, four mailboxes, and one line
//! from the peripheral controller the VideoCore's devices share - and three
//! registers per core say which are routed, which are pending, and what the
//! mailboxes hold. There is no priority, no acknowledge and no end-of-interrupt:
//! a source is pending for as long as its device asserts it, so servicing one
//! means quieting the device.
//!
//! This is the same machine bring-up the GIC module does, for the same reason
//! and with the same small scope: route the timer the scheduler arms, make the
//! inter-core source deliverable, and tell the trap path what fired.

use crate::machine::traps::Interrupt;

/// Core timer interrupt control, one word per core: bits 0-3 route the
/// secure-physical, non-secure-physical, hypervisor and virtual timers as IRQ,
/// and bits 4-7 route the same four as FIQ.
const TIMER_CONTROL: u64 = 0x40;
/// Core mailbox interrupt control, one word per core: bits 0-3 route the core's
/// four mailboxes as IRQ.
const MAILBOX_CONTROL: u64 = 0x50;
/// Core IRQ source, one word per core: bits 0-3 the timers, 4-7 the mailboxes,
/// 8 the peripheral controller, 9 the performance monitor.
const IRQ_SOURCE: u64 = 0x60;
/// Core mailbox read and write-high-to-clear, four words per core.
const MAILBOX_CLEAR: u64 = 0xc0;
/// Performance-monitor routing, write-one-to-clear.
const PMU_ROUTING_CLEAR: u64 = 0x14;

/// The peripheral controller's three disable registers, as offsets from the
/// window its node states. Writing ones masks the sources; zeros do nothing.
const PERIPHERAL_DISABLE: [u64; 3] = [0x1c, 0x20, 0x24];

/// The peripheral controller's pending, enable and disable registers, indexed
/// by bank: 0 is the eight ARM-side sources, 1 and 2 the VideoCore's sixty-four.
/// A device's tree specifier is a bank and a bit, and its line here is
/// `bank * 32 + bit`.
const PERIPHERAL_PENDING: [u64; 3] = [0x00, 0x04, 0x08];
const PERIPHERAL_ENABLE: [u64; 3] = [0x18, 0x10, 0x14];
const PERIPHERAL_DISABLE_BANK: [u64; 3] = [0x24, 0x1c, 0x20];
/// Bank 0's pending word carries summary and shortcut bits above its own
/// eight sources; only these are lines.
const BASIC_SOURCES: u32 = 0xff;
/// The core's source bit for the peripheral controller's output.
const SOURCE_PERIPHERAL: u32 = 1 << 8;

const SOURCE_MAILBOXES: u32 = 0xf << 4;
/// The mailbox this kernel uses as its inter-core source.
const WAKE_MAILBOX: u32 = 0;
/// Reported when the source word is empty, matching the GIC module's value for
/// the same condition so a reader of the log sees one number for "nothing".
const SPURIOUS: u32 = 1023;

struct Admitted
{
    base:       u64,
    /// The source bit of the timer the scheduler arms.
    timer:      u32,
    /// The peripheral controller's window, when the machine describes one.
    peripheral: Option<u64>
}
static ADMITTED: spin::Once<Admitted> = spin::Once::new();

fn read(address: u64) -> u32
{
    // SAFETY: the address was resolved from the machine's tree and checked
    // against the window the kernel root maps as device memory. A volatile
    // read creates no reference.
    unsafe { core::ptr::read_volatile(address as *const u32) }
}

fn write(address: u64, value: u32)
{
    // SAFETY: as `read`; every register here is word-wide.
    unsafe { core::ptr::write_volatile(address as *mut u32, value) };
}

/// The core these per-core registers are indexed by.
fn core() -> u64
{
    (crate::machine::cpu::hart_id() & 3) as u64
}

pub(crate) fn admitted() -> bool
{
    ADMITTED.get().is_some()
}

/// Routes the scheduler's timer to this core and nothing else.
///
/// `peripheral` is the VideoCore peripheral controller's window when the
/// machine describes one. Its output is a level on this core's source word, the
/// firmware does not promise to hand it over quiet, and nothing here services
/// it yet - so every source in it is masked first. An asserted line nobody
/// handles is not a missed interrupt on this controller, it is a storm.
pub(crate) fn admit(base: u64, timer: u32, peripheral: Option<u64>)
{
    let core = core();
    if let Some(peripheral) = peripheral
    {
        for offset in PERIPHERAL_DISABLE
        {
            write(peripheral + offset, !0);
        }
    }
    write(base + PMU_ROUTING_CLEAR, 0xf);
    // The timer as IRQ; every FIQ route and every other timer off.
    write(base + TIMER_CONTROL + 4 * core, 1 << timer);
    write(base + MAILBOX_CONTROL + 4 * core, 0);
    ADMITTED.call_once(|| Admitted { base,
                                     timer,
                                     peripheral });
    crate::machine::cpu::barrier();
    // What the machine reports after the writes, for the reason the GIC module
    // gives: a register that did not take looks exactly like a source that
    // never fired.
    let control = read(base + TIMER_CONTROL + 4 * core);
    let pending = read(base + IRQ_SOURCE + 4 * core);
    crate::machine::emergency::serial(format_args!("INTERRUPTS_READY: controller=bcm2836-l1 \
                                                    base={base:#x} core={core} timer={timer} \
                                                    timer_control={control:#x} \
                                                    pending={pending:#x} peripheral_masked={}\n",
                                                   u32::from(peripheral.is_some())));
}

/// Makes the inter-core source deliverable: this core's wake mailbox as IRQ.
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
    write(admitted.base + MAILBOX_CONTROL + 4 * core(), 1 << WAKE_MAILBOX);
}

/// Says what fired and quiets it.
///
/// The timer is quieted at the timer, which is the only place its level can be
/// dropped; a mailbox is quieted by clearing the bits it holds. Anything else
/// is reported by its source bit. One source is serviced per entry: a second
/// that is still asserted is delivered again as soon as IRQs are unmasked.
pub(crate) fn service() -> Interrupt
{
    let Some(admitted) = ADMITTED.get()
    else
    {
        return Interrupt::Unhandled { number: SPURIOUS };
    };
    let core = core();
    let pending = read(admitted.base + IRQ_SOURCE + 4 * core);
    if pending & (1 << admitted.timer) != 0
    {
        crate::machine::cpu::disable_timer_interrupt();
        return Interrupt::Preempt;
    }
    if pending & SOURCE_MAILBOXES != 0
    {
        for mailbox in 0..4u64
        {
            if pending & (1 << (4 + mailbox)) != 0
            {
                let cell = admitted.base + MAILBOX_CLEAR + 16 * core + 4 * mailbox;
                write(cell, read(cell));
            }
        }
        return Interrupt::Wake;
    }
    if pending & SOURCE_PERIPHERAL != 0
       && let Some(peripheral) = admitted.peripheral
    {
        // One line per entry, lowest bank first. It is masked here, which is
        // the only completion this controller has: its output drops when no
        // enabled source is asserted.
        for bank in 0..3u32
        {
            let mut asserted = read(peripheral + PERIPHERAL_PENDING[bank as usize]);
            if bank == 0
            {
                asserted &= BASIC_SOURCES;
            }
            if asserted != 0
            {
                let bit = asserted.trailing_zeros();
                write(peripheral + PERIPHERAL_DISABLE_BANK[bank as usize], 1 << bit);
                return Interrupt::Device { line: bank * 32 + bit };
            }
        }
        // The summary bit with nothing behind it: a source that dropped
        // between the two reads.
        return Interrupt::Unhandled { number: SPURIOUS };
    }
    if pending == 0
    {
        return Interrupt::Unhandled { number: SPURIOUS };
    }
    Interrupt::Unhandled { number: pending.trailing_zeros() }
}

/// A line's bank and bit, when it names a source this controller has.
fn peripheral_line(line: u32) -> Option<(u64, usize, u32)>
{
    let peripheral = ADMITTED.get()?.peripheral?;
    let (bank, bit) = ((line / 32) as usize, line % 32);
    if bank >= 3 || bank == 0 && bit >= 8
    {
        return None;
    }
    Some((peripheral, bank, bit))
}

/// Enables a device's line at the peripheral controller. Its output already
/// reaches core 0 as an IRQ, which is the reset routing and the only core this
/// kernel runs devices on.
pub(crate) fn enable_line(line: u32) -> bool
{
    let Some((peripheral, bank, bit)) = peripheral_line(line)
    else
    {
        return false;
    };
    write(peripheral + PERIPHERAL_ENABLE[bank], 1 << bit);
    true
}

pub(crate) fn mask_line(line: u32)
{
    if let Some((peripheral, bank, bit)) = peripheral_line(line)
    {
        write(peripheral + PERIPHERAL_DISABLE_BANK[bank], 1 << bit);
    }
}

pub(crate) fn unmask_line(line: u32)
{
    if let Some((peripheral, bank, bit)) = peripheral_line(line)
    {
        write(peripheral + PERIPHERAL_ENABLE[bank], 1 << bit);
    }
}

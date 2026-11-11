//! ACT LED blink diagnostic for the Pi bring-up.
//!
//! The green ACT LED is GPIO 47 on the main GPIO controller, active-low:
//! driving the pin low turns it on, driving it high turns it off. The
//! bootloader blinks it once per stage it passes, so a board with no serial
//! cable still shows how far the boot got: the count of blinks is the last
//! stage reached, and the halt is the next one.
//!
//! The GPIO base comes from the platform record through build.rs, never
//! hardcoded. A platform that declares no GPIO controller gets a base of
//! zero, and every function here is a no-op then, so the calls are harmless
//! on a machine without this block (QEMU's virt platform).

use crate::GPIO_BASE;

/// Off: on the Pi 3 Model B the ACT LED is not GPIO 47. The board's own device
/// tree puts ACT on the firmware's virtual GPIO (`virtgpio` line 0, reached only
/// through the mailbox) and labels GPIO 47 `SMPS_SDA`, the power supply chip's
/// I2C data line. Driving it blinks nothing, pokes that bus, and each "blink"
/// stalls the boot for the length of its busy-wait. Observed 2026-11-10 over
/// serial: the `B` at `_start` printed and nothing followed. Kept as a switch,
/// not deleted, until the LED is driven through the mailbox or removed outright.
const ENABLED: bool = false;
/// The green ACT LED on the Pi 3.
const ACT_PIN: u32 = 47;
/// Bit of GPIO 47 in the second bank: GPSET1/GPCLR1 cover pins 32..63.
const ACT_BIT: u32 = 1u32 << (ACT_PIN - 32);
/// GPFSEL4 register offset; GPIO 47's function select is bits 21..23.
const GPFSEL4: usize = 0x10;
/// GPSET1 register offset: writing a bit drives that pin high.
const GPSET1: usize = 0x20;
/// GPCLR1 register offset: writing a bit drives that pin low.
const GPCLR1: usize = 0x2c;
/// Function-select value for a pin configured as an output.
const OUTPUT: u32 = 0b001;
/// Busy-wait iterations for the LED-on half of a blink. The per-iteration
/// cost is not measured on hardware, so the count is set with a large
/// margin: 600M iterations clears 500 ms even at 1 cycle per iteration on
/// the Pi 3's ~1.2 GHz core, and is ~2 s at 4 cycles per iteration. The
/// v12 count (150M) may have produced a blink too brief to tell from the
/// firmware's SD-activity flicker, so v16 makes it unmistakable.
const ON_DELAY_ITERATIONS: u32 = 600_000_000;
/// Busy-wait iterations for the LED-off gap between blinks. About 200 ms;
/// short, so the pulses stay distinct and countable.
const OFF_DELAY_ITERATIONS: u32 = 40_000_000;

fn read(offset: usize) -> u32
{
    // SAFETY: the address is the platform's GPIO block, declared in its
    // record; the caller only reads the function-select register.
    unsafe { core::ptr::read_volatile((GPIO_BASE as usize + offset) as *const u32) }
}

fn write(offset: usize, value: u32)
{
    // SAFETY: the address is the platform's GPIO block, declared in its
    // record; the caller only writes the set/clear registers, which act on
    // one pin each.
    unsafe { core::ptr::write_volatile((GPIO_BASE as usize + offset) as *mut u32, value) }
}

fn delay(mut remaining: u32)
{
    while remaining != 0
    {
        core::hint::spin_loop();
        remaining -= 1;
    }
}

/// Configure GPIO 47 as an output and turn the LED off.
///
/// Safe to call before the stack is set up: it is a leaf that touches no
/// stack, only the GPIO registers. It never halts the boot — on a platform
/// with no GPIO block it does nothing.
#[unsafe(no_mangle)]
pub extern "C" fn led_init()
{
    if GPIO_BASE == 0 || !ENABLED
    {
        return;
    }
    // GPFSEL4 bits 21..23 select GPIO 47's function; 001 is output.
    write(GPFSEL4, (read(GPFSEL4) & !(0b111u32 << 21)) | (OUTPUT << 21));
    // Active-low: driving the pin high turns the LED off.
    write(GPSET1, ACT_BIT);
}

/// One blink: LED on for about 750 ms, then off for about 200 ms. The long
/// on-hold is what marks it as ours, not the firmware's SD flicker.
#[unsafe(no_mangle)]
pub extern "C" fn led_blink()
{
    if GPIO_BASE == 0 || !ENABLED
    {
        return;
    }
    // Active-low: driving the pin low turns the LED on.
    write(GPCLR1, ACT_BIT);
    delay(ON_DELAY_ITERATIONS);
    write(GPSET1, ACT_BIT);
    delay(OFF_DELAY_ITERATIONS);
}

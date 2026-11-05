//! The PL011 console. It carries the same result lines the screen shows, so a
//! QEMU run leaves a file that can be read without a photograph.
use core::ptr::{read_volatile, write_volatile};

use crate::platform::PL011_BASE;

const DR: usize = 0x00;
const FR: usize = 0x18;
const FR_TX_FULL: u32 = 1 << 5;

pub fn putc(byte: u8)
{
    // SAFETY: PL011_BASE is the window the platform record declares and the
    // mapping is Device both before and after the MMU is enabled.
    unsafe {
        while read_volatile((PL011_BASE + FR) as *const u32) & FR_TX_FULL != 0
        {}
        write_volatile((PL011_BASE + DR) as *mut u32, u32::from(byte));
    }
}

pub fn puts(text: &str)
{
    for byte in text.bytes()
    {
        putc(byte);
    }
}

pub fn putdec(mut value: u64)
{
    let mut digits = [0u8; 20];
    let mut at = digits.len();
    if value == 0
    {
        putc(b'0');
        return;
    }
    while value != 0
    {
        at -= 1;
        digits[at] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    for digit in &digits[at..]
    {
        putc(*digit);
    }
}

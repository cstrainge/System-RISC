//! The mailbox register block: one narrow MMIO boundary.
//!
//! The block is not in the BCM2835 datasheet (its §1.1 omits GPU-controlled
//! peripherals). The layout is the one every working implementation uses and is
//! recorded in `docs/VIDEOCORE-2D-DESIGN.md` §4.1. The base address comes from
//! the platform record's `bcm2835-mailbox` window, never from a constant here:
//! the inventory claims the 4 KiB page and the device tree declares the 0x40
//! byte register window inside it.
#![allow(unsafe_code)]
use core::{ptr::{read_volatile, write_volatile},
           sync::atomic::{Ordering, fence}};

use crate::{Channel, Error, PROPERTY_CHANNEL};

pub const READ: usize = 0x00;
pub const PEEK: usize = 0x10;
pub const SENDER: usize = 0x14;
pub const STATUS: usize = 0x18;
pub const CONFIG: usize = 0x1c;
pub const WRITE: usize = 0x20;
/// Declared window size, from the platform record's hardware device entry.
pub const WINDOW_BYTES: usize = 0x40;
/// Write FIFO has no room.
pub const STATUS_FULL: u32 = 1 << 31;
/// Response FIFO is empty.
pub const STATUS_EMPTY: u32 = 1 << 30;

/// Bounded polls. Most requests are answered in microseconds, but not all:
/// seen on a board, setting a 1920x1080 mode and powering the USB controller
/// both took longer than the hundred thousand polls this used to allow, and
/// each was reported as a mailbox that never answered while the firmware was
/// still working. The bound is a refusal threshold for a mailbox that is
/// dead, so it is set well past the slowest honest answer - seconds, not
/// milliseconds. It is a count and not a time because this crate has no
/// clock; a caller that has one should bound the exchange itself.
const POLL_LIMIT: u32 = 20_000_000;

/// The word written to [`WRITE`]: the message's 16-byte aligned bus address in
/// bits 31:4 and the channel in bits 3:0.
pub const fn doorbell(address: u32) -> u32
{
    (address & !0xf) | PROPERTY_CHANNEL
}

/// The mailbox register block at a mapped base.
pub struct Mailbox
{
    base: usize
}
impl Mailbox
{
    /// # Safety
    /// `base` is the mapped start of the mailbox register window declared by
    /// the platform record (at least [`WINDOW_BYTES`], [`STATUS`] and [`WRITE`]
    /// inside it). The caller guarantees this value has exclusive use of the
    /// block for its lifetime and that the mapping stays valid; two values over
    /// the same window would interleave FIFO reads and lose responses.
    pub const unsafe fn new(base: usize) -> Self
    {
        Self { base }
    }

    pub const fn base(&self) -> usize
    {
        self.base
    }

    fn status(&self) -> u32
    {
        // SAFETY: the window is mapped and owned by this value (see `new`).
        unsafe { read_volatile((self.base + STATUS) as *const u32) }
    }

    fn pop(&self) -> u32
    {
        // SAFETY: the window is mapped and owned by this value; the caller only
        // pops when the status register reports a word waiting.
        unsafe { read_volatile((self.base + READ) as *const u32) }
    }

    fn push(&self, word: u32)
    {
        // SAFETY: the window is mapped and owned by this value; the caller only
        // pushes when the status register reports room.
        unsafe {
            write_volatile((self.base + WRITE) as *mut u32, word);
        }
    }

    /// Discard every word waiting in the response FIFO. Returns how many were
    /// popped, so a caller can report a stale response instead of ignoring it.
    pub fn drain(&mut self) -> usize
    {
        let mut popped = 0;
        while self.status() & STATUS_EMPTY == 0 && popped < POLL_LIMIT as usize
        {
            self.pop();
            popped += 1;
        }
        popped
    }
}
impl Channel for Mailbox
{
    fn transact(&mut self, address: u32, words: &mut [u32]) -> Result<(), Error>
    {
        // The firmware reads the message through the bus, so the buffer must be
        // complete before the doorbell. The slice is otherwise untouched.
        debug_assert!(words.len() >= 3, "a finished message has a header and an end tag");
        let mut spins = 0;
        while self.status() & STATUS_FULL != 0
        {
            spins += 1;
            if spins == POLL_LIMIT
            {
                return Err(Error::MailboxFull);
            }
        }
        // A response nobody collected - a requester that gave up waiting before
        // the firmware answered - is still in the FIFO, and would be read below
        // as the answer to this request. Discard it first. Observed on a board:
        // a slow power request timed out in the boot stage, and the display
        // driver's first exchange, seconds later, read its reply.
        self.drain();
        // Order the message stores before the device write. DMB (SeqCst fence)
        // orders normal memory against the Device window on AArch64.
        fence(Ordering::SeqCst);
        self.push(doorbell(address));
        spins = 0;
        while self.status() & STATUS_EMPTY != 0
        {
            spins += 1;
            if spins == POLL_LIMIT
            {
                return Err(Error::NoResponse);
            }
        }
        let word = self.pop();
        // The first response word is this request's address and channel. A
        // different word means another requester's response is in the FIFO;
        // report it rather than reading a reply that is not ours.
        let ours = word == doorbell(address);
        self.drain();
        // Order the response reads after the firmware's buffer writes.
        fence(Ordering::SeqCst);
        if ours { Ok(()) } else { Err(Error::ForeignResponse { word }) }
    }
}

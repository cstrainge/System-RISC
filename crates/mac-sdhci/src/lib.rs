//! SDHCI host controller: card identification and single-block PIO transfers.
//!
//! This is the reference reader for the Arasan controller the BCM2837-class
//! platform declares. It is shared by the bootrom, which needs the medium
//! before any module can load, and by the hardware self-test, which reads sector
//! zero and nothing else. A write path moves one 512-byte block at a time in
//! the other direction, so the bootrom can append its log to the card;
//! partitioning stays above it, and this crate only brings the card up and
//! moves one block at a time.
//!
//! Register offsets and bit positions follow the SD Host Controller Simplified
//! Specification. All register access is little-endian MMIO into the window the
//! caller supplies from the platform record.
//!
//! The bit positions below were corrected on a physical BCM2837-class board.
//! Two of them are
//! indistinguishable under QEMU, whose model has the data available
//! immediately: `PRESENT_BUFFER_READ` is bit 11 (data in the buffer), not bit 9
//! (data transfer active), and CMD17 must not set the multi-block select bit.
//! Both errors read a full block of zeroes on silicon while passing emulation.
#![no_std]
use core::ptr::{read_volatile, write_volatile};

#[cfg(feature = "block-device")]
use mac_block::{BlockDevice, Error, WritableBlockDevice};

/// Bytes in one block. The same value as `mac_block::SECTOR_SIZE`, restated
/// here so the reader can be built without that crate.
pub const SECTOR_SIZE: usize = 512;

const BLOCK_SIZE: usize = 0x04;
const BLOCK_COUNT: usize = 0x06;
const ARGUMENT: usize = 0x08;
const TRANSFER_MODE: usize = 0x0c;
const BUFFER_DATA: usize = 0x20;
const PRESENT_STATE: usize = 0x24;
const POWER_CONTROL: usize = 0x29;
const CLOCK_CONTROL: usize = 0x2c;
const TIMEOUT_CONTROL: usize = 0x2e;
const SOFTWARE_RESET: usize = 0x2f;
const NORMAL_INT_STATUS: usize = 0x30;
const ERROR_INT_STATUS: usize = 0x32;
const NORMAL_INT_ENABLE: usize = 0x34;
const ERROR_INT_ENABLE: usize = 0x36;
const CAPABILITIES: usize = 0x40;

const INT_COMMAND_COMPLETE: u16 = 1 << 0;
const INT_TRANSFER_COMPLETE: u16 = 1 << 1;
const INT_BUFFER_WRITE_READY: u16 = 1 << 4;
const INT_BUFFER_READ_READY: u16 = 1 << 5;
const INT_ERROR: u16 = 1 << 15;
const ERROR_ALL: u16 = 0xffff;

const PRESENT_COMMAND_INHIBIT: u32 = 1 << 0;
const PRESENT_DAT_INHIBIT: u32 = 1 << 1;
/// Present State bit 11, "Data Buffer Read Enable": there are bytes in the
/// buffer to be read. Bit 9 looks like the same thing and is not — it is
/// "Data Transfer Active", set for the whole transfer — so draining on it reads
/// words before the card has sent them and yields an all-zero block.
const PRESENT_BUFFER_READ: u32 = 1 << 11;
/// Present State bit 10, "Data Buffer Write Enable": the buffer accepts a
/// word. The write mirror of `PRESENT_BUFFER_READ`, and the same trap applies:
/// bit 9 is transfer-active, not buffer-ready, so filling on it writes words
/// the controller is not ready to take.
const PRESENT_BUFFER_WRITE: u32 = 1 << 10;

const CLOCK_INTERNAL_ENABLE: u32 = 1 << 0;
const CLOCK_INTERNAL_STABLE: u32 = 1 << 1;
const CLOCK_SD_ENABLE: u32 = 1 << 2;

const RESET_ALL: u8 = 1;
const POWER_3V3: u8 = 0x0f;
const TIMEOUT_MAXIMUM: u8 = 0x0e;
const INITIAL_HZ: u32 = 400_000;
/// The rate asked for once the card is selected.
///
/// Identification must run slowly; transfers need not, and a card left at the
/// identification rate reads a multi-megabyte System at tens of kilobytes a
/// second. The figure is deliberately modest because `base_hz` falls back to
/// 50 MHz when the capabilities register states no base clock, which is what
/// the BCM2835 controller does, and that board's real base clock can be up to
/// five times the fallback: a divider of five keeps the card at or below the
/// 25 MHz default-speed limit either way. Asking the firmware for the real
/// base clock would allow the full rate.
const TRANSFER_HZ: u32 = 5_000_000;

const RESPONSE_NONE: u16 = 0;
const RESPONSE_136: u16 = 1;
const RESPONSE_48: u16 = 2;
const RESPONSE_48_BUSY: u16 = 3;

const CMD_GO_IDLE: u32 = 0;
const CMD_ALL_SEND_CID: u32 = 2;
const CMD_SEND_RELATIVE_ADDRESS: u32 = 3;
const CMD_SELECT_CARD: u32 = 7;
const CMD_SEND_IF_COND: u32 = 8;
const CMD_SEND_CSD: u32 = 9;
const CMD_SET_BLOCK_LENGTH: u32 = 16;
const CMD_READ_SINGLE_BLOCK: u32 = 17;
const CMD_WRITE_SINGLE_BLOCK: u32 = 24;
const CMD_APP_COMMAND: u32 = 55;
const CMD_APP_SEND_OP_COND: u32 = 41;

/// A real card may take close to a second to leave idle in ACMD41, and each
/// poll here is a pair of commands, so this is not a tight spin count.
const INITIALISE_POLLS: usize = 1_000_000;
/// Enough idle time for the 74 clocks a card needs before it answers its first
/// command. At the 400 kHz identification rate that is under 200 microseconds,
/// so this spin is comfortably longer than required and costs nothing at boot.
const STARTUP_CLOCKS: u32 = 100_000;
const TRANSFER_POLLS: usize = 1_000_000;
/// How long a written block is given to be programmed by the card. See
/// `transfer_write`.
const WRITE_COMPLETE_POLLS: usize = 64 * TRANSFER_POLLS;

/// Why a step refused, with enough state to tell the possibilities apart on
/// real hardware: which command stopped it, and what the two interrupt status
/// registers held when it did. `command` is `u8::MAX` for failures that are not
/// tied to a command.
pub struct Failure
{
    pub what:      &'static str,
    pub command:   u8,
    pub normal:    u16,
    pub error:     u16,
    /// The capabilities register at the time of failure, so a wrong base clock
    /// can be told from a card that simply refused.
    pub caps:      u32,
    /// Normal interrupt status read *before* the command was issued. Non-zero
    /// here means the controller was already reporting something, so the error
    /// did not come from this command.
    pub before:    u16,
    /// A command that completed and returned a response even though the error
    /// interrupt was asserted; `resp` is that first response word.
    pub completed: bool,
    pub resp:      u32
}

impl Failure
{
    fn plain(what: &'static str) -> Self
    {
        Self { what,
               command: u8::MAX,
               normal: 0,
               error: 0,
               caps: 0,
               before: 0,
               completed: false,
               resp: 0 }
    }
}

impl core::fmt::Display for Failure
{
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        write!(formatter,
               "{} cmd={} int={:#06x} err={:#06x} caps={:#010x}",
               self.what, self.command, self.normal, self.error, self.caps)
    }
}

/// Whether a command carries a data phase, and in which direction. The
/// transfer-mode register's direction bit (bit 4) is read for a read and
/// clear for a write; a command with no data phase leaves the block-size
/// and block-count registers alone.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Transfer
{
    None,
    Read,
    Write
}

/// The controller and the one card in it.
pub struct Disk
{
    base:          usize,
    sectors:       u64,
    relative:      u16,
    high_capacity: bool,
    /// Controller state captured when a sector read finished, so a read that
    /// returns zeros can be told from a read that never really completed.
    state_after:   u32,
    normal_after:  u16,
    error_after:   u16
}

impl Disk
{
    /// Brings the controller and card up. `base` is the window the platform
    /// record declares. `stage` reports how far the sequence reached, so a
    /// refusal names the step that stopped it.
    pub fn probe(base: usize, stage: &mut u8) -> core::result::Result<Self, Failure>
    {
        let mut disk = Self { base,
                              sectors: 0,
                              relative: 0,
                              high_capacity: false,
                              state_after: 0,
                              normal_after: 0,
                              error_after: 0 };
        *stage = 1;
        disk.reset()?;
        *stage = 2;
        disk.set_clock(INITIAL_HZ)?;
        *stage = 3;
        disk.write8(POWER_CONTROL, POWER_3V3);
        disk.write8(TIMEOUT_CONTROL, TIMEOUT_MAXIMUM);
        // A status bit is only latched when its enable bit is set, so polling
        // Command Complete needs this mask. Buffer Write Ready is enabled
        // beside Buffer Read Ready: a write fills the buffer through the same
        // status register, and a bit that never latches reads as a card that
        // never accepts a word.
        disk.write16(NORMAL_INT_ENABLE,
                     INT_COMMAND_COMPLETE
                     | INT_TRANSFER_COMPLETE
                     | INT_BUFFER_READ_READY
                     | INT_BUFFER_WRITE_READY);
        disk.write16(ERROR_INT_ENABLE, ERROR_ALL);
        // A card needs at least 74 clocks with the command line idle before it
        // will answer anything. The clock is running by now, so hold still long
        // enough for those; a card that has not been clocked never answers
        // CMD8, which is indistinguishable from a dead controller. QEMU does
        // not require this, so only silicon shows the difference.
        disk.wait_startup_clocks();
        *stage = 4;
        disk.identify(stage)?;
        *stage = 8;
        Ok(disk)
    }

    /// The controller state captured when the last read finished.
    pub fn after_read(&self) -> (u32, u16, u16)
    {
        (self.state_after, self.normal_after, self.error_after)
    }

    fn wait_startup_clocks(&self)
    {
        for _ in 0..STARTUP_CLOCKS
        {
            core::hint::spin_loop();
        }
    }

    fn read8(&self, offset: usize) -> u8
    {
        // SAFETY: `offset` is a controller register and the window is mapped
        // Device memory for the whole run.
        unsafe { read_volatile((self.base + offset) as *const u8) }
    }

    fn read16(&self, offset: usize) -> u16
    {
        // SAFETY: as `read8`.
        unsafe { u16::from_le(read_volatile((self.base + offset) as *const u16)) }
    }

    fn read32(&self, offset: usize) -> u32
    {
        // SAFETY: as `read8`.
        unsafe { u32::from_le(read_volatile((self.base + offset) as *const u32)) }
    }

    fn write8(&self, offset: usize, value: u8)
    {
        // SAFETY: as `read8`.
        unsafe { write_volatile((self.base + offset) as *mut u8, value) };
    }

    fn write16(&self, offset: usize, value: u16)
    {
        // SAFETY: as `read8`.
        unsafe { write_volatile((self.base + offset) as *mut u16, value.to_le()) };
    }

    fn write32(&self, offset: usize, value: u32)
    {
        // SAFETY: as `read8`.
        unsafe { write_volatile((self.base + offset) as *mut u32, value.to_le()) };
    }

    fn reset(&mut self) -> core::result::Result<(), Failure>
    {
        self.write8(SOFTWARE_RESET, RESET_ALL);
        for _ in 0..TRANSFER_POLLS
        {
            if self.read8(SOFTWARE_RESET) & RESET_ALL == 0
            {
                return Ok(());
            }
        }
        Err(Failure::plain("controller-reset"))
    }

    fn base_hz(&self) -> u32
    {
        let megahertz = (self.read32(CAPABILITIES) >> 8) & 0xff;
        if megahertz == 0 { 50_000_000 } else { megahertz * 1_000_000 }
    }

    fn set_clock(&mut self, hz: u32) -> core::result::Result<(), Failure>
    {
        let base = self.base_hz();
        let mut divider = base / (2 * hz);
        if divider == 0
        {
            divider = 1;
        }
        if divider > 0x3ff
        {
            divider = 0x3ff;
        }
        let encoded = ((divider & 0xff) << 8) | (((divider >> 8) & 0x3) << 6);
        self.write32(CLOCK_CONTROL, encoded);
        self.write32(CLOCK_CONTROL, encoded | CLOCK_INTERNAL_ENABLE);
        let mut stable = false;
        for _ in 0..TRANSFER_POLLS
        {
            if self.read32(CLOCK_CONTROL) & CLOCK_INTERNAL_STABLE != 0
            {
                stable = true;
                break;
            }
        }
        if !stable
        {
            return Err(Failure::plain("clock-unstable"));
        }
        self.write32(CLOCK_CONTROL,
                     encoded | CLOCK_INTERNAL_ENABLE | CLOCK_INTERNAL_STABLE | CLOCK_SD_ENABLE);
        Ok(())
    }

    fn wait_inhibit(&self) -> core::result::Result<(), Failure>
    {
        for _ in 0..TRANSFER_POLLS
        {
            if self.read32(PRESENT_STATE) & (PRESENT_COMMAND_INHIBIT | PRESENT_DAT_INHIBIT) == 0
            {
                return Ok(());
            }
        }
        Err(Failure::plain("controller-inhibited"))
    }

    /// Issues one command. `crc` and `index_check` are per response type, not
    /// per command: R3 carries no CRC field, and R2 carries no command index,
    /// so enabling either check on those responses makes the controller report
    /// an error the card never made. QEMU validates neither, so only real
    /// hardware shows the difference.
    fn command(&mut self, index: u32, argument: u32, response: u16, crc: bool, index_check: bool,
               transfer: Transfer)
               -> core::result::Result<[u32; 4], Failure>
    {
        self.wait_inhibit()?;
        self.write16(NORMAL_INT_STATUS, 0xffff);
        self.write16(ERROR_INT_STATUS, ERROR_ALL);
        // Read back after the write-1-to-clear. A stuck error interrupt here
        // means the flag was already asserted and the clear did not take, so
        // the failure below belongs to an earlier command rather than this one.
        let before = self.read16(NORMAL_INT_STATUS);
        let mut direction = 0u16;
        if transfer != Transfer::None
        {
            if transfer == Transfer::Read
            {
                // Read direction only. CMD17 is a single-block command, so the
                // multi-block bit stays clear: setting it tells the controller to
                // expect further blocks and, with Auto CMD12 disabled, leaves the
                // card mid-transfer once the one block has gone.
                direction = 1 << 4;
            }
            self.write16(BLOCK_SIZE, SECTOR_SIZE as u16);
            self.write16(BLOCK_COUNT, 1);
        }
        self.write32(ARGUMENT, argument);
        let mut flags = (index << 8) | u32::from(response);
        if crc
        {
            flags |= 1 << 3;
        }
        if index_check
        {
            flags |= 1 << 4;
        }
        if transfer != Transfer::None
        {
            flags |= 1 << 5;
        }
        self.write32(TRANSFER_MODE, (flags << 16) | u32::from(direction));
        for _ in 0..TRANSFER_POLLS
        {
            let status = self.read16(NORMAL_INT_STATUS);
            if status & INT_ERROR != 0
            {
                // A completed command has a valid response even when the error
                // interrupt is asserted, and reading it separates "the card
                // refused" from "the controller complained about nothing".
                let completed = status & INT_COMMAND_COMPLETE != 0;
                return Err(Failure { what: "command-failed",
                                     command: index as u8,
                                     normal: status,
                                     error: self.read16(ERROR_INT_STATUS),
                                     caps: self.read32(CAPABILITIES),
                                     before,
                                     completed,
                                     resp: if completed { self.read32(0x10) } else { 0 } });
            }
            if status & INT_COMMAND_COMPLETE != 0
            {
                return Ok([self.read32(0x10),
                           self.read32(0x14),
                           self.read32(0x18),
                           self.read32(0x1c)]);
            }
        }
        Err(Failure { what: "command-timeout",
                      command: index as u8,
                      normal: self.read16(NORMAL_INT_STATUS),
                      error: self.read16(ERROR_INT_STATUS),
                      caps: self.read32(CAPABILITIES),
                      before,
                      completed: false,
                      resp: 0 })
    }

    fn identify(&mut self, stage: &mut u8) -> core::result::Result<(), Failure>
    {
        *stage = 5;
        self.command(CMD_GO_IDLE, 0, RESPONSE_NONE, false, false, Transfer::None)?;
        *stage = 6;
        self.command(CMD_SEND_IF_COND, 0x1aa, RESPONSE_48, true, true, Transfer::None)?;
        let mut ready = false;
        for _ in 0..INITIALISE_POLLS
        {
            self.command(CMD_APP_COMMAND, 0, RESPONSE_48, true, true, Transfer::None)?;
            // R3: the OCR reply carries neither a CRC nor a command index.
            let response = self.command(CMD_APP_SEND_OP_COND,
                                        0x40ff_8000,
                                        RESPONSE_48,
                                        false,
                                        false,
                                        Transfer::None)?;
            if response[0] & 0x8000_0000 != 0
            {
                self.high_capacity = response[0] & 0x4000_0000 != 0;
                ready = true;
                break;
            }
        }
        if !ready
        {
            return Err(Failure::plain("card-init"));
        }
        *stage = 7;
        // R2: 136-bit CID, CRC present, no command index in the reply.
        self.command(CMD_ALL_SEND_CID, 0, RESPONSE_136, true, false, Transfer::None)?;
        let relative =
            self.command(CMD_SEND_RELATIVE_ADDRESS, 0, RESPONSE_48, true, true, Transfer::None)?;
        self.relative = (relative[0] >> 16) as u16;
        // The card description is readable in the stand-by state; asking for it
        // after SELECT_CARD times out on this controller.
        let csd_response = self.command(CMD_SEND_CSD,
                                        u32::from(self.relative) << 16,
                                        RESPONSE_136,
                                        true,
                                        false,
                                        Transfer::None)?;
        // SDHCI presents a 136-bit response most-significant word last, so the
        // card description reads from RESP3 down to RESP0. The controller also
        // strips the response's trailing CRC byte, so the registers hold bits
        // 127:8 of the description in their bits 119:0: every field sits eight
        // bits lower than the card specification numbers it. Shifting the whole
        // value up by eight puts each field back where `capacity_sectors`
        // expects it. Without this a version 2.0 card reads as version 1.0 and
        // its size is nonsense: observed on a BCM2837 with a 7.35 GB card,
        // which reported 0x80200 sectors (256 MiB).
        let raw = [csd_response[3], csd_response[2], csd_response[1], csd_response[0]];
        let csd = [(raw[0] << 8) | (raw[1] >> 24),
                   (raw[1] << 8) | (raw[2] >> 24),
                   (raw[2] << 8) | (raw[3] >> 24),
                   raw[3] << 8];
        self.command(CMD_SELECT_CARD,
                     u32::from(self.relative) << 16,
                     RESPONSE_48_BUSY,
                     true,
                     true,
                     Transfer::None)?;
        self.command(CMD_SET_BLOCK_LENGTH,
                     SECTOR_SIZE as u32,
                     RESPONSE_48,
                     true,
                     true,
                     Transfer::None)?;
        self.set_clock(TRANSFER_HZ)?;
        // The clock is programmed with a write of the whole register word, and
        // the data timeout shares that word, so it is stated again.
        self.write8(TIMEOUT_CONTROL, TIMEOUT_MAXIMUM);
        self.sectors = capacity_sectors(csd)?;
        Ok(())
    }

    /// Reads one block through the controller's buffer port.
    /// Waits for the controller to declare a drained read finished, then
    /// clears the read's status bits so none of them can be mistaken for the
    /// next command's.
    fn finish_read(&mut self) -> core::result::Result<(), Failure>
    {
        for _ in 0..TRANSFER_POLLS
        {
            let status = self.read16(NORMAL_INT_STATUS);
            if status & INT_ERROR != 0
            {
                return Err(Failure { what:      "read-failed-after-data",
                                     command:   CMD_READ_SINGLE_BLOCK as u8,
                                     normal:    status,
                                     error:     self.read16(ERROR_INT_STATUS),
                                     caps:      self.read32(CAPABILITIES),
                                     before:    0,
                                     completed: true,
                                     resp:      0 });
            }
            if status & INT_TRANSFER_COMPLETE != 0
            {
                self.write16(NORMAL_INT_STATUS, INT_TRANSFER_COMPLETE | INT_BUFFER_READ_READY);
                return Ok(());
            }
        }
        Err(Failure { what:      "read-never-completed",
                      command:   CMD_READ_SINGLE_BLOCK as u8,
                      normal:    self.read16(NORMAL_INT_STATUS),
                      error:     self.read16(ERROR_INT_STATUS),
                      caps:      self.read32(CAPABILITIES),
                      before:    0,
                      completed: true,
                      resp:      0 })
    }

    fn transfer(&mut self, argument: u32) -> core::result::Result<[u8; SECTOR_SIZE], Failure>
    {
        self.command(CMD_READ_SINGLE_BLOCK, argument, RESPONSE_48, true, true, Transfer::Read)?;
        let mut bytes = [0u8; SECTOR_SIZE];
        let mut words = 0usize;
        let mut saw_ready = false;
        for _ in 0..TRANSFER_POLLS
        {
            let status = self.read16(NORMAL_INT_STATUS);
            if status & INT_ERROR != 0
            {
                return Err(Failure { what:      "read-failed",
                                     command:   CMD_READ_SINGLE_BLOCK as u8,
                                     normal:    status,
                                     error:     self.read16(ERROR_INT_STATUS),
                                     caps:      self.read32(CAPABILITIES),
                                     before:    0,
                                     completed: false,
                                     resp:      0 });
            }
            // Drain only once there is actually data to take. Either signal
            // will do: the Buffer Read Ready interrupt, or the polled
            // data-available bit in PRESENT_STATE.
            if status & INT_BUFFER_READ_READY != 0
               || self.read32(PRESENT_STATE) & PRESENT_BUFFER_READ != 0
            {
                saw_ready = true;
                while words < SECTOR_SIZE / 4
                      && self.read32(PRESENT_STATE) & PRESENT_BUFFER_READ != 0
                {
                    let word = self.read32(BUFFER_DATA).to_le_bytes();
                    bytes[words * 4..words * 4 + 4].copy_from_slice(&word);
                    words += 1;
                }
            }
            if words == SECTOR_SIZE / 4
            {
                // The data is in hand, but the transfer is not over until the
                // controller raises Transfer Complete. Returning before that
                // lets the completion of this read land inside the next one,
                // which then sees "complete" with no data and leaves the
                // controller busy. Observed on a BCM2837: the first read of
                // sector 0 succeeded and the second reported no-buffer-ready.
                // An emulator completes instantly, so only hardware shows it.
                self.finish_read()?;
                self.state_after = self.read32(PRESENT_STATE);
                self.normal_after = self.read16(NORMAL_INT_STATUS);
                self.error_after = self.read16(ERROR_INT_STATUS);
                return Ok(bytes);
            }
            if status & INT_TRANSFER_COMPLETE != 0 && words < SECTOR_SIZE / 4
            {
                // Not having seen Buffer Read Ready at all is a different fault
                // from draining a block that then came up short.
                return Err(Failure { what:      if saw_ready
                                     {
                                         "read-short"
                                     }
                                     else
                                     {
                                         "no-buffer-ready"
                                     },
                                     command:   CMD_READ_SINGLE_BLOCK as u8,
                                     normal:    status,
                                     error:     self.read16(ERROR_INT_STATUS),
                                     caps:      self.read32(CAPABILITIES),
                                     before:    0,
                                     completed: true,
                                     resp:      0 });
            }
        }
        Err(Failure { what:      "read-timeout",
                      command:   CMD_READ_SINGLE_BLOCK as u8,
                      normal:    self.read16(NORMAL_INT_STATUS),
                      error:     self.read16(ERROR_INT_STATUS),
                      caps:      self.read32(CAPABILITIES),
                      before:    0,
                      completed: false,
                      resp:      0 })
    }

    /// Writes one block through the controller's buffer port. The mirror of
    /// `transfer`, with one difference that matters: a read is done when its
    /// last word is drained, but a write is not done when its last word is
    /// accepted — the block is only on the card once the transfer completes,
    /// so this returns only after Transfer Complete, not after the buffer
    /// fills. A caller that returns early on a filled buffer reports a write
    /// that may still be in flight.
    fn transfer_write(&mut self, argument: u32, data: &[u8; SECTOR_SIZE])
                      -> core::result::Result<(), Failure>
    {
        self.command(CMD_WRITE_SINGLE_BLOCK, argument, RESPONSE_48, true, true, Transfer::Write)?;
        let mut words = 0usize;
        let mut saw_ready = false;
        for _ in 0..TRANSFER_POLLS
        {
            let status = self.read16(NORMAL_INT_STATUS);
            if status & INT_ERROR != 0
            {
                return Err(self.write_failure("write-failed", status, false));
            }
            if status & INT_BUFFER_WRITE_READY != 0
            {
                // Buffer Write Ready means the buffer has room for the whole
                // block, so the block is written in one go and the latched
                // status is cleared first. The polled per-word bit in
                // PRESENT_STATE is not consulted on this path: the interrupt is
                // the specification's statement that a block fits, and a fill
                // gated on a bit that reads clear would never advance.
                saw_ready = true;
                self.write16(NORMAL_INT_STATUS, INT_BUFFER_WRITE_READY);
                while words < SECTOR_SIZE / 4
                {
                    let at = words * 4;
                    let word =
                        u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
                    self.write32(BUFFER_DATA, word);
                    words += 1;
                }
            }
            else if self.read32(PRESENT_STATE) & PRESENT_BUFFER_WRITE != 0
            {
                // A controller that reports room without latching the
                // interrupt is filled a word at a time, for as long as it says
                // there is room.
                saw_ready = true;
                while words < SECTOR_SIZE / 4
                      && self.read32(PRESENT_STATE) & PRESENT_BUFFER_WRITE != 0
                {
                    let at = words * 4;
                    let word =
                        u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
                    self.write32(BUFFER_DATA, word);
                    words += 1;
                }
            }
            if words == SECTOR_SIZE / 4
            {
                break;
            }
            if status & INT_TRANSFER_COMPLETE != 0
            {
                // Not having seen Buffer Write Ready at all is a different fault
                // from filling a block that then came up short.
                return Err(self.write_failure(if saw_ready
                                              {
                                                  "write-short"
                                              }
                                              else
                                              {
                                                  "no-buffer-write-ready"
                                              },
                                              status,
                                              true));
            }
        }
        if words < SECTOR_SIZE / 4
        {
            let status = self.read16(NORMAL_INT_STATUS);
            return Err(self.write_failure("write-never-ready", status, false));
        }
        // The block is in the controller; now the card has to program it, and
        // that is the slow part - the specification allows a card a quarter of
        // a second and more, where a read is over in a few milliseconds. The
        // wait is a poll count, not a clock, so it is given a budget far past
        // the one a read needs: the same budget here is what made a healthy
        // card look like a write that never finished.
        for _ in 0..WRITE_COMPLETE_POLLS
        {
            let status = self.read16(NORMAL_INT_STATUS);
            if status & INT_ERROR != 0
            {
                return Err(self.write_failure("write-failed-after-data", status, false));
            }
            if status & INT_TRANSFER_COMPLETE != 0
            {
                self.write16(NORMAL_INT_STATUS, INT_TRANSFER_COMPLETE | INT_BUFFER_WRITE_READY);
                return Ok(());
            }
        }
        let status = self.read16(NORMAL_INT_STATUS);
        Err(self.write_failure("write-never-completed", status, false))
    }

    /// The controller's state at a refused write.
    fn write_failure(&self, what: &'static str, normal: u16, completed: bool) -> Failure
    {
        Failure { what,
                  command: CMD_WRITE_SINGLE_BLOCK as u8,
                  normal,
                  error: self.read16(ERROR_INT_STATUS),
                  caps: self.read32(CAPABILITIES),
                  before: 0,
                  completed,
                  resp: 0 }
    }

    /// Writes one block. The sector must be inside the card; a write past the
    /// end is refused rather than wrapped.
    pub fn write_block(&mut self, sector: u64, data: &[u8; SECTOR_SIZE])
                       -> core::result::Result<(), Failure>
    {
        if sector >= self.sectors
        {
            return Err(Failure::plain("outside-card"));
        }
        let argument = if self.high_capacity { sector as u32 } else { (sector * 512) as u32 };
        self.transfer_write(argument, data)
    }

    /// Reads one block and returns the controller state on refusal. The
    /// `BlockDevice` implementation discards that state; a diagnostic caller
    /// keeps it.
    pub fn read_block(&mut self, sector: u64) -> core::result::Result<[u8; SECTOR_SIZE], Failure>
    {
        if sector >= self.sectors
        {
            return Err(Failure::plain("outside-card"));
        }
        let argument = if self.high_capacity { sector as u32 } else { (sector * 512) as u32 };
        self.transfer(argument)
    }
}

#[cfg(feature = "block-device")]
impl BlockDevice for Disk
{
    fn sector_count(&self) -> u64
    {
        self.sectors
    }

    fn read_sector(&mut self, sector: u64, output: &mut [u8; SECTOR_SIZE])
                   -> mac_block::Result<()>
    {
        *output = self.read_block(sector).map_err(|failure| Error::Malformed(failure.what))?;
        Ok(())
    }
}

#[cfg(feature = "block-device")]
impl WritableBlockDevice for Disk
{
    fn is_read_only(&self) -> bool
    {
        false
    }

    fn write_sector(&mut self, sector: u64, input: &[u8; SECTOR_SIZE]) -> mac_block::Result<()>
    {
        self.write_block(sector, input).map_err(|failure| Error::Malformed(failure.what))?;
        Ok(())
    }

    fn flush(&mut self) -> mac_block::Result<()>
    {
        // No write cache sits between this driver and the card: a block is on
        // the card when `write_block` returns, so there is nothing to flush.
        Ok(())
    }
}

/// Capacity from the card's CSD. Version 2.0 cards report capacity in 512 KiB
/// units; version 1.0 cards report it in bytes with a block length.
fn capacity_sectors(csd: [u32; 4]) -> core::result::Result<u64, Failure>
{
    // Words are ordered most-significant first, so `csd[0]` holds bits 127:96.
    let structure = (csd[0] >> 30) & 0x3;
    if structure == 1
    {
        // Capacity is reported in 512 KiB units, split across the two words.
        let c_size = (u64::from(csd[1] & 0x3f) << 16) | u64::from((csd[2] >> 16) & 0xffff);
        return Ok((c_size + 1) * 1024);
    }
    if structure == 0
    {
        let read_bl_len = (csd[1] >> 16) & 0xf;
        let c_size = ((csd[1] & 0x3ff) << 2) | ((csd[2] >> 30) & 0x3);
        let c_size_mult = (csd[2] >> 15) & 0x7;
        let block = 1u64 << read_bl_len;
        let multiplier = 1u64 << (c_size_mult + 2);
        let bytes = (u64::from(c_size) + 1) * multiplier * block;
        return Ok(bytes / SECTOR_SIZE as u64);
    }
    Err(Failure::plain(if structure == 2 { "csd-structure-2" } else { "csd-structure-3" }))
}

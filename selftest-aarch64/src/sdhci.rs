//! SDHCI single-block PIO read, trimmed from `bootrom-aarch64/src/sdhci.rs`.
//! The self-test only ever reads sector zero, so the card-description decode and
//! write support are deliberately absent: nothing here can damage the card.
//!
//! Register offsets and bit positions follow the SD Host Controller Simplified
//! Specification. All register access is little-endian MMIO into the window the
//! platform record declares.
use core::ptr::{read_volatile, write_volatile};

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
const INT_BUFFER_READ_READY: u16 = 1 << 5;
const INT_ERROR: u16 = 1 << 15;
const ERROR_ALL: u16 = 0xffff;

const PRESENT_COMMAND_INHIBIT: u32 = 1 << 0;
const PRESENT_DAT_INHIBIT: u32 = 1 << 1;
const PRESENT_BUFFER_READ: u32 = 1 << 9;

const CLOCK_INTERNAL_ENABLE: u32 = 1 << 0;
const CLOCK_INTERNAL_STABLE: u32 = 1 << 1;
const CLOCK_SD_ENABLE: u32 = 1 << 2;

const RESET_ALL: u8 = 1;
const POWER_3V3: u8 = 0x0f;
const TIMEOUT_MAXIMUM: u8 = 0x0e;
const INITIAL_HZ: u32 = 400_000;

const RESPONSE_NONE: u16 = 0;
const RESPONSE_136: u16 = 1;
const RESPONSE_48: u16 = 2;
const RESPONSE_48_BUSY: u16 = 3;

const CMD_GO_IDLE: u32 = 0;
const CMD_ALL_SEND_CID: u32 = 2;
const CMD_SEND_RELATIVE_ADDRESS: u32 = 3;
const CMD_SELECT_CARD: u32 = 7;
const CMD_SEND_IF_COND: u32 = 8;
const CMD_SET_BLOCK_LENGTH: u32 = 16;
const CMD_READ_SINGLE_BLOCK: u32 = 17;
const CMD_APP_COMMAND: u32 = 55;
const CMD_APP_SEND_OP_COND: u32 = 41;

pub const SECTOR_BYTES: usize = 512;
const INITIALISE_POLLS: usize = 20_000;
const TRANSFER_POLLS: usize = 1_000_000;

/// Why a step refused, with enough state to tell the possibilities apart on
/// real hardware: which command stopped it, and what the two interrupt status
/// registers held when it did. `command` is `u8::MAX` for failures that are not
/// tied to a command.
pub struct Failure
{
    pub what:    &'static str,
    pub command: u8,
    pub normal:  u16,
    pub error:   u16
}

impl Failure
{
    fn plain(what: &'static str) -> Self
    {
        Self { what,
               command: u8::MAX,
               normal:  0,
               error:   0 }
    }
}

/// The controller and the one card in it.
pub struct Disk
{
    base:          usize,
    relative:      u16,
    high_capacity: bool
}

impl Disk
{
    /// Brings the controller and card up. Every refusal names the step that
    /// stopped it, because "no card" and "the card answered garbage" are
    /// different findings.
    pub fn probe(base: usize) -> Result<Self, Failure>
    {
        let mut disk = Self { base,
                              relative: 0,
                              high_capacity: false };
        disk.reset()?;
        disk.set_clock(INITIAL_HZ)?;
        disk.write8(POWER_CONTROL, POWER_3V3);
        disk.write8(TIMEOUT_CONTROL, TIMEOUT_MAXIMUM);
        // A status bit is only latched when its enable bit is set, so polling
        // Command Complete needs this mask.
        disk.write16(NORMAL_INT_ENABLE,
                     INT_COMMAND_COMPLETE | INT_TRANSFER_COMPLETE | INT_BUFFER_READ_READY);
        disk.write16(ERROR_INT_ENABLE, ERROR_ALL);
        disk.identify()?;
        Ok(disk)
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

    fn reset(&mut self) -> Result<(), Failure>
    {
        self.write8(SOFTWARE_RESET, RESET_ALL);
        for _ in 0..TRANSFER_POLLS
        {
            if self.read8(SOFTWARE_RESET) & RESET_ALL == 0
            {
                return Ok(());
            }
        }
        Err(Failure::plain("controller did not leave reset"))
    }

    fn base_hz(&self) -> u32
    {
        let megahertz = (self.read32(CAPABILITIES) >> 8) & 0xff;
        if megahertz == 0 { 50_000_000 } else { megahertz * 1_000_000 }
    }

    fn set_clock(&mut self, hz: u32) -> Result<(), Failure>
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
            return Err(Failure::plain("internal clock never stabilised"));
        }
        self.write32(CLOCK_CONTROL,
                     encoded | CLOCK_INTERNAL_ENABLE | CLOCK_INTERNAL_STABLE | CLOCK_SD_ENABLE);
        Ok(())
    }

    fn wait_inhibit(&self) -> Result<(), Failure>
    {
        for _ in 0..TRANSFER_POLLS
        {
            if self.read32(PRESENT_STATE) & (PRESENT_COMMAND_INHIBIT | PRESENT_DAT_INHIBIT) == 0
            {
                return Ok(());
            }
        }
        Err(Failure::plain("controller stayed inhibited"))
    }

    fn command(&mut self, index: u32, argument: u32, response: u16, data: bool)
               -> Result<[u32; 4], Failure>
    {
        self.wait_inhibit()?;
        self.write16(NORMAL_INT_STATUS, 0xffff);
        self.write16(ERROR_INT_STATUS, ERROR_ALL);
        let mut transfer = 0u16;
        if data
        {
            // Read direction, one block, block count enabled.
            transfer = (1 << 4) | (1 << 1) | (1 << 5);
            self.write16(BLOCK_SIZE, SECTOR_BYTES as u16);
            self.write16(BLOCK_COUNT, 1);
        }
        self.write32(ARGUMENT, argument);
        let mut flags = (index << 8) | u32::from(response);
        if response != RESPONSE_NONE
        {
            flags |= (1 << 3) | (1 << 4);
        }
        if data
        {
            flags |= 1 << 5;
        }
        self.write32(TRANSFER_MODE, (flags << 16) | u32::from(transfer));
        for _ in 0..TRANSFER_POLLS
        {
            let status = self.read16(NORMAL_INT_STATUS);
            if status & INT_ERROR != 0
            {
                return Err(Failure { what:    "command failed",
                                     command: index as u8,
                                     normal:  status,
                                     error:   self.read16(ERROR_INT_STATUS) });
            }
            if status & INT_COMMAND_COMPLETE != 0
            {
                return Ok([self.read32(0x10),
                           self.read32(0x14),
                           self.read32(0x18),
                           self.read32(0x1c)]);
            }
        }
        Err(Failure { what:    "command did not complete",
                      command: index as u8,
                      normal:  self.read16(NORMAL_INT_STATUS),
                      error:   self.read16(ERROR_INT_STATUS) })
    }

    fn identify(&mut self) -> Result<(), Failure>
    {
        self.command(CMD_GO_IDLE, 0, RESPONSE_NONE, false)?;
        self.command(CMD_SEND_IF_COND, 0x1aa, RESPONSE_48, false)?;
        let mut ready = false;
        for _ in 0..INITIALISE_POLLS
        {
            self.command(CMD_APP_COMMAND, 0, RESPONSE_48, false)?;
            let response = self.command(CMD_APP_SEND_OP_COND, 0x40ff_8000, RESPONSE_48, false)?;
            if response[0] & 0x8000_0000 != 0
            {
                self.high_capacity = response[0] & 0x4000_0000 != 0;
                ready = true;
                break;
            }
        }
        if !ready
        {
            return Err(Failure::plain("card never finished initialising"));
        }
        self.command(CMD_ALL_SEND_CID, 0, RESPONSE_136, false)?;
        let relative = self.command(CMD_SEND_RELATIVE_ADDRESS, 0, RESPONSE_48, false)?;
        self.relative = (relative[0] >> 16) as u16;
        self.command(CMD_SELECT_CARD, u32::from(self.relative) << 16, RESPONSE_48_BUSY, false)?;
        self.command(CMD_SET_BLOCK_LENGTH, SECTOR_BYTES as u32, RESPONSE_48, false)?;
        Ok(())
    }

    /// Reads sector zero through the controller's buffer port.
    pub fn read_sector_zero(&mut self) -> Result<[u8; SECTOR_BYTES], Failure>
    {
        self.command(CMD_READ_SINGLE_BLOCK, 0, RESPONSE_48, true)?;
        let mut bytes = [0u8; SECTOR_BYTES];
        let mut words = 0usize;
        for _ in 0..TRANSFER_POLLS
        {
            let status = self.read16(NORMAL_INT_STATUS);
            if status & INT_ERROR != 0
            {
                return Err(Failure { what:    "read failed",
                                     command: CMD_READ_SINGLE_BLOCK as u8,
                                     normal:  status,
                                     error:   self.read16(ERROR_INT_STATUS) });
            }
            while words < SECTOR_BYTES / 4 && self.read32(PRESENT_STATE) & PRESENT_BUFFER_READ != 0
            {
                let word = self.read32(BUFFER_DATA).to_le_bytes();
                bytes[words * 4..words * 4 + 4].copy_from_slice(&word);
                words += 1;
            }
            if words == SECTOR_BYTES / 4
            {
                return Ok(bytes);
            }
            if status & INT_TRANSFER_COMPLETE != 0 && words < SECTOR_BYTES / 4
            {
                return Err(Failure { what:    "transfer completed short",
                                     command: CMD_READ_SINGLE_BLOCK as u8,
                                     normal:  status,
                                     error:   self.read16(ERROR_INT_STATUS) });
            }
        }
        Err(Failure { what:    "read did not complete",
                      command: CMD_READ_SINGLE_BLOCK as u8,
                      normal:  self.read16(NORMAL_INT_STATUS),
                      error:   self.read16(ERROR_INT_STATUS) })
    }
}

/// A partition entry from the sector's MBR.
pub struct Partition
{
    pub index: usize,
    pub kind:  u8,
    pub first: u32
}

/// Decode the primary partition table. Returns the signature and up to four
/// entries; a zeroed entry means the slot is unused.
pub fn partitions(sector: &[u8; SECTOR_BYTES]) -> ([u8; 2], [Option<Partition>; 4])
{
    let signature = [sector[510], sector[511]];
    let mut entries: [Option<Partition>; 4] = [None, None, None, None];
    for index in 0..4
    {
        let at = 0x1be + index * 16;
        let kind = sector[at + 4];
        let first =
            u32::from_le_bytes([sector[at + 8], sector[at + 9], sector[at + 10], sector[at + 11]]);
        if kind != 0 || first != 0
        {
            entries[index] = Some(Partition { index,
                                              kind,
                                              first });
        }
    }
    (signature, entries)
}

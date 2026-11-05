//! SDHCI host controller, single-block PIO reads. This is the storage hardware
//! adapter: everything above it consumes `BlockDevice`, so the partition table,
//! FAT32 and HFS-B code is the same code the virtio controller uses.
//!
//! This is pre-module boot code and belongs here deliberately. Its job is to
//! fetch the payload that contains the drivers, so it must work before any
//! module can load. It is not the runtime block driver: `drivers/sdhci/` owns
//! that, with partitioning, write support and error recovery. If this reader
//! starts growing any of those, it has drifted past boot scope and the work
//! belongs in the driver module instead.
//!
//! The kernel gains no FAT32, filesystem or SD knowledge from this; the
//! bootloader hands on the medium or the loaded image.
//!
//! Register offsets and bit positions follow the SD Host Controller Simplified
//! Specification. All register access is 32-bit, 16-bit or 8-bit little-endian
//! MMIO into the window the platform record declares for the controller.
use core::ptr::{read_volatile, write_volatile};

use mac_block::{BlockDevice, Error, Result};

const BLOCK_SIZE: usize = 0x04;
const BLOCK_COUNT: usize = 0x06;
const ARGUMENT: usize = 0x08;
const TRANSFER_MODE: usize = 0x0C;
const BUFFER_DATA: usize = 0x20;
const PRESENT_STATE: usize = 0x24;
const POWER_CONTROL: usize = 0x29;
const CLOCK_CONTROL: usize = 0x2C;
const TIMEOUT_CONTROL: usize = 0x2E;
const SOFTWARE_RESET: usize = 0x2F;
const NORMAL_INT_STATUS: usize = 0x30;
const ERROR_INT_STATUS: usize = 0x32;
const NORMAL_INT_ENABLE: usize = 0x34;
const ERROR_INT_ENABLE: usize = 0x36;
const CAPABILITIES: usize = 0x40;

const INT_COMMAND_COMPLETE: u16 = 1 << 0;
const INT_TRANSFER_COMPLETE: u16 = 1 << 1;
const INT_BUFFER_READ_READY: u16 = 1 << 5;
const INT_ERROR: u16 = 1 << 15;
/// Every error status bit is reported together; a single mask is enough because
/// a failed command always leaves the card unusable until it is re-initialised.
const ERROR_ALL: u16 = 0xFFFF;

const PRESENT_COMMAND_INHIBIT: u32 = 1 << 0;
const PRESENT_DAT_INHIBIT: u32 = 1 << 1;
const PRESENT_BUFFER_READ: u32 = 1 << 9;

/// Clock Control layout: Internal Clock Enable bit 0, Internal Clock Stable
/// bit 1, SD Clock Enable bit 2, PLL Enable bit 3, and the divider in bits 8-15
/// with its upper two bits at 6-7. A divider written into bits 0-7 sets the
/// enable bits instead and the card never receives a clock.
const CLOCK_INTERNAL_ENABLE: u32 = 1 << 0;
const CLOCK_INTERNAL_STABLE: u32 = 1 << 1;
const CLOCK_SD_ENABLE: u32 = 1 << 2;

const RESET_ALL: u8 = 1;
/// Bus power on with the 3.3 V select, the voltage an SD card expects.
const POWER_3V3: u8 = 0x0F;
const TIMEOUT_MAXIMUM: u8 = 0x0E;
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
const CMD_SEND_CSD: u32 = 9;
const CMD_SET_BLOCK_LENGTH: u32 = 16;
const CMD_READ_SINGLE_BLOCK: u32 = 17;
const CMD_APP_COMMAND: u32 = 55;
const CMD_APP_SEND_OP_COND: u32 = 41;

const SECTOR_BYTES: usize = 512;
/// A card that never reports ready is absent or unresponsive; this bounds the
/// poll so an empty slot fails instead of hanging the boot.
const INITIALISE_POLLS: usize = 20_000;
const TRANSFER_POLLS: usize = 1_000_000;

pub struct Disk
{
    base:           usize,
    sectors:        u64,
    relative:       u16,
    high_capacity:  bool
}
impl Disk
{
    /// Brings the controller and card up. `base` is the window the platform
    /// record declares; the card's capacity comes from its CSD, so a second
    /// board with a different card needs no code change. `stage` reports how far
    /// the sequence reached, so a refusal names the step that stopped it.
    pub fn probe(base: usize, stage: &mut u8) -> Result<Self>
    {
        let mut disk = Self { base,
                              sectors: 0,
                              relative: 0,
                              high_capacity: false };
        *stage = 1;
        disk.reset()?;
        *stage = 2;
        disk.set_clock(INITIAL_HZ)?;
        *stage = 3;
        disk.write8(POWER_CONTROL, POWER_3V3);
        disk.write8(TIMEOUT_CONTROL, TIMEOUT_MAXIMUM);
        // A status bit is only latched when its enable bit is set, so polling
        // Command Complete needs this mask; leaving it clear masks every
        // completion and the first command polls forever.
        disk.write16(NORMAL_INT_ENABLE,
                     INT_COMMAND_COMPLETE | INT_TRANSFER_COMPLETE | INT_BUFFER_READ_READY);
        disk.write16(ERROR_INT_ENABLE, ERROR_ALL);
        *stage = 4;
        disk.identify(stage)?;
        *stage = 8;
        Ok(disk)
    }

    fn read32(&self, offset: usize) -> u32
    {
        // SAFETY: `offset` is one of the controller's own register offsets and
        // the window is mapped Device memory for the whole boot.
        unsafe { u32::from_le(read_volatile((self.base + offset) as *const u32)) }
    }

    fn write32(&self, offset: usize, value: u32)
    {
        // SAFETY: as `read32`; the offset is a controller register.
        unsafe { write_volatile((self.base + offset) as *mut u32, value.to_le()) };
    }

    fn read16(&self, offset: usize) -> u16
    {
        // SAFETY: as `read32`.
        unsafe { u16::from_le(read_volatile((self.base + offset) as *const u16)) }
    }

    fn write16(&self, offset: usize, value: u16)
    {
        // SAFETY: as `read32`.
        unsafe { write_volatile((self.base + offset) as *mut u16, value.to_le()) };
    }

    fn write8(&self, offset: usize, value: u8)
    {
        // SAFETY: as `read32`.
        unsafe { write_volatile((self.base + offset) as *mut u8, value) };
    }

    fn reset(&mut self) -> Result<()>
    {
        self.write8(SOFTWARE_RESET, RESET_ALL);
        for _ in 0..TRANSFER_POLLS
        {
            if self.read8(SOFTWARE_RESET) & RESET_ALL == 0
            {
                return Ok(());
            }
        }
        Err(Error::Malformed("sdhci controller did not leave reset"))
    }

    fn read8(&self, offset: usize) -> u8
    {
        // SAFETY: as `read32`.
        unsafe { read_volatile((self.base + offset) as *const u8) }
    }

    /// Base clock in hertz from the capabilities register, falling back to the
    /// 50 MHz the controller defaults to when the field is absent.
    fn base_hz(&self) -> u32
    {
        let megahertz = (self.read32(CAPABILITIES) >> 8) & 0xFF;
        if megahertz == 0 { 50_000_000 } else { megahertz * 1_000_000 }
    }

    /// SDHCI divides the base clock by twice the divider, so the divider is
    /// chosen to land at or below `hz` rather than above it.
    fn set_clock(&mut self, hz: u32) -> Result<()>
    {
        let base = self.base_hz();
        let mut divider = (base / (2 * hz)) as u32;
        if divider == 0
        {
            divider = 1;
        }
        if divider > 0x3FF
        {
            divider = 0x3FF;
        }
        let encoded = ((divider & 0xFF) << 8) | (((divider >> 8) & 0x3) << 6);
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
            // Without a stable internal clock no command reaches the card, so
            // enabling the SD clock anyway would defer the failure to the first
            // command with a misleading timeout.
            return Err(Error::Malformed("sdhci internal clock never stabilised"));
        }
        self.write32(CLOCK_CONTROL,
                     encoded | CLOCK_INTERNAL_ENABLE | CLOCK_INTERNAL_STABLE | CLOCK_SD_ENABLE);
        Ok(())
    }

    fn wait_inhibit(&self) -> Result<()>
    {
        for _ in 0..TRANSFER_POLLS
        {
            if self.read32(PRESENT_STATE) & (PRESENT_COMMAND_INHIBIT | PRESENT_DAT_INHIBIT) == 0
            {
                return Ok(());
            }
        }
        Err(Error::Malformed("sdhci controller stayed inhibited"))
    }

    /// Issues one command and waits for command completion. `data` selects the
    /// data-present path; the caller performs the transfer itself.
    fn command(&mut self, index: u32, argument: u32, response: u16, data: bool)
               -> Result<[u32; 4]>
    {
        self.wait_inhibit()?;
        self.write16(NORMAL_INT_STATUS, 0xFFFF);
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
        // Command index occupies bits 8-13; the response type is bits 0-1.
        let mut flags = (index << 8) | u32::from(response);
        if response != RESPONSE_NONE
        {
            flags |= (1 << 3) | (1 << 4);
        }
        if data
        {
            flags |= 1 << 5;
        }
        // Transfer mode and command are adjacent, so one 32-bit write sets both
        // and issues the command.
        self.write32(TRANSFER_MODE, (flags << 16) | u32::from(transfer));
        for _ in 0..TRANSFER_POLLS
        {
            let status = self.read16(NORMAL_INT_STATUS);
            if status & INT_ERROR != 0
            {
                return Err(Error::Malformed("sdhci command failed"));
            }
            if status & INT_COMMAND_COMPLETE != 0
            {
                return Ok([self.read32(0x10),
                           self.read32(0x14),
                           self.read32(0x18),
                           self.read32(0x1C)]);
            }
        }
        Err(Error::Malformed("sdhci command did not complete"))
    }

    fn identify(&mut self, stage: &mut u8) -> Result<()>
    {
        // GO_IDLE_STATE resets the card, so it must precede every other command;
        // the card ignores or rejects commands sent before it.
        *stage = 5;
        self.command(CMD_GO_IDLE, 0, RESPONSE_NONE, false)?;
        *stage = 6;
        self.command(CMD_SEND_IF_COND, 0x1AA, RESPONSE_48, false)?;
        let mut ready = false;
        for _ in 0..INITIALISE_POLLS
        {
            self.command(CMD_APP_COMMAND, 0, RESPONSE_48, false)?;
            let response = self.command(CMD_APP_SEND_OP_COND, 0x40FF_8000, RESPONSE_48, false)?;
            if response[0] & 0x8000_0000 != 0
            {
                self.high_capacity = response[0] & 0x4000_0000 != 0;
                ready = true;
                break;
            }
        }
        if !ready
        {
            *stage = 7;
            return Err(Error::Malformed("sdhci card did not finish initialising"));
        }
        self.command(CMD_ALL_SEND_CID, 0, RESPONSE_136, false)?;
        let relative = self.command(CMD_SEND_RELATIVE_ADDRESS, 0, RESPONSE_48, false)?;
        self.relative = (relative[0] >> 16) as u16;
        // The card description is readable in the stand-by state; asking for it
        // after SELECT_CARD times out on this controller.
        let csd_response =
            self.command(CMD_SEND_CSD, u32::from(self.relative) << 16, RESPONSE_136, false)?;
        // SDHCI presents a 136-bit response most-significant word last, so the
        // card description reads from RESP3 down to RESP0. Taking RESP0 as the
        // high word yields a structure value no SD card uses.
        let csd = [csd_response[3], csd_response[2], csd_response[1], csd_response[0]];
        self.command(CMD_SELECT_CARD, u32::from(self.relative) << 16, RESPONSE_48_BUSY, false)?;
        self.command(CMD_SET_BLOCK_LENGTH, SECTOR_BYTES as u32, RESPONSE_48, false)?;
        self.sectors = capacity_sectors(csd)?;
        Ok(())
    }

    /// Reads one 512-byte block through the controller's buffer port.
    fn transfer(&mut self, sector: u64) -> Result<[u8; SECTOR_BYTES]>
    {
        let argument = if self.high_capacity { sector as u32 } else { (sector * 512) as u32 };
        self.command(CMD_READ_SINGLE_BLOCK, argument, RESPONSE_48, true)?;
        let mut bytes = [0u8; SECTOR_BYTES];
        let mut words = 0usize;
        for _ in 0..TRANSFER_POLLS
        {
            let status = self.read16(NORMAL_INT_STATUS);
            if status & INT_ERROR != 0
            {
                return Err(Error::Malformed("sdhci read failed"));
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
                return Err(Error::Malformed("sdhci transfer completed short"));
            }
        }
        Err(Error::Malformed("sdhci read did not complete"))
    }
}
impl BlockDevice for Disk
{
    fn sector_count(&self) -> u64
    {
        self.sectors
    }

    fn read_sector(&mut self, sector: u64, output: &mut [u8; SECTOR_BYTES]) -> Result<()>
    {
        if sector >= self.sectors
        {
            return Err(Error::Malformed("sdhci read outside the card"));
        }
        *output = self.transfer(sector)?;
        Ok(())
    }
}

/// Capacity from the card's CSD. Version 2.0 cards report capacity in 512 KiB
/// units; version 1.0 cards report it in bytes with a block length.
fn capacity_sectors(csd: [u32; 4]) -> Result<u64>
{
    // Words are ordered most-significant first, so `csd[0]` holds bits 127:96.
    let structure = (csd[0] >> 30) & 0x3;
    if structure == 1
    {
        // Capacity is reported in 512 KiB units, split across the two words.
        let c_size = (u64::from(csd[1] & 0x3F) << 16) | u64::from((csd[2] >> 16) & 0xFFFF);
        return Ok((c_size + 1) * 1024);
    }
    if structure == 0
    {
        let read_bl_len = (csd[1] >> 16) & 0xF;
        let c_size = ((csd[1] & 0x3FF) << 2) | ((csd[2] >> 30) & 0x3);
        let c_size_mult = (csd[2] >> 15) & 0x7;
        let block = 1u64 << read_bl_len;
        let multiplier = 1u64 << (c_size_mult + 2);
        let bytes = (u64::from(c_size) + 1) * multiplier * block;
        return Ok(bytes / SECTOR_BYTES as u64);
    }
    Err(Error::Malformed(if structure == 2
    {
        "sdhci card reports CSD structure 2, which this reader does not decode"
    }
    else
    {
        "sdhci card reports CSD structure 3, which this reader does not decode"
    }))
}

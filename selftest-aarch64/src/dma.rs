//! The BCM2837 DMA controller: channel claim and control-block transfers.
//!
//! The engine moves bus addresses, not CPU addresses. With the identity map
//! active the CPU address of a RAM buffer is its physical address, and the
//! engine's view of RAM is `0xC0000000 | physical`; the SPI0 FIFO is reached
//! through the VideoCore peripheral aperture instead. Both conversions are
//! explicit here because a wrong one produces a successful transfer to the
//! wrong place, not an error.
use core::ptr::{read_volatile, write_volatile};

use crate::cache;

const CS: usize = 0x00;
const CONBLK_AD: usize = 0x04;
const ENABLE: usize = 0xff0;

const CS_ACTIVE: u32 = 1;
const CS_ERROR: u32 = 1 << 8;
const CS_RESET: u32 = 1 << 31;
/// Wait for outstanding writes, default priority, then start.
const CS_START: u32 = (1 << 28) | (1 << 16) | CS_ACTIVE;

const TI_WAIT_RESP: u32 = 1 << 3;
const TI_DEST_INC: u32 = 1 << 4;
const TI_DEST_DREQ: u32 = 1 << 6;
const TI_SRC_INC: u32 = 1 << 8;
const TI_PERMAP_SHIFT: u32 = 16;
/// SPI0 transmit request, as the engine names it.
const DREQ_SPI_TX: u32 = 6;

const RAM_BUS_BASE: u32 = 0xc000_0000;

/// The eight-word control block the engine reads. It must be 32-byte aligned
/// and reachable at the bus address written to CONBLK_AD.
#[repr(C, align(32))]
pub struct ControlBlock
{
    pub transfer_information: u32,
    pub source:               u32,
    pub destination:          u32,
    pub length:               u32,
    pub stride:               u32,
    pub next:                 u32,
    pub reserved:             [u32; 2]
}

impl ControlBlock
{
    pub fn memory_copy(source: usize, destination: usize, bytes: usize) -> Self
    {
        Self { transfer_information: TI_SRC_INC | TI_DEST_INC | TI_WAIT_RESP,
               source: bus(source),
               destination: bus(destination),
               length: bytes as u32,
               stride: 0,
               next: 0,
               reserved: [0; 2] }
    }

    pub fn spi_write(source: usize, fifo_bus: u32, bytes: usize) -> Self
    {
        Self { transfer_information: (DREQ_SPI_TX << TI_PERMAP_SHIFT)
                                     | TI_SRC_INC
                                     | TI_DEST_DREQ
                                     | TI_WAIT_RESP,
               source: bus(source),
               destination: fifo_bus,
               length: bytes as u32,
               stride: 0,
               next: 0,
               reserved: [0; 2] }
    }
}

/// The engine's address for a RAM buffer.
pub fn bus(address: usize) -> u32
{
    (address as u32 & 0x3fff_ffff) | RAM_BUS_BASE
}

fn read32(address: usize) -> u32
{
    // SAFETY: the caller passes a register in the DMA window declared by the
    // platform record, or a RAM address owned by the caller.
    unsafe { read_volatile(address as *const u32) }
}

fn write32(address: usize, value: u32)
{
    // SAFETY: as `read32`.
    unsafe { write_volatile(address as *mut u32, value) };
}

pub struct Dma
{
    base: usize
}

impl Dma
{
    pub fn new() -> Self
    {
        Self { base: crate::platform::DMA_BASE }
    }

    /// A writable channel register that reads back is the only cheap proof the
    /// block is present; an unimplemented window reads zero.
    pub fn present(&self) -> bool
    {
        write32(self.base + CS, 1 << 16);
        read32(self.base + CS) & (0xf << 16) == (1 << 16)
    }

    /// Takes the lowest channel in the board's ARM set. The firmware keeps the
    /// rest, so a channel outside the mask is never considered.
    pub fn claim(&self, pool: u32) -> Option<Channel>
    {
        for channel in 0..15u32
        {
            if pool & (1 << channel) == 0
            {
                continue;
            }
            let base = self.base + channel as usize * 0x100;
            write32(self.base + ENABLE, read32(self.base + ENABLE) | (1 << channel));
            write32(base + CS, CS_RESET);
            let mut spins = 0u32;
            while read32(base + CS) & CS_RESET != 0 && spins < 1_000_000
            {
                spins += 1;
            }
            if read32(base + CS) & CS_RESET == 0
            {
                return Some(Channel { base,
                                      channel: channel as u8 });
            }
        }
        None
    }
}

pub struct Channel
{
    base:    usize,
    channel: u8
}

impl Channel
{
    pub fn number(&self) -> u8
    {
        self.channel
    }

    pub fn active(&self) -> bool
    {
        read32(self.base + CS) & CS_ACTIVE != 0
    }

    /// Hands the engine a control block and starts it. The block and the
    /// source it names must stay alive and unmodified until `wait` returns.
    pub fn start(&self, block: &ControlBlock) -> Result<(), &'static str>
    {
        if block.length == 0 || block.length & 3 != 0
        {
            return Err("length");
        }
        let address = block as *const ControlBlock as usize;
        cache::clean(address, core::mem::size_of::<ControlBlock>());
        write32(self.base + CONBLK_AD, bus(address));
        write32(self.base + CS, CS_START);
        Ok(())
    }

    pub fn wait(&self) -> Result<(), &'static str>
    {
        let mut spins = 0u32;
        while self.active()
        {
            spins += 1;
            if spins > 50_000_000
            {
                return Err("timeout");
            }
            core::hint::spin_loop();
        }
        if read32(self.base + CS) & CS_ERROR != 0
        {
            return Err("error");
        }
        Ok(())
    }

    /// A memory-to-memory copy. `clean_source` is the switch the cache
    /// experiment needs: false deliberately leaves the CPU's dirty lines in
    /// place so the engine reads memory instead.
    pub fn copy(&self, source: usize, destination: usize, bytes: usize, clean_source: bool)
                -> Result<(), &'static str>
    {
        if clean_source
        {
            cache::clean(source, bytes);
        }
        cache::invalidate(destination, bytes);
        let block = ControlBlock::memory_copy(source, destination, bytes);
        self.start(&block)?;
        self.wait()
    }
}

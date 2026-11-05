//! SPI0 as a serialiser: claim, divider, byte length, DMA pacing and
//! completion.
//!
//! The block is clocked from the core clock; the record's SPI0 window is the
//! ARM view. The DMA engine reaches the FIFO through the VideoCore peripheral
//! aperture, which is a different address for the same register.
use core::ptr::{read_volatile, write_volatile};

use crate::platform;

const CS: usize = 0x00;
const FIFO: usize = 0x04;
const CLK: usize = 0x08;
const DLEN: usize = 0x0c;

const CS_TA: u32 = 1 << 7;
const CS_DMAEN: u32 = 1 << 8;
const CS_REN: u32 = 1 << 12;
const CS_CLEAR_TX: u32 = 1 << 4;
const CS_CLEAR_RX: u32 = 1 << 5;
const CS_DONE: u32 = 1 << 16;
const CS_RXD: u32 = 1 << 17;
const CS_TXD: u32 = 1 << 18;

/// Reset controller register inside the power-management window the record
/// declares, and the bit that holds SPI0.
const PM_RSTC: usize = 0x1c;
const PM_PASSWORD: u32 = 0x5a00_0000;
const SPI0_RESET: u32 = 1 << 22;

/// The engine's view of the FIFO: the VideoCore peripheral aperture, not the
/// ARM window the CPU uses.
pub const FIFO_BUS: u32 = 0x7e00_0000 | (platform::SPI0_BASE as u32 & 0x00ff_ffff) | 0x04;

const POLL_LIMIT: u32 = 2_000_000;

pub struct Spi0
{
    base:       usize,
    shift_bits: u32
}

impl Spi0
{
    pub fn new() -> Self
    {
        Self { base:       platform::SPI0_BASE,
               shift_bits: 0 }
    }

    pub fn shift_bits(&self) -> u32
    {
        self.shift_bits
    }

    /// A writable register that reads back is the cheap presence proof. An
    /// unimplemented window reads zero.
    pub fn present(&self) -> bool
    {
        self.write32(CLK, 1000);
        self.read32(CLK) & 0xffff == 1000
    }

    /// Takes the block out of reset, sets a non-power-of-two divider and reads
    /// it back. The transmit-ready status bit is the evidence the block is
    /// actually running: it is produced by the FIFO, not stored by the CPU.
    pub fn claim(&mut self) -> Result<(), &'static str>
    {
        let value = self.read32_pm(PM_RSTC);
        if value & SPI0_RESET != 0
        {
            self.write32_pm(PM_RSTC, PM_PASSWORD | (value & !SPI0_RESET));
            let mut spins = 0u32;
            while self.read32_pm(PM_RSTC) & SPI0_RESET != 0 && spins < 1_000_000
            {
                spins += 1;
            }
            if self.read32_pm(PM_RSTC) & SPI0_RESET != 0
            {
                return Err("reset");
            }
        }
        self.clear_fifos();
        self.write32(CLK, 1000);
        if self.read32(CLK) & 0xffff != 1000
        {
            return Err("clk-readback");
        }
        if self.read_cs() & CS_TXD == 0
        {
            return Err("no-txd");
        }
        Ok(())
    }

    /// Sends eight bytes and counts what came back. The count reveals whether
    /// the block shifts 8 bits or 32 bits per FIFO entry, which is the fact the
    /// "n words is 4n bytes" rule depends on. Eight entries cannot overflow the
    /// transmit FIFO, so both widths complete without a stall.
    pub fn probe_shift(&mut self) -> Result<usize, &'static str>
    {
        self.clear_fifos();
        self.write32(CLK, 1000);
        self.write32(DLEN, 8);
        self.write_cs(CS_REN);
        self.write_cs(CS_REN | CS_TA);
        let mut received = 0usize;
        for index in 0..8
        {
            self.wait_txd()?;
            self.write32(FIFO, 0xa5a5_0000 | index);
            // Drain as the block fills the receive FIFO. Waiting until the end
            // would let a 16-word receive FIFO stall the shift register.
            while received < 16 && self.read_cs() & CS_RXD != 0
            {
                self.read32(FIFO);
                received += 1;
            }
        }
        self.wait_done()?;
        received += self.drain_rx(16);
        self.finish();
        self.shift_bits = match received
        {
            8 => 8,
            2 => 32,
            _ => 0
        };
        Ok(received)
    }

    /// A CPU-fed transfer of `bytes` at `divider`. Returns the received word
    /// count and the measured duration in timer ticks.
    pub fn transfer(&mut self, bytes: usize, divider: u16) -> Result<(usize, u64), &'static str>
    {
        if self.shift_bits == 0
        {
            return Err("no-shift");
        }
        let entries = if self.shift_bits == 8 { bytes } else { bytes / 4 };
        self.clear_fifos();
        self.write32(CLK, u32::from(divider));
        self.write32(DLEN, bytes as u32);
        self.write_cs(CS_REN);
        let start = crate::timer();
        self.write_cs(CS_REN | CS_TA);
        let mut received = 0usize;
        for index in 0..entries
        {
            self.wait_txd()?;
            self.write32(FIFO, 0xa5a5_0000 | index as u32);
            // Keep the receive FIFO moving: a full one stalls the shift
            // register and looks exactly like a transfer that never started.
            while self.read_cs() & CS_RXD != 0
            {
                self.read32(FIFO);
                received += 1;
            }
        }
        self.wait_done()?;
        let end = crate::timer();
        received += self.drain_rx(entries);
        self.finish();
        Ok((received, end.wrapping_sub(start)))
    }

    /// Prepares a DMA-fed transfer. The engine is started first, then `start`
    /// sets transfer-active; the other order leaves the block waiting.
    pub fn prepare_dma(&self, bytes: usize, divider: u16) -> Result<(), &'static str>
    {
        self.clear_fifos();
        self.write32(CLK, u32::from(divider));
        self.write32(DLEN, bytes as u32);
        self.write_cs(CS_DMAEN);
        Ok(())
    }

    pub fn start(&self)
    {
        self.write_cs(self.read_cs() | CS_TA);
    }

    pub fn busy(&self) -> bool
    {
        self.read_cs() & CS_TA != 0
    }

    pub fn wait_done(&self) -> Result<(), &'static str>
    {
        let mut spins = 0u32;
        while self.read_cs() & CS_DONE == 0
        {
            spins += 1;
            if spins > POLL_LIMIT
            {
                return Err("timeout");
            }
        }
        Ok(())
    }

    pub fn finish(&self)
    {
        self.write_cs(self.read_cs() & !CS_TA);
        self.clear_fifos();
    }

    fn clear_fifos(&self)
    {
        // No chip select is asserted here: the FIFO clear must not happen with
        // one selected. Done is write-one-to-clear, so the same write retires a
        // previous transfer's completion flag before the next one starts.
        self.write_cs(CS_CLEAR_TX | CS_CLEAR_RX | CS_DONE);
    }

    fn wait_txd(&self) -> Result<(), &'static str>
    {
        let mut spins = 0u32;
        while self.read_cs() & CS_TXD == 0
        {
            spins += 1;
            if spins > POLL_LIMIT
            {
                return Err("tx-full");
            }
        }
        Ok(())
    }

    fn drain_rx(&self, limit: usize) -> usize
    {
        let mut count = 0usize;
        while count < limit && self.read_cs() & CS_RXD != 0
        {
            self.read32(FIFO);
            count += 1;
        }
        count
    }

    fn read_cs(&self) -> u32
    {
        self.read32(CS)
    }

    fn write_cs(&self, value: u32)
    {
        self.write32(CS, value);
    }

    fn read32(&self, offset: usize) -> u32
    {
        // SAFETY: `offset` is one of the SPI0 registers and the window comes
        // from the platform record.
        unsafe { read_volatile((self.base + offset) as *const u32) }
    }

    fn write32(&self, offset: usize, value: u32)
    {
        // SAFETY: as `read32`.
        unsafe { write_volatile((self.base + offset) as *mut u32, value) };
    }

    fn read32_pm(&self, offset: usize) -> u32
    {
        // SAFETY: the power-management window comes from the platform record.
        unsafe { read_volatile((platform::PM_BASE + offset) as *const u32) }
    }

    fn write32_pm(&self, offset: usize, value: u32)
    {
        // SAFETY: as `read32_pm`.
        unsafe { write_volatile((platform::PM_BASE + offset) as *mut u32, value) };
    }
}

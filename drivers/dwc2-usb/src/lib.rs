//! The DesignWare Hi-Speed USB 2.0 OTG controller ("DWC2"), driven as a host.
//!
//! **This is the start of a host stack, not one.** It proves the controller is
//! the core it is taken for, puts it in host mode, powers the root port, and
//! talks to what is on it with control transfers: it reads the device's
//! descriptor, gives it an address, and - where it is a hub, as it is on the
//! board this was written against - configures it, powers its ports and reads
//! what each one sees. Nothing is done with the devices behind the hub yet:
//! reaching a low- or full-speed one takes split transactions, which are not
//! here.
//!
//! The registers and their bits are from the Synopsys core's programming
//! model. Three facts shape the code:
//!
//! * **The core identifies itself.** `GSNPSID` reads `0x4F54` in its top half
//!   - "OT" - followed by the release. A window that answers anything else is
//!   not this core, or is a core whose power domain is off, and nothing further
//!   is written to it.
//! * **The port register mixes control bits with write-one-to-clear status
//!   bits, and one of those is the port enable.** Writing back what was read
//!   disables the port. Every write to `HPRT` therefore goes through
//!   [`port_control`], which strips the status bits first.
//! * **Nothing here may wait forever.** A controller that never finishes its
//!   reset, or never reports host mode, is a refusal with a name. Each wait is
//!   bounded by the clock and, behind it, by a count, so a clock that stops is
//!   not an endless loop either.
//!
//! The decisions live here as safe code over [`Registers`] and [`Clock`]; the
//! claim, the mapping and the volatile access live in the one audited adapter.

#![no_std]
#![deny(unsafe_code)]

use core::fmt;

pub const GAHBCFG: usize = 0x008;
pub const GUSBCFG: usize = 0x00c;
pub const GRSTCTL: usize = 0x010;
pub const GINTSTS: usize = 0x014;
pub const GINTMSK: usize = 0x018;
pub const GSNPSID: usize = 0x040;
pub const GHWCFG1: usize = 0x044;
pub const GHWCFG2: usize = 0x048;
pub const GHWCFG3: usize = 0x04c;
pub const GHWCFG4: usize = 0x050;
pub const GRXFSIZ: usize = 0x024;
pub const GNPTXFSIZ: usize = 0x028;
pub const HPTXFSIZ: usize = 0x100;
pub const HCFG: usize = 0x400;
/// Host channel 0's registers. Each channel is 0x20 bytes; one is all a
/// single control transfer needs.
pub const HCCHAR: usize = 0x500;
pub const HCSPLT: usize = 0x504;
pub const HCINT: usize = 0x508;
pub const HCINTMSK: usize = 0x50c;
pub const HCTSIZ: usize = 0x510;
pub const HCDMA: usize = 0x514;
pub const HPRT: usize = 0x440;
pub const PCGCCTL: usize = 0xe00;
/// The extent this driver reaches: through the power and clock gating word.
pub const REQUIRED_BYTES: u64 = 0xe04;
/// One page is what the adapter asks System for as the transfer buffer.
pub const TRANSFER_BUFFER_BYTES: usize = 4096;

/// `GAHBCFG`: the core's one interrupt line is gated by bit 0.
pub const GLOBAL_INTERRUPT: u32 = 1 << 0;
/// `GAHBCFG`: let the core move transfer data itself, to and from memory.
pub const DMA_ENABLE: u32 = 1 << 5;
/// `GAHBCFG` bit 4 on this integration: finish every bus write before
/// signalling completion. The board's vendor sets it; without it a completed
/// transfer's last bytes may not be in memory yet.
pub const AXI_WRITE_WAIT: u32 = 1 << 4;
/// `GHWCFG2` bits 4:3: 2 is a core with its own DMA engine.
pub const ARCHITECTURE_SHIFT: u32 = 3;
pub const ARCHITECTURE_MASK: u32 = 0b11 << ARCHITECTURE_SHIFT;
pub const ARCHITECTURE_INTERNAL_DMA: u32 = 2 << ARCHITECTURE_SHIFT;
/// `GRSTCTL`: flush the receive FIFO, or the transmit FIFO numbered in bits
/// 10:6 (0x10 is "all of them"). Both bits clear themselves.
pub const RX_FIFO_FLUSH: u32 = 1 << 4;
pub const TX_FIFO_FLUSH: u32 = 1 << 5;
pub const TX_FIFO_ALL: u32 = 0x10 << 6;
/// FIFO depths in 32-bit words. The core reports 4080 words of FIFO memory on
/// the board this was written against; these three total 3072.
pub const RX_FIFO_WORDS: u32 = 1024;
pub const NON_PERIODIC_TX_FIFO_WORDS: u32 = 1024;
pub const PERIODIC_TX_FIFO_WORDS: u32 = 1024;

/// `HCCHAR` fields.
pub const CHANNEL_ENABLE: u32 = 1 << 31;
pub const CHANNEL_DISABLE: u32 = 1 << 30;
pub const CHANNEL_ADDRESS_SHIFT: u32 = 22;
pub const CHANNEL_ONE_TRANSACTION: u32 = 1 << 20;
pub const CHANNEL_DIRECTION_IN: u32 = 1 << 15;
pub const CHANNEL_ENDPOINT_SHIFT: u32 = 11;
/// `HCTSIZ` fields: the packet identifier, the packet count, the byte count.
pub const PID_SHIFT: u32 = 29;
pub const PID_DATA1: u32 = 2;
pub const PID_SETUP: u32 = 3;
pub const PACKET_COUNT_SHIFT: u32 = 19;
pub const TRANSFER_SIZE_MASK: u32 = 0x7_ffff;
/// `HCINT` bits.
pub const TRANSFER_COMPLETE: u32 = 1 << 0;
pub const CHANNEL_HALTED: u32 = 1 << 1;
pub const NAK: u32 = 1 << 4;

/// The packet size every device accepts on its control endpoint before it has
/// said otherwise; a high-speed device's is exactly this.
pub const DEFAULT_PACKET: u32 = 64;
pub const DEVICE_DESCRIPTOR_BYTES: usize = 18;
/// GET_DESCRIPTOR(device), eighteen bytes, as the eight-byte SETUP packet.
pub const GET_DEVICE_DESCRIPTOR: [u8; 8] = [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 0x12, 0x00];
/// How long one stage of a control transfer is given, and how many times a
/// device may answer "not yet" before that is the answer.
pub const STAGE_MICROSECONDS: u64 = 500_000;
pub const NAK_LIMIT: u32 = 200;

/// `GUSBCFG`: force the core's role regardless of the ID pin.
pub const FORCE_HOST: u32 = 1 << 29;
pub const FORCE_DEVICE: u32 = 1 << 30;
/// `GRSTCTL`: soft reset (self-clearing) and the bus-idle indication.
pub const CORE_SOFT_RESET: u32 = 1 << 0;
pub const AHB_IDLE: u32 = 1 << 31;
/// `GINTSTS` bit 0: set while the core is operating as a host.
pub const CURRENT_MODE_HOST: u32 = 1 << 0;

/// `HPRT` bits.
pub const PORT_CONNECTED: u32 = 1 << 0;
pub const PORT_CONNECT_DETECTED: u32 = 1 << 1;
pub const PORT_ENABLED: u32 = 1 << 2;
pub const PORT_ENABLE_CHANGED: u32 = 1 << 3;
pub const PORT_OVERCURRENT_CHANGED: u32 = 1 << 5;
pub const PORT_RESET: u32 = 1 << 8;
pub const PORT_POWER: u32 = 1 << 12;
pub const PORT_SPEED_SHIFT: u32 = 17;
pub const PORT_SPEED_MASK: u32 = 0b11 << PORT_SPEED_SHIFT;
/// The bits of `HPRT` a write of one clears - including the enable.
pub const PORT_WRITE_CLEARS: u32 =
    PORT_CONNECT_DETECTED | PORT_ENABLED | PORT_ENABLE_CHANGED | PORT_OVERCURRENT_CHANGED;

/// "OT" in the top half of `GSNPSID`; the low half is the release.
pub const CORE_ID: u32 = 0x4f54_0000;
pub const CORE_ID_MASK: u32 = 0xffff_0000;

/// How long each stage is given. The reset and the role change are documented
/// as taking milliseconds; these are several times that, as refusal
/// thresholds rather than tuned delays.
pub const RESET_MICROSECONDS: u64 = 100_000;
pub const HOST_MODE_MICROSECONDS: u64 = 200_000;
/// A device is given this long after power to pull its line.
pub const ATTACH_MICROSECONDS: u64 = 300_000;
/// USB 2.0 asks a root port to hold reset for at least 50 ms.
pub const PORT_RESET_MICROSECONDS: u64 = 60_000;
pub const PORT_RECOVERY_MICROSECONDS: u64 = 20_000;
/// Behind every clock bound: polls after which a wait gives up regardless.
pub const POLL_LIMIT: u32 = 50_000_000;

pub trait Registers
{
    fn read(&mut self, offset: usize) -> u32;
    fn write(&mut self, offset: usize, value: u32);
}

/// One buffer the controller reads and writes by itself, at an address it is
/// given. The adapter backs it with storage no cache holds a copy of, so
/// neither side needs to synchronize it.
pub trait TransferBuffer
{
    /// The buffer's address as the controller uses it.
    fn address(&self) -> u32;
    fn capacity(&self) -> usize;
    fn fill(&mut self, bytes: &[u8]);
    fn copy_out(&self, bytes: &mut [u8]);
}

/// Monotonic time, or `None` when the clock refused a reading.
pub trait Clock
{
    fn microseconds(&mut self) -> Option<u64>;
}

/// What the core says it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity
{
    pub id:       u32,
    pub hardware: [u32; 4]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speed
{
    High,
    Full,
    Low,
    Unknown(u32)
}

/// What the root port sees after power and one reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Port
{
    Empty
    {
        port: u32
    },
    Attached
    {
        speed:   Speed,
        enabled: bool,
        port:    u32
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal
{
    /// The window does not answer as this core. Also what a core whose power
    /// domain is off looks like.
    NotThisCore
    {
        id: u32
    },
    /// The core's bus never went idle, so a reset could not be started.
    BusBusy
    {
        control: u32
    },
    /// The soft reset bit never cleared.
    ResetStuck
    {
        control: u32
    },
    /// The core never reported host mode after being told to take it.
    NotHost
    {
        status: u32
    },
    /// The clock refused a reading, so no wait could be bounded by it.
    Clock,
    /// The core has no DMA engine of its own, which is the only way this
    /// driver moves data.
    NoDma
    {
        hardware: u32
    },
    /// A FIFO flush never completed.
    FlushStuck
    {
        control: u32
    },
    /// A stage of a control transfer did not finish in its time.
    StageTimeout
    {
        stage:  Stage,
        status: u32
    },
    /// A stage halted without completing: a stall, a transaction error, or
    /// too many "not yet" answers. `status` is the channel's interrupt word.
    StageFailed
    {
        stage:  Stage,
        status: u32
    },
    /// The data stage completed with fewer bytes than a device descriptor.
    ShortDescriptor
    {
        bytes: usize
    }
}

/// The three stages of a control transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage
{
    Setup,
    Data,
    Status
}

impl fmt::Display for Refusal
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::NotThisCore { id } =>
            {
                write!(f, "the window identifies as {id:#010x}, not a DWC2 core (or it is unpowered)")
            }
            Self::BusBusy { control } => write!(f, "the core's bus never went idle (GRSTCTL={control:#010x})"),
            Self::ResetStuck { control } =>
            {
                write!(f, "the core's soft reset never completed (GRSTCTL={control:#010x})")
            }
            Self::NotHost { status } =>
            {
                write!(f, "the core never entered host mode (GINTSTS={status:#010x})")
            }
            Self::Clock => write!(f, "the clock refused a reading"),
            Self::NoDma { hardware } =>
            {
                write!(f, "the core has no internal DMA engine (GHWCFG2={hardware:#010x})")
            }
            Self::FlushStuck { control } =>
            {
                write!(f, "a FIFO flush never completed (GRSTCTL={control:#010x})")
            }
            Self::StageTimeout { stage,
                                 status } =>
            {
                write!(f, "the {stage:?} stage did not finish (HCINT={status:#010x})")
            }
            Self::StageFailed { stage,
                                status } =>
            {
                write!(f, "the {stage:?} stage halted without completing (HCINT={status:#010x})")
            }
            Self::ShortDescriptor { bytes } =>
            {
                write!(f, "the device answered with {bytes} bytes, fewer than a device descriptor")
            }
        }
    }
}

/// Wait until `done` holds or `microseconds` pass. `Ok(true)` is `done`.
fn wait(registers: &mut impl Registers, clock: &mut impl Clock, microseconds: u64,
        mut done: impl FnMut(&mut dyn FnMut(usize) -> u32) -> bool)
        -> Result<bool, Refusal>
{
    let start = clock.microseconds().ok_or(Refusal::Clock)?;
    for _ in 0..POLL_LIMIT
    {
        if done(&mut |offset| registers.read(offset))
        {
            return Ok(true);
        }
        let now = clock.microseconds().ok_or(Refusal::Clock)?;
        if now.saturating_sub(start) >= microseconds
        {
            return Ok(false);
        }
    }
    Ok(false)
}

/// Let `microseconds` pass.
fn pause(registers: &mut impl Registers, clock: &mut impl Clock, microseconds: u64)
         -> Result<(), Refusal>
{
    wait(registers, clock, microseconds, |_| false).map(|_| ())
}

/// The value to write to `HPRT` to change control bits: what was read, without
/// the bits a write of one would clear.
pub const fn port_control(read: u32) -> u32
{
    read & !PORT_WRITE_CLEARS
}

pub const fn speed(port: u32) -> Speed
{
    match (port & PORT_SPEED_MASK) >> PORT_SPEED_SHIFT
    {
        0 => Speed::High,
        1 => Speed::Full,
        2 => Speed::Low,
        other => Speed::Unknown(other)
    }
}

/// Prove the window is a DWC2 core and read what it was built with. Reads only.
pub fn identify(registers: &mut impl Registers) -> Result<Identity, Refusal>
{
    let id = registers.read(GSNPSID);
    if id & CORE_ID_MASK != CORE_ID
    {
        return Err(Refusal::NotThisCore { id });
    }
    Ok(Identity { id,
                  hardware: [registers.read(GHWCFG1),
                             registers.read(GHWCFG2),
                             registers.read(GHWCFG3),
                             registers.read(GHWCFG4)] })
}

/// Reset the core and make it a host, with its interrupt line gated.
///
/// This driver polls, so the line stays gated and every source masked: an
/// interrupt nobody handles is worse than one that never fires.
pub fn start_host(registers: &mut impl Registers, clock: &mut impl Clock) -> Result<(), Refusal>
{
    let bus = registers.read(GAHBCFG);
    registers.write(GAHBCFG, bus & !GLOBAL_INTERRUPT);
    registers.write(GINTMSK, 0);
    if !wait(registers, clock, RESET_MICROSECONDS, |read| read(GRSTCTL) & AHB_IDLE != 0)?
    {
        return Err(Refusal::BusBusy { control: registers.read(GRSTCTL) });
    }
    let control = registers.read(GRSTCTL);
    registers.write(GRSTCTL, control | CORE_SOFT_RESET);
    if !wait(registers, clock, RESET_MICROSECONDS, |read| {
           let control = read(GRSTCTL);
           control & CORE_SOFT_RESET == 0 && control & AHB_IDLE != 0
       })?
    {
        return Err(Refusal::ResetStuck { control: registers.read(GRSTCTL) });
    }
    // The reset put the interrupt gate back to its default; gate it again.
    let bus = registers.read(GAHBCFG);
    registers.write(GAHBCFG, bus & !GLOBAL_INTERRUPT);
    registers.write(GINTMSK, 0);
    // Un-gate the PHY clock before asking for a role: a gated core does not
    // change mode.
    registers.write(PCGCCTL, 0);
    let configuration = registers.read(GUSBCFG);
    registers.write(GUSBCFG, (configuration & !FORCE_DEVICE) | FORCE_HOST);
    if !wait(registers, clock, HOST_MODE_MICROSECONDS, |read| {
           read(GINTSTS) & CURRENT_MODE_HOST != 0
       })?
    {
        return Err(Refusal::NotHost { status: registers.read(GINTSTS) });
    }
    Ok(())
}

/// Power the root port, and if something is attached reset it once and read
/// its speed. A port with nothing on it is an answer, not a refusal.
pub fn power_port(registers: &mut impl Registers, clock: &mut impl Clock) -> Result<Port, Refusal>
{
    let port = registers.read(HPRT);
    registers.write(HPRT, port_control(port) | PORT_POWER);
    if !wait(registers, clock, ATTACH_MICROSECONDS, |read| read(HPRT) & PORT_CONNECTED != 0)?
    {
        return Ok(Port::Empty { port: registers.read(HPRT) });
    }
    // Acknowledge the connect, then reset the port so the device and the core
    // agree on a speed.
    let port = registers.read(HPRT);
    registers.write(HPRT, port_control(port) | PORT_CONNECT_DETECTED);
    let port = registers.read(HPRT);
    registers.write(HPRT, port_control(port) | PORT_RESET);
    pause(registers, clock, PORT_RESET_MICROSECONDS)?;
    let port = registers.read(HPRT);
    registers.write(HPRT, port_control(port) & !PORT_RESET);
    pause(registers, clock, PORT_RECOVERY_MICROSECONDS)?;
    let port = registers.read(HPRT);
    Ok(Port::Attached { speed: speed(port),
                        enabled: port & PORT_ENABLED != 0,
                        port })
}

/// Size the FIFOs, empty them, and let the core move transfer data itself.
/// Done once, after the port is up and before the first transfer.
pub fn prepare_transfers(registers: &mut impl Registers, clock: &mut impl Clock,
                         identity: &Identity)
                         -> Result<(), Refusal>
{
    if identity.hardware[1] & ARCHITECTURE_MASK != ARCHITECTURE_INTERNAL_DMA
    {
        return Err(Refusal::NoDma { hardware: identity.hardware[1] });
    }
    registers.write(GRXFSIZ, RX_FIFO_WORDS);
    registers.write(GNPTXFSIZ, NON_PERIODIC_TX_FIFO_WORDS << 16 | RX_FIFO_WORDS);
    registers.write(HPTXFSIZ,
                    PERIODIC_TX_FIFO_WORDS << 16 | (RX_FIFO_WORDS + NON_PERIODIC_TX_FIFO_WORDS));
    for flush in [TX_FIFO_FLUSH | TX_FIFO_ALL, RX_FIFO_FLUSH]
    {
        registers.write(GRSTCTL, flush);
        if !wait(registers, clock, RESET_MICROSECONDS, |read| {
               read(GRSTCTL) & (TX_FIFO_FLUSH | RX_FIFO_FLUSH) == 0
           })?
        {
            return Err(Refusal::FlushStuck { control: registers.read(GRSTCTL) });
        }
    }
    // The interrupt line stays gated; only the data path is switched on.
    let bus = registers.read(GAHBCFG);
    registers.write(GAHBCFG, (bus & !GLOBAL_INTERRUPT) | DMA_ENABLE | AXI_WRITE_WAIT);
    // This driver reads the channel's status word itself and enables none of
    // its interrupt sources.
    registers.write(HCINTMSK, 0);
    Ok(())
}

/// Run one stage on channel 0, to endpoint 0 of the device at `device`.
/// Returns the bytes the stage left untransferred.
#[allow(clippy::too_many_arguments)]
fn stage(registers: &mut impl Registers, clock: &mut impl Clock, stage: Stage, device: u8,
         address: u32, bytes: u32, inward: bool, pid: u32)
         -> Result<u32, Refusal>
{
    let direction = if inward { CHANNEL_DIRECTION_IN } else { 0 };
    // Endpoint 0, type control (zero), one transaction per microframe.
    let characteristics = DEFAULT_PACKET
                          | direction
                          | CHANNEL_ONE_TRANSACTION
                          | u32::from(device & 0x7f) << CHANNEL_ADDRESS_SHIFT;
    // A stage is whole packets; a zero-length one is still one packet.
    let packets = bytes.div_ceil(DEFAULT_PACKET).max(1);
    for _ in 0..NAK_LIMIT
    {
        registers.write(HCINT, u32::MAX);
        registers.write(HCSPLT, 0);
        registers.write(HCTSIZ, bytes | packets << PACKET_COUNT_SHIFT | pid << PID_SHIFT);
        registers.write(HCDMA, address);
        registers.write(HCCHAR, characteristics | CHANNEL_ENABLE);
        // With its own DMA the core halts the channel when the stage ends,
        // whichever way it ends, so the halt is the one thing to wait for.
        if !wait(registers, clock, STAGE_MICROSECONDS, |read| read(HCINT) & CHANNEL_HALTED != 0)?
        {
            let status = registers.read(HCINT);
            // Ask the core to stop the channel and give it the same time to
            // say it has, so the next use does not start on a live channel.
            registers.write(HCCHAR, characteristics | CHANNEL_ENABLE | CHANNEL_DISABLE);
            let _ = wait(registers, clock, STAGE_MICROSECONDS, |read| {
                read(HCINT) & CHANNEL_HALTED != 0
            })?;
            return Err(Refusal::StageTimeout { stage,
                                               status });
        }
        let status = registers.read(HCINT);
        if status & TRANSFER_COMPLETE != 0
        {
            return Ok(registers.read(HCTSIZ) & TRANSFER_SIZE_MASK);
        }
        if status & NAK == 0
        {
            return Err(Refusal::StageFailed { stage,
                                              status });
        }
    }
    Err(Refusal::StageFailed { stage,
                               status: NAK })
}

/// One control transfer to endpoint 0 of the device at `device`: the SETUP
/// packet, then `reply.len()` bytes in if any are asked for, then the status
/// stage in the opposite direction. Returns how many bytes arrived.
///
/// Only requests that carry data toward the host, or none, are needed so far,
/// so there is no data-out form.
pub fn control(registers: &mut impl Registers, clock: &mut impl Clock,
               buffer: &mut impl TransferBuffer, device: u8, setup: [u8; 8], reply: &mut [u8])
               -> Result<usize, Refusal>
{
    debug_assert!(reply.len() <= buffer.capacity());
    buffer.fill(&setup);
    stage(registers, clock, Stage::Setup, device, buffer.address(), 8, false, PID_SETUP)?;
    if reply.is_empty()
    {
        // No data: the device acknowledges with a zero-length packet in.
        stage(registers, clock, Stage::Status, device, buffer.address(), 0, true, PID_DATA1)?;
        return Ok(0);
    }
    // Whole packets are asked for; the device ends the stage early with a
    // short one when it has less to say.
    let asked = (reply.len() as u32).div_ceil(DEFAULT_PACKET) * DEFAULT_PACKET;
    let left =
        stage(registers, clock, Stage::Data, device, buffer.address(), asked, true, PID_DATA1)?;
    let received = (asked.saturating_sub(left) as usize).min(reply.len());
    buffer.copy_out(&mut reply[..received]);
    stage(registers, clock, Stage::Status, device, buffer.address(), 0, false, PID_DATA1)?;
    Ok(received)
}

/// Ask the device at `device` for its device descriptor.
pub fn device_descriptor(registers: &mut impl Registers, clock: &mut impl Clock,
                         buffer: &mut impl TransferBuffer, device: u8)
                         -> Result<[u8; DEVICE_DESCRIPTOR_BYTES], Refusal>
{
    let mut descriptor = [0; DEVICE_DESCRIPTOR_BYTES];
    let received =
        control(registers, clock, buffer, device, GET_DEVICE_DESCRIPTOR, &mut descriptor)?;
    if received < DEVICE_DESCRIPTOR_BYTES
    {
        return Err(Refusal::ShortDescriptor { bytes: received });
    }
    Ok(descriptor)
}

/// The address this driver gives the one device on the root port.
pub const ROOT_DEVICE_ADDRESS: u8 = 1;
/// The device class code of a hub.
pub const CLASS_HUB: u8 = 9;
/// The most downstream ports this reads; a hub states its own count.
pub const HUB_PORTS: usize = 8;
/// A device is given this long to start answering at a new address, and a
/// hub's ports this long beyond the hub's own figure to settle after power.
pub const ADDRESS_SETTLE_MICROSECONDS: u64 = 10_000;
pub const PORT_SETTLE_MICROSECONDS: u64 = 100_000;

/// Hub port status bits, from the hub class.
pub const HUB_PORT_CONNECTED: u16 = 1 << 0;
pub const HUB_PORT_ENABLED: u16 = 1 << 1;
pub const HUB_PORT_POWERED: u16 = 1 << 8;
pub const HUB_PORT_LOW_SPEED: u16 = 1 << 9;
pub const HUB_PORT_HIGH_SPEED: u16 = 1 << 10;

/// What a hub said about itself and its ports.
#[derive(Clone, Copy, Debug)]
pub struct Hub
{
    pub ports:              usize,
    pub power_good_ms:      u32,
    /// Each port's status and change words, in port order from port 1.
    pub status:             [(u16, u16); HUB_PORTS]
}

/// Move the device answering at the default address to `address`. Only one
/// device may be at the default address at a time, which is the caller's to
/// arrange: one port is reset, and its device addressed, before the next.
pub fn assign_address(registers: &mut impl Registers, clock: &mut impl Clock,
                      buffer: &mut impl TransferBuffer, address: u8)
                      -> Result<(), Refusal>
{
    control(registers, clock, buffer, 0, [0x00, 0x05, address & 0x7f, 0, 0, 0, 0, 0], &mut [])?;
    pause(registers, clock, ADDRESS_SETTLE_MICROSECONDS)
}

/// Hub port change bits, and the feature numbers that drive a port.
pub const HUB_CHANGE_RESET: u16 = 1 << 4;
const FEATURE_PORT_RESET: u8 = 4;
const FEATURE_C_PORT_CONNECTION: u8 = 16;
const FEATURE_C_PORT_RESET: u8 = 20;
/// A hub is asked this often, and at most this many times, whether a port
/// reset it was told to perform has finished.
pub const HUB_RESET_POLL_MICROSECONDS: u64 = 20_000;
pub const HUB_RESET_POLLS: u32 = 25;
/// What a device is given after its port's reset before it is spoken to.
pub const RESET_RECOVERY_MICROSECONDS: u64 = 20_000;

/// One port's status and change words.
pub fn hub_port_status(registers: &mut impl Registers, clock: &mut impl Clock,
                       buffer: &mut impl TransferBuffer, hub: u8, port: u8)
                       -> Result<(u16, u16), Refusal>
{
    let mut words = [0u8; 4];
    let received =
        control(registers, clock, buffer, hub, [0xa3, 0x00, 0, 0, port, 0, 4, 0], &mut words)?;
    if received < 4
    {
        return Err(Refusal::ShortDescriptor { bytes: received });
    }
    Ok((u16::from_le_bytes([words[0], words[1]]), u16::from_le_bytes([words[2], words[3]])))
}

/// Have the hub reset one port and wait for it to say the reset is over.
///
/// A port's speed is only known after this: the hub works it out during the
/// reset, and the speed bits in a status read before it mean nothing. The
/// status returned is the one read after, with the change bits acknowledged.
pub fn reset_hub_port(registers: &mut impl Registers, clock: &mut impl Clock,
                      buffer: &mut impl TransferBuffer, hub: u8, port: u8)
                      -> Result<(u16, u16), Refusal>
{
    let feature = |set: bool, feature: u8| {
        [0x23, if set { 0x03 } else { 0x01 }, feature, 0, port, 0, 0, 0]
    };
    control(registers, clock, buffer, hub, feature(true, FEATURE_PORT_RESET), &mut [])?;
    let mut last = (0, 0);
    for _ in 0..HUB_RESET_POLLS
    {
        pause(registers, clock, HUB_RESET_POLL_MICROSECONDS)?;
        last = hub_port_status(registers, clock, buffer, hub, port)?;
        if last.1 & HUB_CHANGE_RESET != 0
        {
            break;
        }
    }
    control(registers, clock, buffer, hub, feature(false, FEATURE_C_PORT_RESET), &mut [])?;
    control(registers, clock, buffer, hub, feature(false, FEATURE_C_PORT_CONNECTION), &mut [])?;
    pause(registers, clock, RESET_RECOVERY_MICROSECONDS)?;
    Ok(last)
}

/// Configure the hub at `device`, power every port, and read each port's
/// status once the hub's own power-good time has passed.
///
/// It selects configuration 1 without reading the configuration descriptor,
/// which is the only one a hub has. It does not reset or enable any port:
/// what is attached is reported, and left alone.
pub fn start_hub(registers: &mut impl Registers, clock: &mut impl Clock,
                 buffer: &mut impl TransferBuffer, device: u8)
                 -> Result<Hub, Refusal>
{
    // SET_CONFIGURATION(1).
    control(registers, clock, buffer, device, [0x00, 0x09, 1, 0, 0, 0, 0, 0], &mut [])?;
    // GET_DESCRIPTOR(hub): port count in byte 2, power-on-to-power-good in
    // byte 5, in units of two milliseconds.
    let mut descriptor = [0u8; 9];
    let received = control(registers,
                           clock,
                           buffer,
                           device,
                           [0xa0, 0x06, 0x00, 0x29, 0, 0, 9, 0],
                           &mut descriptor)?;
    if received < 7
    {
        return Err(Refusal::ShortDescriptor { bytes: received });
    }
    let ports = usize::from(descriptor[2]).min(HUB_PORTS);
    let power_good_ms = u32::from(descriptor[5]) * 2;
    for port in 1..=ports
    {
        // SET_FEATURE(PORT_POWER) on each port.
        control(registers,
                clock,
                buffer,
                device,
                [0x23, 0x03, 8, 0, port as u8, 0, 0, 0],
                &mut [])?;
    }
    pause(registers, clock, u64::from(power_good_ms) * 1000 + PORT_SETTLE_MICROSECONDS)?;
    let mut hub = Hub { ports,
                        power_good_ms,
                        status: [(0, 0); HUB_PORTS] };
    for port in 1..=ports
    {
        // GET_STATUS(port): two little-endian words, status then change.
        let mut words = [0u8; 4];
        let received = control(registers,
                               clock,
                               buffer,
                               device,
                               [0xa3, 0x00, 0, 0, port as u8, 0, 4, 0],
                               &mut words)?;
        if received < 4
        {
            return Err(Refusal::ShortDescriptor { bytes: received });
        }
        hub.status[port - 1] = (u16::from_le_bytes([words[0], words[1]]),
                                u16::from_le_bytes([words[2], words[3]]));
    }
    Ok(hub)
}

#[cfg(target_os = "none")]
mod native;
#[cfg(target_os = "none")]
pub use native::DRIVER;

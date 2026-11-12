//! The DesignWare Hi-Speed USB 2.0 OTG controller ("DWC2"), driven as a host.
//!
//! **This is the first step of a host stack and does no transfers.** It proves
//! the controller is the core it is taken for, puts it in host mode, powers the
//! root port and reports what the port sees. Everything a transfer needs -
//! channels, the FIFOs, split transactions through the board's hub - is built
//! on what this step observes, and none of it is here yet.
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
pub const HCFG: usize = 0x400;
pub const HPRT: usize = 0x440;
pub const PCGCCTL: usize = 0xe00;
/// The extent this driver reaches: through the power and clock gating word.
pub const REQUIRED_BYTES: u64 = 0xe04;

/// `GAHBCFG`: the core's one interrupt line is gated by bit 0.
pub const GLOBAL_INTERRUPT: u32 = 1 << 0;
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
    Clock
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
            Self::Clock => write!(f, "the clock refused a reading")
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

#[cfg(target_os = "none")]
mod native;
#[cfg(target_os = "none")]
pub use native::DRIVER;

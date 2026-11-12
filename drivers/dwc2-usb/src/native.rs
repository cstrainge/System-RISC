//! The one audited machine adapter: the claimed controller window, its
//! registers, the clock, and the report of what bring-up observed.
#![allow(unsafe_code)]

use core::ptr::{read_volatile, write_volatile};

use mac_driver::{Client, Completion, Driver, Error, Request, Result as DriverResult,
                 devices::DeviceService, runtime::report};
use spinning_top::Spinlock;

use crate::{Clock, Port, REQUIRED_BYTES, Registers};

/// The controller's `compatible` as this board's tree states it, which is the
/// inventory key and the 16 bytes `module.json` matches on.
const CONTROLLER_KEY: [u8; 16] = *b"brcm,bcm2708-usb";

static DEVICE: Spinlock<Option<Machine>> = Spinlock::new(None);
pub struct Usb;
pub static DRIVER: Usb = Usb;

struct Machine
{
    service: DeviceService,
    claim:   u64,
    base:    usize
}

struct MachineRegisters
{
    base: usize
}
impl Registers for MachineRegisters
{
    fn read(&mut self, offset: usize) -> u32
    {
        debug_assert!((offset as u64) < REQUIRED_BYTES && offset % 4 == 0);
        // SAFETY: a word inside the window this module exclusively claims,
        // mapped as device memory for the claim's lifetime.
        unsafe { u32::from_le(read_volatile((self.base + offset) as *const u32)) }
    }

    fn write(&mut self, offset: usize, value: u32)
    {
        debug_assert!((offset as u64) < REQUIRED_BYTES && offset % 4 == 0);
        // SAFETY: as `read`.
        unsafe { write_volatile((self.base + offset) as *mut u32, value.to_le()) };
    }
}

struct HostClock(Client);
impl Clock for HostClock
{
    fn microseconds(&mut self) -> Option<u64>
    {
        self.0.monotonic_microseconds().ok()
    }
}

impl Machine
{
    /// Claim the controller. Nothing is read until the window is claimed.
    fn claim(&mut self) -> DriverResult<()>
    {
        let slots = self.service.count()?;
        for ordinal in 0..slots
        {
            let description = match self.service.probe(ordinal)
            {
                Ok(description) => description,
                Err(Error(mac_abi::NOT_FOUND)) => continue,
                Err(error) => return Err(error)
            };
            if description.bus.0 != *b"MMIO"
               || description.key != CONTROLLER_KEY
               || description.flags & mac_driver::devices::interface::MMIO == 0
            {
                continue;
            }
            if description.register_bytes < REQUIRED_BYTES || description.registers & 0xfff != 0
            {
                return Err(Error(mac_abi::BAD_ABI));
            }
            let claim = self.service.claim(ordinal)?;
            self.claim = claim.handle;
            if claim.registers != description.registers
               || claim.register_bytes != description.register_bytes
               || claim.key != description.key
            {
                return Err(Error(mac_abi::BAD_ABI));
            }
            self.base = usize::try_from(claim.registers).map_err(|_| Error(mac_abi::BAD_ABI))?;
            return Ok(());
        }
        Err(Error(mac_abi::NOT_FOUND))
    }

    /// Identify the core, make it a host, power the port, and say what was
    /// seen at each stage. Each line is a fact from the hardware; the next
    /// step of this driver is written against them.
    fn bring_up(&mut self, host: Client)
    {
        let mut registers = MachineRegisters { base: self.base };
        let mut clock = HostClock(host);
        let identity = match crate::identify(&mut registers)
        {
            Ok(identity) => identity,
            Err(refusal) =>
            {
                report(format_args!("USB_CORE_REFUSED: {refusal}\n"));
                return;
            }
        };
        report(format_args!("USB_CORE id={:#010x} hwcfg1={:#010x} hwcfg2={:#010x} \
                             hwcfg3={:#010x} hwcfg4={:#010x}\n",
                            identity.id,
                            identity.hardware[0],
                            identity.hardware[1],
                            identity.hardware[2],
                            identity.hardware[3]));
        if let Err(refusal) = crate::start_host(&mut registers, &mut clock)
        {
            report(format_args!("USB_HOST_REFUSED: {refusal}\n"));
            return;
        }
        report(format_args!("USB_HOST_READY: core reset, host mode, interrupts gated\n"));
        match crate::power_port(&mut registers, &mut clock)
        {
            Ok(Port::Empty { port }) =>
            {
                report(format_args!("USB_ROOT_PORT empty hprt={port:#010x}\n"));
            }
            Ok(Port::Attached { speed,
                                enabled,
                                port }) =>
            {
                report(format_args!("USB_ROOT_PORT attached speed={speed:?} enabled={enabled} \
                                     hprt={port:#010x}\n"));
            }
            Err(refusal) => report(format_args!("USB_ROOT_PORT_REFUSED: {refusal}\n"))
        }
    }
}

impl Driver for Usb
{
    fn initialize(&self, host: Client) -> DriverResult<()>
    {
        let mut slot = DEVICE.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
        if slot.is_some()
        {
            return Err(Error(mac_abi::SERVICE_BUSY));
        }
        let mut machine = Machine { service: DeviceService::new(host)?,
                                    claim:   0,
                                    base:    0 };
        // Nothing consumes this driver's class yet, so no request would ever
        // start it; bring-up runs here, bounded, and reports. A controller
        // that refuses is reported and left alone, and the module still
        // loads: a board with a dead USB core must still boot.
        match machine.claim()
        {
            Ok(()) => machine.bring_up(host),
            Err(error) => report(format_args!("USB_CLAIM_REFUSED status={}\n", error.0))
        }
        *slot = Some(machine);
        Ok(())
    }

    fn request(&self, _request: Request<'_>, _output: &mut [u8]) -> DriverResult<Completion>
    {
        // No operation of the host class exists yet.
        Err(Error(mac_abi::NOT_SUPPORTED))
    }

    fn quiesce(&self) -> DriverResult<()>
    {
        let mut slot = DEVICE.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
        if let Some(machine) = slot.as_mut()
            && machine.claim != 0
            && machine.service.release(machine.claim).is_ok()
        {
            machine.claim = 0;
        }
        *slot = None;
        Ok(())
    }
}

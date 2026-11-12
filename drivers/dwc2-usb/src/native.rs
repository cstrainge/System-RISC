//! The one audited machine adapter: the claimed controller window, its
//! registers, the clock, and the report of what bring-up observed.
#![allow(unsafe_code)]

use core::ptr::{copy_nonoverlapping, read_volatile, write_volatile};

use mac_driver::{Client, Completion, Driver, Error, Phase, Request, Result as DriverResult,
                 devices::DeviceService, runtime::report};
use spinning_top::Spinlock;

use crate::{Clock, Port, REQUIRED_BYTES, Registers, TRANSFER_BUFFER_BYTES, TransferBuffer};

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

/// The transfer buffer: one page System allocated for the controller to read
/// and write, which on this board is memory no cache holds.
struct Page
{
    cpu:    usize,
    device: u32
}
impl TransferBuffer for Page
{
    fn address(&self) -> u32
    {
        self.device
    }

    fn capacity(&self) -> usize
    {
        TRANSFER_BUFFER_BYTES
    }

    fn fill(&mut self, bytes: &[u8])
    {
        let count = bytes.len().min(TRANSFER_BUFFER_BYTES);
        // SAFETY: the page is this module's allocation of
        // `TRANSFER_BUFFER_BYTES`, and no transfer is running while it is
        // filled.
        unsafe { copy_nonoverlapping(bytes.as_ptr(), self.cpu as *mut u8, count) };
    }

    fn copy_out(&self, bytes: &mut [u8])
    {
        let count = bytes.len().min(TRANSFER_BUFFER_BYTES);
        // SAFETY: as `fill`; the stage that wrote the page has halted.
        unsafe { copy_nonoverlapping(self.cpu as *const u8, bytes.as_mut_ptr(), count) };
    }
}

/// How long System is given to hand over the transfer page.
const ALLOCATION_MICROSECONDS: u64 = 1_000_000;

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

    /// One page for the controller to read and write, asked for as
    /// device-accessed so System takes it from memory no cache holds.
    fn transfer_page(&mut self, host: Client) -> DriverResult<Page>
    {
        let clock = |host: Client| host.monotonic_microseconds().map_err(|_| Error(mac_abi::IO_ERROR));
        let bytes = TRANSFER_BUFFER_BYTES as u64;
        let mut ticket = 0;
        let deadline = clock(host)?.saturating_add(ALLOCATION_MICROSECONDS);
        loop
        {
            let phase = if ticket == 0 { Phase::Start } else { Phase::Poll };
            match self.service.allocate_device_accessed(self.claim, bytes, &mut ticket, phase)?
            {
                (Completion::Complete(_), Some(buffer)) =>
                {
                    return Ok(Page { cpu:    usize::try_from(buffer.cpu_address)
                                                 .map_err(|_| Error(mac_abi::BAD_ABI))?,
                                     device: u32::try_from(buffer.device_address)
                                                 .map_err(|_| Error(mac_abi::BAD_ABI))? });
                }
                (Completion::Pending, _) if clock(host)? < deadline => (),
                _ =>
                {
                    if ticket != 0
                    {
                        let _ = self.service.allocate_device_accessed(self.claim,
                                                                      bytes,
                                                                      &mut ticket,
                                                                      Phase::Cancel);
                    }
                    return Err(Error(mac_abi::NO_MEMORY));
                }
            }
        }
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
                if !enabled
                {
                    return;
                }
            }
            Err(refusal) =>
            {
                report(format_args!("USB_ROOT_PORT_REFUSED: {refusal}\n"));
                return;
            }
        }
        // The first transfer: ask what is on the port what it is.
        let mut page = match self.transfer_page(host)
        {
            Ok(page) => page,
            Err(error) =>
            {
                report(format_args!("USB_BUFFER_REFUSED status={}\n", error.0));
                return;
            }
        };
        if let Err(refusal) = crate::prepare_transfers(&mut registers, &mut clock, &identity)
        {
            report(format_args!("USB_TRANSFERS_REFUSED: {refusal}\n"));
            return;
        }
        match crate::device_descriptor(&mut registers, &mut clock, &mut page)
        {
            Ok(d) =>
            {
                report(format_args!("USB_DEVICE address=0 usb={:x}.{:02x} class={:02x} \
                                     subclass={:02x} protocol={:02x} max_packet={} \
                                     vendor={:02x}{:02x} product={:02x}{:02x} configurations={}\n",
                                    d[3], d[2], d[4], d[5], d[6], d[7], d[9], d[8], d[11], d[10],
                                    d[17]));
            }
            Err(refusal) => report(format_args!("USB_DEVICE_REFUSED: {refusal}\n"))
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

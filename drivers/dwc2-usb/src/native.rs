//! The one audited machine adapter: the claimed controller window, its
//! registers, the clock, and the report of what bring-up observed.
#![allow(unsafe_code)]

use core::ptr::{copy_nonoverlapping, read_volatile, write_volatile};

use mac_driver::{Client, Completion, Driver, Error, Phase, Request, Result as DriverResult,
                 devices::DeviceService, runtime::report};
use mac_input_interface as input;
use mac_interrupt_interface as interrupt;
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
    base:    usize,
    host:    Option<Client>,
    page:    Option<Page>,
    mouse:   Option<Mouse>,
    /// When the mouse was last asked for a report.
    asked:   u64,
    /// The mouse's read, in steps, once the controller's line is taken.
    read:    Option<crate::SplitRead>,
    /// Reports translated and waiting for System to ask for them.
    queue:   [input::Event; QUEUE_EVENTS],
    queued:  usize
}

/// As many events as one POLL reply holds.
const QUEUE_EVENTS: usize = input::MAX_EVENTS;
/// A transaction that has not halted by now never will.
const WATCHDOG_MICROSECONDS: u64 = 50_000;
/// How long to leave a mouse whose read failed before asking again.
const FAILED_RETRY_MICROSECONDS: u64 = 100_000;

/// Translates one boot report - buttons, then how far it moved across and
/// down - into events, ending with the one that closes the group.
fn translate(mouse: &mut Mouse, bytes: [u8; 3]) -> ([input::Event; 6], usize)
{
    let mut events = [input::Event::synchronize(); 6];
    let mut count = 0;
    for (bit, code) in [(1u8, input::BUTTON_LEFT),
                        (2, input::BUTTON_RIGHT),
                        (4, input::BUTTON_MIDDLE)]
    {
        if (bytes[0] ^ mouse.buttons) & bit != 0
        {
            events[count] = input::Event::key(code, bytes[0] & bit != 0);
            count += 1;
        }
    }
    mouse.buttons = bytes[0] & 0b111;
    for (code, delta) in [(input::RELATIVE_X, bytes[1] as i8), (input::RELATIVE_Y, bytes[2] as i8)]
    {
        if delta != 0
        {
            events[count] = input::Event::relative(code, i32::from(delta));
            count += 1;
        }
    }
    if count != 0
    {
        events[count] = input::Event::synchronize();
        count += 1;
    }
    (events, count)
}

/// A boot-protocol mouse behind the hub, and what reading it needs kept.
struct Mouse
{
    route:    crate::Route,
    address:  u8,
    endpoint: u8,
    packet:   u32,
    /// The data toggle the next report is expected under.
    pid:      u32,
    buttons:  u8,
    refused:  bool
}

/// A mouse is asked for a report no more often than this.
const REPORT_INTERVAL_MICROSECONDS: u64 = 8_000;

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
#[derive(Clone, Copy)]
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
        self.page = Some(Page { cpu:    page.cpu,
                                device: page.device });
        if let Err(refusal) = crate::prepare_transfers(&mut registers, &mut clock, &identity)
        {
            report(format_args!("USB_TRANSFERS_REFUSED: {refusal}\n"));
            return;
        }
        let descriptor = match crate::device_descriptor(&mut registers, &mut clock, &mut page, 0)
        {
            Ok(descriptor) => descriptor,
            Err(refusal) =>
            {
                report(format_args!("USB_DEVICE_REFUSED: {refusal}\n"));
                return;
            }
        };
        let d = descriptor;
        report(format_args!("USB_DEVICE address=0 usb={:x}.{:02x} class={:02x} subclass={:02x} \
                             protocol={:02x} max_packet={} vendor={:02x}{:02x} \
                             product={:02x}{:02x} configurations={}\n",
                            d[3], d[2], d[4], d[5], d[6], d[7], d[9], d[8], d[11], d[10], d[17]));
        if let Err(refusal) = crate::assign_address(&mut registers,
                                                    &mut clock,
                                                    &mut page,
                                                    crate::ROOT_DEVICE_ADDRESS)
        {
            report(format_args!("USB_ADDRESS_REFUSED: {refusal}\n"));
            return;
        }
        // Prove the address took: the same device must answer at it.
        match crate::device_descriptor(&mut registers,
                                       &mut clock,
                                       &mut page,
                                       crate::ROOT_DEVICE_ADDRESS)
        {
            Ok(again) if again == descriptor =>
            {
                report(format_args!("USB_DEVICE_ADDRESSED address={}\n",
                                    crate::ROOT_DEVICE_ADDRESS));
            }
            Ok(_) =>
            {
                report(format_args!("USB_ADDRESS_REFUSED: a different descriptor answered at \
                                     the new address\n"));
                return;
            }
            Err(refusal) =>
            {
                report(format_args!("USB_ADDRESS_REFUSED: {refusal}\n"));
                return;
            }
        }
        if descriptor[4] != crate::CLASS_HUB
        {
            return;
        }
        match crate::start_hub(&mut registers, &mut clock, &mut page, crate::ROOT_DEVICE_ADDRESS)
        {
            Ok(hub) =>
            {
                report(format_args!("USB_HUB address={} ports={} power_good_ms={}\n",
                                    crate::ROOT_DEVICE_ADDRESS,
                                    hub.ports,
                                    hub.power_good_ms));
                for (index, (status, change)) in hub.status[..hub.ports].iter().enumerate()
                {
                    report(format_args!("USB_HUB_PORT port={} connected={} powered={} \
                                         status={:#06x} change={:#06x}\n",
                                        index + 1,
                                        status & crate::HUB_PORT_CONNECTED != 0,
                                        status & crate::HUB_PORT_POWERED != 0,
                                        status,
                                        change));
                }
                // Reset each occupied port in turn. One device answers at the
                // default address at a time, so a port's device is addressed
                // before the next port is touched.
                for index in 0..hub.ports
                {
                    if hub.status[index].0 & crate::HUB_PORT_CONNECTED == 0
                    {
                        continue;
                    }
                    let port = (index + 1) as u8;
                    let (status, _) = match crate::reset_hub_port(&mut registers,
                                                                  &mut clock,
                                                                  &mut page,
                                                                  crate::ROOT_DEVICE_ADDRESS,
                                                                  port)
                    {
                        Ok(words) => words,
                        Err(refusal) =>
                        {
                            report(format_args!("USB_HUB_PORT_RESET_REFUSED port={port}: \
                                                 {refusal}\n"));
                            continue;
                        }
                    };
                    let high = status & crate::HUB_PORT_HIGH_SPEED != 0;
                    let speed = if high
                    {
                        "high"
                    }
                    else if status & crate::HUB_PORT_LOW_SPEED != 0
                    {
                        "low"
                    }
                    else
                    {
                        "full"
                    };
                    report(format_args!("USB_HUB_PORT_RESET port={port} enabled={} speed={speed} \
                                         status={status:#06x}\n",
                                        status & crate::HUB_PORT_ENABLED != 0));
                    if status & crate::HUB_PORT_ENABLED == 0
                    {
                        continue;
                    }
                    let address = crate::ROOT_DEVICE_ADDRESS + port;
                    if !high
                    {
                        // Behind a high-speed hub, a slower device is reached
                        // through the hub by split transactions.
                        let route = crate::Route { hub: crate::ROOT_DEVICE_ADDRESS,
                                                   port,
                                                   low_speed: status
                                                              & crate::HUB_PORT_LOW_SPEED
                                                              != 0 };
                        match crate::address_slow_device(&mut registers,
                                                         &mut clock,
                                                         &mut page,
                                                         route,
                                                         address)
                        {
                            Ok(d) =>
                            {
                                match crate::start_boot_mouse(&mut registers,
                                                              &mut clock,
                                                              &mut page,
                                                              route,
                                                              address,
                                                              u32::from(d[7]))
                                {
                                    Ok(found) if self.mouse.is_none() =>
                                    {
                                        report(format_args!("USB_MOUSE port={port} \
                                                             address={address} interface={} \
                                                             endpoint={} packet={}\n",
                                                            found.interface,
                                                            found.endpoint,
                                                            found.packet));
                                        self.mouse = Some(Mouse { route,
                                                                  address,
                                                                  endpoint: found.endpoint,
                                                                  packet: found.packet.max(1),
                                                                  pid: crate::PID_DATA0,
                                                                  buttons: 0,
                                                                  refused: false });
                                    }
                                    Ok(_) => (),
                                    Err(refusal) =>
                                    {
                                        report(format_args!("USB_MOUSE_REFUSED port={port}: \
                                                             {refusal}\n"));
                                    }
                                }
                                report(format_args!("USB_PORT_DEVICE port={port} \
                                                     address={address} speed={speed} \
                                                     usb={:x}.{:02x} class={:02x} \
                                                     subclass={:02x} protocol={:02x} \
                                                     max_packet={} vendor={:02x}{:02x} \
                                                     product={:02x}{:02x}\n",
                                                    d[3], d[2], d[4], d[5], d[6], d[7], d[9],
                                                    d[8], d[11], d[10]));
                            }
                            Err(refusal) =>
                            {
                                report(format_args!("USB_PORT_DEVICE_REFUSED port={port} \
                                                     speed={speed}: {refusal}\n"));
                            }
                        }
                        continue;
                    }
                    let d = match crate::device_descriptor(&mut registers,
                                                           &mut clock,
                                                           &mut page,
                                                           0)
                    {
                        Ok(descriptor) => descriptor,
                        Err(refusal) =>
                        {
                            report(format_args!("USB_PORT_DEVICE_REFUSED port={port}: \
                                                 {refusal}\n"));
                            continue;
                        }
                    };
                    if let Err(refusal) =
                        crate::assign_address(&mut registers, &mut clock, &mut page, address)
                    {
                        report(format_args!("USB_PORT_DEVICE_REFUSED port={port}: {refusal}\n"));
                        continue;
                    }
                    report(format_args!("USB_PORT_DEVICE port={port} address={address} \
                                         usb={:x}.{:02x} class={:02x} subclass={:02x} \
                                         protocol={:02x} vendor={:02x}{:02x} \
                                         product={:02x}{:02x}\n",
                                        d[3], d[2], d[4], d[5], d[6], d[9], d[8], d[11], d[10]));
                }
            }
            Err(refusal) => report(format_args!("USB_HUB_REFUSED: {refusal}\n"))
        }
    }
}

impl Machine
{
    /// Takes the controller's interrupt line, if the machine gives one, and
    /// starts reading the mouse from it. Without the line the mouse is read
    /// when System asks, as before.
    fn listen(&mut self)
    {
        let (Some(mouse), Some(page)) = (self.mouse.as_ref(), self.page)
        else
        {
            return;
        };
        let line = match self.service.take_interrupt(self.claim)
        {
            Ok(line) => line,
            Err(error) =>
            {
                report(format_args!("USB_INTERRUPT_REFUSED status={}: the mouse is read when \
                                     System asks\n",
                                    error.0));
                return;
            }
        };
        let mut registers = MachineRegisters { base: self.base };
        let mut read =
            crate::SplitRead::new(&page, mouse.route, mouse.address, mouse.endpoint, mouse.packet);
        crate::listen_for_channel(&mut registers);
        read.start(&mut registers, mouse.pid);
        self.read = Some(read);
        let alarm = self.service.set_alarm(self.claim, WATCHDOG_MICROSECONDS);
        report(format_args!("USB_INTERRUPT line={line}: the mouse is read from the \
                             controller's interrupt (alarm={})\n",
                            if alarm.is_ok() { "set" } else { "refused" }));
    }

    /// The controller interrupted, or this driver's alarm fell due: take the
    /// mouse's read one step on, and say when to be called next.
    fn interrupted(&mut self, line: u32)
    {
        let (Some(read), Some(mouse), Some(page)) =
            (self.read.as_mut(), self.mouse.as_mut(), self.page)
        else
        {
            return;
        };
        let mut registers = MachineRegisters { base: self.base };
        let step = if line != interrupt::ALARM
        {
            read.halted(&mut registers)
        }
        else if read.idle()
        {
            read.start(&mut registers, mouse.pid);
            crate::ReadStep::Running
        }
        else
        {
            read.alarm(&mut registers)
        };
        let next = match step
        {
            crate::ReadStep::Stray => return,
            crate::ReadStep::Running => WATCHDOG_MICROSECONDS,
            crate::ReadStep::Wait(microseconds) => microseconds,
            crate::ReadStep::Nothing => REPORT_INTERVAL_MICROSECONDS,
            crate::ReadStep::Data(received) =>
            {
                mouse.pid =
                    if mouse.pid == crate::PID_DATA0 { crate::PID_DATA1 } else { crate::PID_DATA0 };
                if received >= 3
                {
                    let mut bytes = [0u8; 3];
                    page.copy_out(&mut bytes);
                    let (events, count) = translate(mouse, bytes);
                    // A full queue means System has stopped asking; the
                    // newest report is the one dropped.
                    if self.queued + count <= QUEUE_EVENTS
                    {
                        self.queue[self.queued..self.queued + count]
                            .copy_from_slice(&events[..count]);
                        self.queued += count;
                    }
                }
                REPORT_INTERVAL_MICROSECONDS
            }
            crate::ReadStep::Failed(status) =>
            {
                // Said once. A wrong data toggle is the likeliest cause, and
                // trying the other one is the remedy either way.
                if !mouse.refused
                {
                    mouse.refused = true;
                    report(format_args!("USB_MOUSE_READ_REFUSED: the read halted without \
                                         completing (HCINT={status:#010x})\n"));
                }
                mouse.pid =
                    if mouse.pid == crate::PID_DATA0 { crate::PID_DATA1 } else { crate::PID_DATA0 };
                FAILED_RETRY_MICROSECONDS
            }
        };
        let _ = self.service.set_alarm(self.claim, next);
    }

    /// POLL of the input class: ask the mouse for one report and translate
    /// it. Bounded: one split read, which a mouse with nothing to say ends at
    /// its first answer.
    fn poll(&mut self, request: Request<'_>, output: &mut [u8]) -> DriverResult<Completion>
    {
        if request.class == interrupt::CLASS
        {
            if request.operation != interrupt::SERVICE || !output.is_empty()
            {
                return Err(Error(mac_abi::NOT_SUPPORTED));
            }
            if request.phase != Phase::Cancel
            {
                self.interrupted(interrupt::line(request.input).ok_or(Error(mac_abi::BAD_ARGUMENT))?);
            }
            return Ok(Completion::Complete(0));
        }
        if request.class != input::CLASS || request.operation != input::POLL
        {
            return Err(Error(mac_abi::NOT_SUPPORTED));
        }
        if request.phase == Phase::Cancel
        {
            return Ok(Completion::Complete(0));
        }
        if !request.input.is_empty()
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        if self.read.is_some()
        {
            // The controller's interrupt reads the mouse; this hands over what
            // it has gathered and touches no hardware.
            let count = self.queued.min(output.len() / input::EVENT_BYTES);
            for (slot, event) in
                output.chunks_exact_mut(input::EVENT_BYTES).zip(&self.queue[..count])
            {
                slot.copy_from_slice(&event.encode());
            }
            self.queue.copy_within(count..self.queued, 0);
            self.queued -= count;
            return Ok(Completion::Complete(count * input::EVENT_BYTES));
        }
        let (Some(mouse), Some(mut page), Some(host)) = (self.mouse.as_mut(), self.page, self.host)
        else
        {
            return Ok(Completion::Complete(0));
        };
        let mut clock = HostClock(host);
        let now = clock.microseconds().unwrap_or(0);
        if now.saturating_sub(self.asked) < REPORT_INTERVAL_MICROSECONDS
        {
            return Ok(Completion::Complete(0));
        }
        self.asked = now;
        let mut registers = MachineRegisters { base: self.base };
        let received = match crate::split_read(&mut registers,
                                               &mut clock,
                                               &mut page,
                                               mouse.route,
                                               mouse.address,
                                               mouse.endpoint,
                                               mouse.packet,
                                               mouse.pid)
        {
            Ok(Some(received)) => received,
            Ok(None) => return Ok(Completion::Complete(0)),
            Err(refusal) =>
            {
                // Said once. A wrong data toggle is the likeliest cause, and
                // trying the other one is the remedy either way.
                if !mouse.refused
                {
                    mouse.refused = true;
                    report(format_args!("USB_MOUSE_READ_REFUSED: {refusal}\n"));
                }
                mouse.pid = if mouse.pid == crate::PID_DATA0 { crate::PID_DATA1 } else { crate::PID_DATA0 };
                return Ok(Completion::Complete(0));
            }
        };
        mouse.pid = if mouse.pid == crate::PID_DATA0 { crate::PID_DATA1 } else { crate::PID_DATA0 };
        if received < 3
        {
            return Ok(Completion::Complete(0));
        }
        let mut bytes = [0u8; 3];
        page.copy_out(&mut bytes);
        let (events, count) = translate(mouse, bytes);
        if count == 0
        {
            return Ok(Completion::Complete(0));
        }
        let needed = count * input::EVENT_BYTES;
        if output.len() < needed
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        for (slot, event) in output.chunks_exact_mut(input::EVENT_BYTES).zip(&events[..count])
        {
            slot.copy_from_slice(&event.encode());
        }
        Ok(Completion::Complete(needed))
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
                                    base:    0,
                                    host:    Some(host),
                                    page:    None,
                                    mouse:   None,
                                    asked:   0,
                                    read:    None,
                                    queue:   [input::Event::synchronize(); QUEUE_EVENTS],
                                    queued:  0 };
        // Nothing consumes this driver's class yet, so no request would ever
        // start it; bring-up runs here, bounded, and reports. A controller
        // that refuses is reported and left alone, and the module still
        // loads: a board with a dead USB core must still boot.
        match machine.claim()
        {
            Ok(()) =>
            {
                machine.bring_up(host);
                machine.listen();
            }
            Err(error) => report(format_args!("USB_CLAIM_REFUSED status={}\n", error.0))
        }
        *slot = Some(machine);
        Ok(())
    }

    fn request(&self, request: Request<'_>, output: &mut [u8]) -> DriverResult<Completion>
    {
        DEVICE.try_lock()
              .ok_or(Error(mac_abi::SERVICE_BUSY))?
              .as_mut()
              .ok_or(Error(mac_abi::NOT_FOUND))?
              .poll(request, output)
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

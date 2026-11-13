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
    keyboard: Option<Keyboard>,
    /// The reads, in steps, once the controller's line is taken.
    mouse_reader: Option<Reader>,
    keyboard_reader: Option<Reader>,
    hub_reader: Option<Reader>,
    /// How many ports the hub has, or 0 when there is none.
    hub_ports: u8,
    listening: bool,
    /// Reports translated and waiting for System to ask for them.
    queue:   [input::Event; QUEUE_EVENTS],
    queued:  usize
}

/// A boot-protocol keyboard behind the hub.
struct Keyboard
{
    route:    crate::Route,
    address:  u8,
    endpoint: u8,
    packet:   u32,
    /// The last report that said anything: the keys held.
    last:     [u8; 8],
    /// The key that repeats while it is held, and when it next does, in
    /// microseconds; 0 for none. The last key to go down is the one.
    repeat:   u16,
    repeats:  u64
}

/// How long a key is held before it repeats, and how often it then does.
const REPEAT_DELAY_MICROSECONDS: u64 = 500_000;
const REPEAT_INTERVAL_MICROSECONDS: u64 = 40_000;

/// The channel each device is read on, which is also the part of the
/// transfer page its reports land in.
const MOUSE_CHANNEL: usize = 0;
const KEYBOARD_CHANNEL: usize = 1;
const HUB_CHANNEL: usize = 2;
/// The part of the transfer page the waiting transfers use, clear of the
/// parts the reads above land in.
const CONTROL_PART: usize = 4;
/// A hub reports which ports changed on its one interrupt endpoint, a bit
/// for each port. This is every hub's layout and is not read from this one's
/// descriptors.
const HUB_CHANGE_ENDPOINT: u8 = 1;
const HUB_CHANGE_PACKET: u32 = 1;
/// How often the hub is asked whether anything changed.
const HUB_INTERVAL_MICROSECONDS: u64 = 100_000;

/// One device's read, and what stepping it needs kept.
struct Reader
{
    read:    crate::SplitRead,
    page:    Page,
    /// The data toggle the next report is expected under.
    pid:     u32,
    /// When this read next wants the alarm, in microseconds, or 0.
    due:      u64,
    /// How long after one answer the next is asked for.
    interval: u64,
    failures: u32,
    heard:    bool,
    name:     &'static str
}

/// How many failed reads a device is allowed to report.
const FAILURES_SAID: u32 = 4;
impl Reader
{
    /// Takes the read one step on: the channel halted, or its time came.
    /// Returns the bytes of a report that arrived.
    fn step(&mut self, registers: &mut MachineRegisters, now: u64, halted: bool) -> Option<usize>
    {
        let step = if halted
        {
            self.read.halted(registers)
        }
        else if self.read.idle()
        {
            self.read.start(registers, self.pid);
            crate::ReadStep::Running
        }
        else
        {
            self.read.alarm(registers)
        };
        let (delay, received) = match step
        {
            crate::ReadStep::Stray => return None,
            crate::ReadStep::Running => (WATCHDOG_MICROSECONDS, None),
            crate::ReadStep::Wait(microseconds) => (microseconds, None),
            crate::ReadStep::Nothing => (self.interval, None),
            crate::ReadStep::Data(received) =>
            {
                // The toggle moves on only when a report was taken. After a
                // failure the device either never sent, or sent and was
                // acknowledged and discarded; in both cases its next report
                // comes under the toggle already expected here.
                self.pid =
                    if self.pid == crate::PID_DATA0 { crate::PID_DATA1 } else { crate::PID_DATA0 };
                if !self.heard
                {
                    self.heard = true;
                    report(format_args!("USB_READ_FIRST device={} bytes={received}\n",
                                        self.name));
                }
                (self.interval, Some(received))
            }
            crate::ReadStep::Failed(status) =>
            {
                // The first few are said, each with what the channel reported;
                // a device that has been unplugged fails at every interval.
                if self.failures < FAILURES_SAID
                {
                    self.failures += 1;
                    report(format_args!("USB_READ_REFUSED device={} stage={}: the read halted \
                                         without completing (HCINT={status:#010x})\n",
                                        self.name,
                                        if self.read.failed_at_start()
                                        {
                                            "start"
                                        }
                                        else
                                        {
                                            "completion"
                                        }));
                }
                (FAILED_RETRY_MICROSECONDS, None)
            }
        };
        self.due = now.saturating_add(delay).max(1);
        received
    }
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
impl Page
{
    /// A part of the page for one channel's reports, far enough from the
    /// next that the largest packet a slow device sends fits.
    fn part(self, channel: usize) -> Self
    {
        let offset = channel * 256;
        Self { cpu:    self.cpu + offset,
               device: self.device + offset as u32 }
    }
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
        // The waiting transfers have a part of the page to themselves, so the
        // ones made later, when a device arrives, do not land on a report.
        let mut page = page.part(CONTROL_PART);
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
                    if hub.status[index].0 & crate::HUB_PORT_CONNECTED != 0
                    {
                        self.attach_port(&mut registers, &mut clock, &mut page, (index + 1) as u8);
                    }
                }
                self.hub_ports = hub.ports as u8;
            }
            Err(refusal) => report(format_args!("USB_HUB_REFUSED: {refusal}\n"))
        }
    }
}

impl Machine
{
    /// One port of the hub: reset it, give what is there an address, and take
    /// up any boot-protocol mouse or keyboard it carries. Used when the bus is
    /// first walked and again whenever the hub says a port has gained a device.
    fn attach_port(&mut self, registers: &mut MachineRegisters, clock: &mut HostClock,
                   page: &mut Page, port: u8)
    {
        let (status, _) = match crate::reset_hub_port(registers,
                                                      clock,
                                                      page,
                                                      crate::ROOT_DEVICE_ADDRESS,
                                                      port)
        {
            Ok(words) => words,
            Err(refusal) =>
            {
                report(format_args!("USB_HUB_PORT_RESET_REFUSED port={port}: \
                                     {refusal}\n"));
                return;
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
            return;
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
            match crate::address_slow_device(registers,
                                             clock,
                                             page,
                                             route,
                                             address)
            {
                Ok(d) =>
                {
                    match crate::start_boot_inputs(registers,
                                                  clock,
                                                  page,
                                                  route,
                                                  address,
                                                  u32::from(d[7]))
                    {
                        Ok(inputs) =>
                        {
                            for interface in &inputs.interfaces[..inputs.count]
                            {
                                report(format_args!("USB_INTERFACE port={port} \
                                                     number={} class={:02x} \
                                                     subclass={:02x} \
                                                     protocol={:02x}\n",
                                                    interface[0],
                                                    interface[1],
                                                    interface[2],
                                                    interface[3]));
                            }
                            if let Some(found) = inputs.mouse
                               && self.mouse.is_none()
                            {
                                report(format_args!("USB_MOUSE port={port} \
                                                     address={address} \
                                                     interface={} endpoint={} \
                                                     packet={}\n",
                                                    found.interface,
                                                    found.endpoint,
                                                    found.packet));
                                self.mouse =
                                    Some(Mouse { route,
                                                 address,
                                                 endpoint: found.endpoint,
                                                 packet: found.packet.max(1),
                                                 pid: crate::PID_DATA0,
                                                 buttons: 0,
                                                 refused: false });
                            }
                            if let Some(found) = inputs.keyboard
                               && self.keyboard.is_none()
                            {
                                report(format_args!("USB_KEYBOARD port={port} \
                                                     address={address} \
                                                     interface={} endpoint={} \
                                                     packet={}\n",
                                                    found.interface,
                                                    found.endpoint,
                                                    found.packet));
                                self.keyboard =
                                    Some(Keyboard { route,
                                                    address,
                                                    endpoint: found.endpoint,
                                                    packet: found.packet.max(1),
                                                    last: [0; 8],
                                                    repeat: 0,
                                                    repeats: 0 });
                            }
                        }
                        Err(refusal) =>
                        {
                            report(format_args!("USB_INPUT_REFUSED port={port}: \
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
            return;
        }
        let d = match crate::device_descriptor(registers,
                                               clock,
                                               page,
                                               0)
        {
            Ok(descriptor) => descriptor,
            Err(refusal) =>
            {
                report(format_args!("USB_PORT_DEVICE_REFUSED port={port}: \
                                     {refusal}\n"));
                return;
            }
        };
        if let Err(refusal) =
            crate::assign_address(registers, clock, page, address)
        {
            report(format_args!("USB_PORT_DEVICE_REFUSED port={port}: {refusal}\n"));
            return;
        }
        report(format_args!("USB_PORT_DEVICE port={port} address={address} \
                             usb={:x}.{:02x} class={:02x} subclass={:02x} \
                             protocol={:02x} vendor={:02x}{:02x} \
                             product={:02x}{:02x}\n",
                            d[3], d[2], d[4], d[5], d[6], d[9], d[8], d[11], d[10]));
    }

    /// Whatever was on this port is gone: stop reading it and forget it. Keys
    /// a keyboard was holding are let up, or they would be held for ever.
    fn detach_port(&mut self, registers: &mut MachineRegisters, port: u8)
    {
        if self.mouse.as_ref().is_some_and(|mouse| mouse.route.port == port)
        {
            if let Some(reader) = self.mouse_reader.as_mut()
            {
                reader.read.abandon(registers);
            }
            self.mouse_reader = None;
            self.mouse = None;
            report(format_args!("USB_MOUSE_GONE port={port}\n"));
        }
        if let Some(keyboard) = self.keyboard.as_ref()
           && keyboard.route.port == port
        {
            let mut events = [input::Event::synchronize(); 15];
            let mut count = 0;
            crate::keyboard_changes(&keyboard.last, &[0; 8], |key, down| {
                if count < 14
                {
                    events[count] = input::Event::key(key, down);
                    count += 1;
                }
            });
            if let Some(reader) = self.keyboard_reader.as_mut()
            {
                reader.read.abandon(registers);
            }
            self.keyboard_reader = None;
            self.keyboard = None;
            if count != 0
            {
                count += 1;
                self.enqueue(&events[..count]);
            }
            report(format_args!("USB_KEYBOARD_GONE port={port}\n"));
        }
    }

    /// Starts a read for each device that has none yet.
    fn start_readers(&mut self, registers: &mut MachineRegisters, now: u64)
    {
        let Some(page) = self.page
        else
        {
            return;
        };
        if self.mouse_reader.is_none()
           && let Some(mouse) = self.mouse.as_ref()
        {
            let page = page.part(MOUSE_CHANNEL);
            let mut reader = Reader { read: crate::SplitRead::new(MOUSE_CHANNEL,
                                                                  &page,
                                                                  mouse.route,
                                                                  mouse.address,
                                                                  mouse.endpoint,
                                                                  mouse.packet),
                                      page,
                                      pid: mouse.pid,
                                      due: 0,
                                      interval: REPORT_INTERVAL_MICROSECONDS,
                                      failures: 0,
                                      heard: false,
                                      name: "mouse" };
            reader.step(registers, now, false);
            self.mouse_reader = Some(reader);
        }
        if self.keyboard_reader.is_none()
           && let Some(keyboard) = self.keyboard.as_ref()
        {
            let page = page.part(KEYBOARD_CHANNEL);
            let mut reader = Reader { read: crate::SplitRead::new(KEYBOARD_CHANNEL,
                                                                  &page,
                                                                  keyboard.route,
                                                                  keyboard.address,
                                                                  keyboard.endpoint,
                                                                  keyboard.packet),
                                      page,
                                      pid: crate::PID_DATA0,
                                      due: 0,
                                      interval: REPORT_INTERVAL_MICROSECONDS,
                                      failures: 0,
                                      heard: false,
                                      name: "keyboard" };
            reader.step(registers, now, false);
            self.keyboard_reader = Some(reader);
        }
    }

    /// Reads each port's change bits and clears the ones that are set, so the
    /// hub's change report says only what happens from here on. Returns each
    /// port's status and what had changed.
    fn settle_port(&mut self, registers: &mut MachineRegisters, clock: &mut HostClock,
                   page: &mut Page, port: u8)
                   -> Option<(u16, u16)>
    {
        let (status, change) =
            crate::hub_port_status(registers, clock, page, crate::ROOT_DEVICE_ADDRESS, port).ok()?;
        // The change bits are features 16 to 20: connection, enable, suspend,
        // over-current and reset.
        for bit in 0..5u8
        {
            if change & 1 << bit != 0
            {
                let _ = crate::control(registers,
                                       clock,
                                       page,
                                       crate::ROOT_DEVICE_ADDRESS,
                                       [0x23, 0x01, 16 + bit, 0, port, 0, 0, 0],
                                       &mut []);
            }
        }
        Some((status, change))
    }

    /// The hub said these ports changed: find out how, and take up or let go
    /// of what is on each. This runs the same bounded, waiting transfers the
    /// first walk of the bus does, so a device arriving holds the caller for
    /// as long as its reset and its questions take.
    fn ports_changed(&mut self, registers: &mut MachineRegisters, changed: u32, now: u64)
    {
        let (Some(host), Some(page)) = (self.host, self.page)
        else
        {
            return;
        };
        let mut clock = HostClock(host);
        let mut page = page.part(CONTROL_PART);
        for port in 1..=self.hub_ports
        {
            if changed & 1 << port == 0
            {
                continue;
            }
            let Some((status, change)) = self.settle_port(registers, &mut clock, &mut page, port)
            else
            {
                continue;
            };
            if change & crate::HUB_PORT_CONNECTED == 0
            {
                continue;
            }
            // Whatever was known about the port is stale either way.
            self.detach_port(registers, port);
            if status & crate::HUB_PORT_CONNECTED != 0
            {
                report(format_args!("USB_PORT_ATTACHED port={port} status={status:#06x}\n"));
                self.attach_port(registers, &mut clock, &mut page, port);
                // The reset raised its own change bits; they are not news.
                let _ = self.settle_port(registers, &mut clock, &mut page, port);
            }
            else
            {
                report(format_args!("USB_PORT_DETACHED port={port} status={status:#06x}\n"));
            }
        }
        self.start_readers(registers, now);
    }

    /// Takes the controller's interrupt line, if the machine gives one, and
    /// starts reading the mouse, the keyboard and the hub's change reports
    /// from it. Without the line the mouse is read when System asks, as
    /// before; a keyboard is not read and a device plugged in later is not
    /// noticed.
    fn listen(&mut self)
    {
        let (Some(page), Some(host)) = (self.page, self.host)
        else
        {
            return;
        };
        if self.hub_ports == 0 && self.mouse.is_none() && self.keyboard.is_none()
        {
            return;
        }
        let line = match self.service.take_interrupt(self.claim)
        {
            Ok(line) => line,
            Err(error) =>
            {
                report(format_args!("USB_INTERRUPT_REFUSED status={}: the mouse is read when \
                                     System asks, a keyboard is not read, and a device plugged \
                                     in later is not noticed\n",
                                    error.0));
                return;
            }
        };
        let mut registers = MachineRegisters { base: self.base };
        // What changed while the bus was first walked is already acted on.
        {
            let mut clock = HostClock(host);
            let mut control = page.part(CONTROL_PART);
            for port in 1..=self.hub_ports
            {
                let _ = self.settle_port(&mut registers, &mut clock, &mut control, port);
            }
        }
        let now = self.now();
        crate::listen_for_channels(&mut registers, 3);
        self.start_readers(&mut registers, now);
        if self.hub_ports != 0
        {
            let page = page.part(HUB_CHANNEL);
            let mut reader = Reader { read: crate::SplitRead::direct(HUB_CHANNEL,
                                                                     &page,
                                                                     crate::ROOT_DEVICE_ADDRESS,
                                                                     HUB_CHANGE_ENDPOINT,
                                                                     HUB_CHANGE_PACKET),
                                      page,
                                      pid: crate::PID_DATA0,
                                      due: 0,
                                      interval: HUB_INTERVAL_MICROSECONDS,
                                      failures: 0,
                                      heard: false,
                                      name: "hub" };
            reader.step(&mut registers, now, false);
            self.hub_reader = Some(reader);
        }
        self.listening = true;
        let alarm = self.rearm(now);
        report(format_args!("USB_INTERRUPT line={line}: mouse={} keyboard={} hub={} are read \
                             from the controller's interrupt (alarm={})\n",
                            self.mouse_reader.is_some(),
                            self.keyboard_reader.is_some(),
                            self.hub_reader.is_some(),
                            if alarm { "set" } else { "refused" }));
    }

    fn now(&self) -> u64
    {
        self.host.and_then(|host| host.monotonic_microseconds().ok()).unwrap_or(0)
    }

    /// Sets the one alarm this claim has for whichever read is due first.
    fn rearm(&mut self, now: u64) -> bool
    {
        let due = [self.mouse_reader.as_ref(),
                   self.keyboard_reader.as_ref(),
                   self.hub_reader.as_ref()].into_iter()
                                                                             .flatten()
                                                                             .map(|reader| reader.due)
                                                                             .chain(self.keyboard
                                                                                        .as_ref()
                                                                                        .map(|k| k.repeats))
                                                                             .filter(|&due| due != 0)
                                                                             .min();
        match due
        {
            Some(due) => self.service.set_alarm(self.claim, due.saturating_sub(now).max(1)).is_ok(),
            None => true
        }
    }

    fn enqueue(&mut self, events: &[input::Event])
    {
        // A full queue means System has stopped asking; the newest report is
        // the one dropped.
        if self.queued + events.len() <= QUEUE_EVENTS
        {
            self.queue[self.queued..self.queued + events.len()].copy_from_slice(events);
            self.queued += events.len();
        }
    }

    /// The controller interrupted, or this driver's alarm fell due: take each
    /// read that is concerned one step on, and say when to be called next.
    fn interrupted(&mut self, line: u32)
    {
        let now = self.now();
        let mut registers = MachineRegisters { base: self.base };
        let halted = line != interrupt::ALARM;
        let asking = if halted { registers.read(crate::HAINT) } else { 0 };
        let concerned = |reader: &Reader, channel: usize| {
            if halted { asking & 1 << channel != 0 } else { reader.due != 0 && reader.due <= now }
        };
        if let Some(reader) = self.mouse_reader.as_mut()
           && concerned(reader, MOUSE_CHANNEL)
           && let Some(received) = reader.step(&mut registers, now, halted)
           && received >= 3
           && let Some(mouse) = self.mouse.as_mut()
        {
            let mut bytes = [0u8; 3];
            reader.page.copy_out(&mut bytes);
            let (events, count) = translate(mouse, bytes);
            self.enqueue(&events[..count]);
        }
        if let Some(reader) = self.keyboard_reader.as_mut()
           && concerned(reader, KEYBOARD_CHANNEL)
           && let Some(received) = reader.step(&mut registers, now, halted)
           && received >= 8
           && let Some(keyboard) = self.keyboard.as_mut()
        {
            let mut report = [0u8; 8];
            reader.page.copy_out(&mut report);
            let mut events = [input::Event::synchronize(); 15];
            let mut count = 0;
            let (mut pressed, mut released) = (0u16, false);
            let held = keyboard.repeat;
            let said = crate::keyboard_changes(&keyboard.last, &report, |key, down| {
                if count < 14
                {
                    events[count] = input::Event::key(key, down);
                    count += 1;
                }
                if down && !crate::MODIFIER_KEYS.contains(&key)
                {
                    pressed = key;
                }
                else if !down && key == held
                {
                    released = true;
                }
            });
            // The last key to go down is the one that repeats; letting it up
            // stops that, and so does any other key going down.
            if pressed != 0
            {
                keyboard.repeat = pressed;
                keyboard.repeats = now.saturating_add(REPEAT_DELAY_MICROSECONDS).max(1);
            }
            else if released
            {
                keyboard.repeat = 0;
                keyboard.repeats = 0;
            }
            if said
            {
                keyboard.last = report;
            }
            if count != 0
            {
                count += 1;
                self.enqueue(&events[..count]);
            }
        }
        // The hub's report is a bit for each port that changed.
        let mut changed = 0u32;
        if let Some(reader) = self.hub_reader.as_mut()
           && concerned(reader, HUB_CHANNEL)
           && let Some(received) = reader.step(&mut registers, now, halted)
           && received >= 1
        {
            let mut bits = [0u8; 1];
            reader.page.copy_out(&mut bits);
            changed = u32::from(bits[0]);
        }
        if changed != 0
        {
            self.ports_changed(&mut registers, changed, now);
        }
        // A held key says so again: the input class's "repeat" is a key event
        // whose value is 2, which System turns into an auto-key.
        if !halted
           && let Some(keyboard) = self.keyboard.as_mut()
           && keyboard.repeats != 0
           && keyboard.repeats <= now
        {
            keyboard.repeats = now.saturating_add(REPEAT_INTERVAL_MICROSECONDS);
            let events = [input::Event { kind:  input::KEY,
                                         code:  keyboard.repeat,
                                         value: 2 },
                          input::Event::synchronize()];
            self.enqueue(&events);
        }
        self.rearm(now);
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
        if self.listening
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
                                    keyboard: None,
                                    mouse_reader: None,
                                    keyboard_reader: None,
                                    hub_reader: None,
                                    hub_ports: 0,
                                    listening: false,
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

//! Audited modern virtio-console MMIO/DMA adapter. Queue storage is pinned static
//! memory inside this module's admitted load reservation. CPU never creates a
//! Rust reference to a DMA ring/buffer; every device-shared access uses raw
//! pointers, ownership bookkeeping and the virtio publication fences.
#![allow(unsafe_code)]
use core::{cell::UnsafeCell,
           mem::{offset_of, size_of},
           ptr::{addr_of_mut, copy_nonoverlapping, read_volatile, write_volatile},
           sync::atomic::{AtomicU8, Ordering}};

use mac_abi::{BAD_ABI, BAD_ARGUMENT, IO_ERROR, NOT_FOUND, NOT_SUPPORTED, SERVICE_BUSY};
use mac_driver::{Client, Completion, Error, Phase, Result,
                 devices::{DeviceService, interface}};

use crate::{Channel, Diagnostic, FRAGMENT, QUEUE_INDICES,
            control::{Control, Event},
            discovery::{Discovery, Step},
            lifetime::{Authority, Lifetime},
            validate_profile};
const N: usize = 8;
/// Four unique arena banks: selected port 1 uses device queues 4/5, controls
/// use 2/3. Reserved/uninstantiated port 0 queues 0/1 are never configured.
fn hardware_queue(bank: usize) -> u32
{
    QUEUE_INDICES[bank]
}
#[derive(Clone, Copy)]
#[repr(C)]
struct Descriptor
{
    address: u64,
    length:  u32,
    flags:   u16,
    next:    u16
}
#[repr(C)]
struct Available
{
    flags: u16,
    index: u16,
    ring:  [u16; N],
    event: u16
}
#[derive(Clone, Copy)]
#[repr(C)]
struct UsedEntry
{
    id:     u32,
    length: u32
}
#[repr(C)]
struct Used
{
    flags: u16,
    index: u16,
    ring:  [UsedEntry; N],
    event: u16
}
#[repr(C, align(4096))]
struct QueueMemory
{
    descriptors: [Descriptor; N],
    available:   Available,
    used:        Used,
    buffers:     [[u8; FRAGMENT]; N]
}
impl QueueMemory
{
    const fn new() -> Self
    {
        Self { descriptors: [Descriptor { address: 0,
                                         length:  0,
                                         flags:   0,
                                         next:    0 }; N],
               available:   Available { flags: 0,
                                        index: 0,
                                        ring:  [0; N],
                                        event: 0 },
               used:        Used { flags: 0,
                                   index: 0,
                                   ring:  [UsedEntry { id:     0,
                                                      length: 0 };
                                           N],
                                   event: 0 },
               buffers:     [[0; FRAGMENT]; N] }
    }
}
/// Pinned module-owned DMA storage. State 2 retains cleanup authority after a
/// failed Drop. Backing must remain loaded until the module owner walk confirms
/// every claim/mapping was retired; a failed cleanup never returns this lease.
pub struct Arena
{
    memory:   UnsafeCell<[QueueMemory; 4]>,
    state:    AtomicU8,
    retained: UnsafeCell<Option<Resources>>
}
// SAFETY: state grants one exclusive Device or recovery walk. DMA cells are only
// accessed through raw volatile operations, never through Rust references.
unsafe impl Sync for Arena
{
}
impl Arena
{
    pub const fn new() -> Self
    {
        Self { memory:   UnsafeCell::new([const { QueueMemory::new() }; 4]),
               state:    AtomicU8::new(0),
               retained: UnsafeCell::new(None) }
    }

    fn acquire(&'static self) -> Result<()>
    {
        if self.state.compare_exchange(2, 3, Ordering::AcqRel, Ordering::Acquire).is_ok()
        {
            // SAFETY: state 3 exclusively owns the retained cleanup record.
            let slot = unsafe { &mut *self.retained.get() };
            let complete = match slot.as_mut()
            {
                Some(resources) => resources.cleanup(),
                None => Err(Error(BAD_ABI))
            };
            match complete
            {
                Ok(true) =>
                {
                    *slot = None;
                    self.state.store(0, Ordering::Release);
                }
                Ok(false) =>
                {
                    self.state.store(2, Ordering::Release);
                    return Err(Error(SERVICE_BUSY));
                }
                Err(error) =>
                {
                    self.state.store(2, Ordering::Release);
                    return Err(error);
                }
            }
        }
        self.state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| Error(SERVICE_BUSY))
    }
}
impl Default for Arena
{
    fn default() -> Self
    {
        Self::new()
    }
}
struct Resources
{
    service: DeviceService,
    life:    Lifetime
}
impl Authority for DeviceService
{
    fn reset(&mut self, claim: u64, ticket: &mut u64, phase: Phase) -> Result<Completion>
    {
        (*self).reset(claim, ticket, phase)
    }

    fn unmap(&mut self, mapping: u64) -> Result<()>
    {
        (*self).unmap(mapping)
    }

    fn release(&mut self, claim: u64) -> Result<()>
    {
        (*self).release(claim)
    }
}
impl Resources
{
    fn cleanup(&mut self) -> Result<bool>
    {
        self.life.cleanup(&mut self.service)
    }
}
#[derive(Clone, Copy)]
struct Queue
{
    owned:     [bool; N],
    lengths:   [usize; N],
    available: u16,
    used:      u16
}
impl Queue
{
    const fn new() -> Self
    {
        Self { owned:     [false; N],
               lengths:   [0; N],
               available: 0,
               used:      0 }
    }
}
struct Hardware
{
    resources: Resources,
    arena:     &'static Arena,
    ordinal:   u32,
    registers: usize,
    dma:       u64,
    queues:    [Queue; 4],
    running:   bool,
    control:   Control,
    receive:   Option<(usize, usize, usize)>,
    closing:   bool,
    restart:   bool
}
fn fence()
{
    unsafe {
        core::arch::asm!("fence iorw, iorw", options(nostack));
    }
}
impl Hardware
{
    fn configure(&mut self) -> Result<()>
    {
        self.resources.life.reset_complete = false;
        self.write(0x70, 3);
        self.write(0x14, 0);
        let low = self.read(0x10);
        self.write(0x14, 1);
        let high = self.read(0x10);
        validate_profile(u64::from(low) | (u64::from(high) << 32), self.read(0x104))
            .map_err(|_| Error(NOT_SUPPORTED))?;
        self.write(0x24, 0);
        self.write(0x20, 2);
        self.write(0x24, 1);
        self.write(0x20, 1);
        self.write(0x70, 11);
        if self.read(0x70) & 8 == 0
        {
            return Err(Error(BAD_ABI));
        }
        let cpu = self.arena.memory.get().cast::<u8>() as usize as u64;
        // Initial reset acknowledged; software may now clear and publish rings.
        unsafe {
            core::ptr::write_bytes(self.arena.memory.get().cast::<u8>(),
                                   0,
                                   size_of::<[QueueMemory; 4]>());
        }
        let mapping =
            self.resources
                .service
                .map(self.resources.life.claim, cpu, size_of::<[QueueMemory; 4]>() as u64)?;
        self.resources.life.mapping = mapping.handle;
        if mapping.length != size_of::<[QueueMemory; 4]>() as u64
           || mapping.handle == 0
           || mapping.address.checked_add(mapping.length).is_none()
        {
            return Err(Error(BAD_ABI));
        }
        self.dma = mapping.address;
        // From this point hardware may own resources again, including failed
        // queue configuration. A fresh reset is mandatory on every exit.
        self.resources.life.reset_complete = false;
        for queue in 0..4
        {
            self.write(0x30, hardware_queue(queue));
            if self.read(0x34) < N as u32 || self.read(0x44) != 0
            {
                return Err(Error(BAD_ABI));
            }
            self.write(0x38, N as u32);
            let pointer = self.queue_pointer(queue);
            unsafe {
                write_volatile(addr_of_mut!((*pointer).available.flags), 1u16.to_le());
            }
            let offset = queue as u64 * size_of::<QueueMemory>() as u64;
            self.address(0x80, self.dma + offset + offset_of!(QueueMemory, descriptors) as u64);
            self.address(0x90, self.dma + offset + offset_of!(QueueMemory, available) as u64);
            self.address(0xa0, self.dma + offset + offset_of!(QueueMemory, used) as u64);
            self.write(0x44, 1);
        }
        for queue in [0, 2]
        {
            for index in 0..N
            {
                self.post(queue, index, FRAGMENT, true)?;
            }
        }
        fence();
        self.write(0x70, 15);
        self.running = true;
        self.write(0x50, hardware_queue(0));
        self.write(0x50, 2);
        Ok(())
    }

    fn read(&self, offset: usize) -> u32
    {
        unsafe { u32::from_le(read_volatile((self.registers + offset) as *const u32)) }
    }

    fn write(&self, offset: usize, value: u32)
    {
        unsafe {
            write_volatile((self.registers + offset) as *mut u32, value.to_le());
        }
    }

    fn address(&self, offset: usize, address: u64)
    {
        self.write(offset, address as u32);
        self.write(offset + 4, (address >> 32) as u32);
    }

    fn queue_pointer(&self, queue: usize) -> *mut QueueMemory
    {
        // Every caller uses one of the four fixed queue numbers.
        unsafe { self.arena.memory.get().cast::<QueueMemory>().add(queue) }
    }

    fn buffer(&self, queue: usize, index: usize) -> *mut u8
    {
        unsafe {
            addr_of_mut!((*self.queue_pointer(queue)).buffers).cast::<u8>().add(index * FRAGMENT)
        }
    }

    fn post(&mut self, queue: usize, index: usize, length: usize, receive: bool) -> Result<()>
    {
        if queue >= 4 || index >= N || length > FRAGMENT || self.queues[queue].owned[index]
        {
            return Err(Error(BAD_ARGUMENT));
        }
        let pointer = self.queue_pointer(queue);
        let address = self.dma
                      + (queue * size_of::<QueueMemory>()
                         + offset_of!(QueueMemory, buffers)
                         + index * FRAGMENT) as u64;
        unsafe {
            write_volatile(addr_of_mut!((*pointer).descriptors[index]),
                           Descriptor { address: address.to_le(),
                                        length:  (length as u32).to_le(),
                                        flags:   if receive { 2u16.to_le() } else { 0 },
                                        next:    0 });
        }
        self.queues[queue].owned[index] = true;
        self.queues[queue].lengths[index] = length;
        let next = self.queues[queue].available;
        unsafe {
            write_volatile(addr_of_mut!((*pointer).available.ring[next as usize % N]),
                           (index as u16).to_le());
        }
        fence();
        self.queues[queue].available = next.wrapping_add(1);
        unsafe {
            write_volatile(addr_of_mut!((*pointer).available.index),
                           self.queues[queue].available.to_le());
        }
        fence();
        if self.running
        {
            self.write(0x50, hardware_queue(queue));
        }
        Ok(())
    }

    fn used(&mut self, queue: usize) -> Result<Option<(usize, usize)>>
    {
        let pointer = self.queue_pointer(queue);
        let index = unsafe { u16::from_le(read_volatile(addr_of_mut!((*pointer).used.index))) };
        let count = index.wrapping_sub(self.queues[queue].used);
        if count == 0
        {
            return Ok(None);
        }
        if count as usize > N
        {
            return Err(Error(BAD_ABI));
        }
        fence();
        let entry = unsafe {
            read_volatile(addr_of_mut!((*pointer).used.ring[self.queues[queue].used as usize % N]))
        };
        let id = u32::from_le(entry.id) as usize;
        let length = u32::from_le(entry.length) as usize;
        if id >= N || !self.queues[queue].owned[id] || length > self.queues[queue].lengths[id]
        {
            return Err(Error(BAD_ABI));
        }
        self.queues[queue].used = self.queues[queue].used.wrapping_add(1);
        self.queues[queue].owned[id] = false;
        Ok(Some((id, length)))
    }

    fn send_queue(&mut self, queue: usize, bytes: &[u8]) -> Result<usize>
    {
        if bytes.len() > FRAGMENT
        {
            return Err(Error(BAD_ARGUMENT));
        }
        let Some(index) = self.queues[queue].owned.iter().position(|owned| !owned)
        else
        {
            return Ok(0);
        };
        // The descriptor is software-owned and the engine's source is disjoint
        // from this DMA memory. Hardware cannot read until post publishes it.
        unsafe {
            copy_nonoverlapping(bytes.as_ptr(), self.buffer(queue, index), bytes.len());
        }
        self.post(queue, index, bytes.len(), false)?;
        Ok(bytes.len())
    }

    fn receive_queue(&mut self, queue: usize, output: &mut [u8]) -> Result<usize>
    {
        let Some((index, length)) = self.used(queue)?
        else
        {
            return Ok(0);
        };
        if length > FRAGMENT || length > output.len()
        {
            return Err(Error(BAD_ABI));
        }
        // Used publication returned the buffer to software. Repost only after
        // copying; there are no live references to memory owned by the device.
        unsafe {
            copy_nonoverlapping(self.buffer(queue, index), output.as_mut_ptr(), length);
        }
        self.post(queue, index, FRAGMENT, true)?;
        Ok(length)
    }

    fn progress(&mut self, name: &str) -> Result<Event>
    {
        if self.read(0x70) & 64 != 0
        {
            return Err(Error(IO_ERROR));
        }
        for queue in [1, 3]
        {
            for _ in 0..2
            {
                if self.used(queue)?.is_none()
                {
                    break;
                }
            }
        }
        let mut bytes = [0; FRAGMENT];
        for _ in 0..2
        {
            let length = self.receive_queue(2, &mut bytes)?;
            if length == 0
            {
                break;
            }
            match self.control.ingest(name, &bytes[..length]).map_err(|diagnostic| {
                                                                  Error(if diagnostic
                                                                           == Diagnostic::Profile
                                                                        {
                                                                            NOT_SUPPORTED
                                                                        }
                                                                        else
                                                                        {
                                                                            BAD_ABI
                                                                        })
                                                              })?
            {
                Event::Progress => (),
                event => return Ok(event)
            }
        }
        for _ in 0..2
        {
            let Some(record) = self.control.next()
            else
            {
                break;
            };
            if self.send_queue(3, &record)? == 0
            {
                break;
            }
            self.control.published();
        }
        let interrupts = self.read(0x60) & 3;
        if interrupts != 0
        {
            self.write(0x64, interrupts);
        }
        Ok(Event::Progress)
    }

    fn receive_bytes(&mut self, output: &mut [u8]) -> Result<usize>
    {
        if output.is_empty()
        {
            return Ok(0);
        }
        if self.receive.is_none()
        {
            let Some((index, length)) = self.used(0)?
            else
            {
                return Ok(0);
            };
            if length > FRAGMENT
            {
                return Err(Error(BAD_ABI));
            }
            self.receive = Some((index, length, 0));
        }
        let Some((index, length, offset)) = self.receive
        else
        {
            return Err(Error(BAD_ABI));
        };
        let count = output.len().min(length - offset);
        // Used publication returned ownership. The held descriptor is not
        // reposted until even a short caller buffer has consumed every byte.
        unsafe {
            copy_nonoverlapping(self.buffer(0, index).add(offset), output.as_mut_ptr(), count);
        }
        if offset + count == length
        {
            self.receive = None;
            self.post(0, index, FRAGMENT, true)?;
        }
        else
        {
            self.receive = Some((index, length, offset + count));
        }
        Ok(count)
    }
}
fn monotonic_microseconds() -> u64
{
    let ticks: u64;
    // SAFETY: side-effect-free virt machine 10 MHz monotonic counter.
    unsafe {
        core::arch::asm!("rdtime {ticks}",ticks=out(reg)ticks,options(nomem,nostack));
    }
    ticks / 10
}

/// A named connection searches the complete resident device inventory. Initial
/// discovery is asynchronous; no guest-open or byte I/O precedes exact identity.
pub struct Device
{
    service:    DeviceService,
    arena:      &'static Arena,
    name:       &'static str,
    discovery:  Discovery,
    hardware:   Option<Hardware>,
    diagnostic: Diagnostic,
    generation: u64,
    connected:  bool,
    terminal:   bool,
    refusal:    Option<Diagnostic>,
    live:       bool
}
impl Device
{
    pub fn initialize(client: Client, arena: &'static Arena, name: &'static str) -> Result<Self>
    {
        if name.is_empty() || name.as_bytes().contains(&0)
        {
            return Err(Error(BAD_ARGUMENT));
        }
        let service = DeviceService::new(client)?;
        let count = service.count()?;
        arena.acquire()?;
        Ok(Self { service,
                  arena,
                  name,
                  discovery: Discovery::new(count),
                  hardware: None,
                  diagnostic: Diagnostic::Discovering,
                  generation: 0,
                  connected: false,
                  terminal: false,
                  refusal: None,
                  live: true })
    }

    pub fn diagnostic(&self) -> Diagnostic
    {
        self.diagnostic
    }

    pub fn generation(&self) -> u64
    {
        self.generation
    }

    pub fn connected(&self) -> bool
    {
        self.connected
    }

    pub fn unavailable(&self) -> bool
    {
        self.terminal && self.hardware.is_none()
    }

    fn search(&mut self, now: u64) -> Result<bool>
    {
        let ordinal = match self.discovery.step(now)
        {
            Step::Probe(ordinal) => ordinal,
            Step::Wait => return Ok(false),
            Step::Refused(diagnostic) =>
            {
                self.diagnostic = diagnostic;
                self.terminal = true;
                self.refusal = Some(diagnostic);
                return Err(Error(NOT_SUPPORTED));
            }
            Step::Exhausted =>
            {
                self.terminal = true;
                self.refusal = Some(self.diagnostic);
                return Err(Error(NOT_SUPPORTED));
            }
        };
        let info = match self.service.probe(ordinal)
        {
            Ok(info) => info,
            Err(Error(NOT_FOUND)) =>
            {
                self.discovery.probed();
                return Ok(false);
            }
            Err(Error(SERVICE_BUSY)) =>
            {
                self.discovery.probe_busy(now);
                self.diagnostic = Diagnostic::Claimed;
                return Ok(false);
            }
            Err(error) =>
            {
                self.diagnostic = Diagnostic::Hardware;
                return Err(error);
            }
        };
        self.discovery.probed();
        if info.bus.0 != *b"VMMI" || info.key[..4] != 3u32.to_le_bytes()
        {
            return Ok(false);
        }
        if info.register_bytes != 4096
           || info.flags & (interface::COHERENT_DMA | interface::MMIO)
              != interface::COHERENT_DMA | interface::MMIO
        {
            self.diagnostic = Diagnostic::Profile;
            return Ok(false);
        }
        let claim = match self.service.claim(ordinal)
        {
            Ok(claim) => claim,
            Err(Error(SERVICE_BUSY)) =>
            {
                self.discovery.claim_busy();
                self.diagnostic = Diagnostic::Claimed;
                return Ok(false);
            }
            Err(error) =>
            {
                self.diagnostic = Diagnostic::Hardware;
                return Err(error);
            }
        };
        // The claim is recorded before any further fallible operation.
        let mut hardware = Hardware { resources: Resources { service: self.service,
                                                             life:    Lifetime::new(claim.handle) },
                                      arena: self.arena,
                                      ordinal,
                                      registers: 0,
                                      dma: 0,
                                      queues: [Queue::new(); 4],
                                      running: false,
                                      control: Control::new(),
                                      receive: None,
                                      closing: false,
                                      restart: false };
        match usize::try_from(info.registers)
        {
            Ok(registers) if registers % 4 == 0 => hardware.registers = registers,
            _ =>
            {
                hardware.closing = true;
                self.diagnostic = Diagnostic::Profile;
            }
        }
        self.hardware = Some(hardware);
        Ok(false)
    }

    /// Flush a real connection before reusing any protocol correlation. Keep
    /// the arena exclusively admitted, and retain cleanup authority on refusal.
    /// Success means old queues and mappings have actually retired; later poll
    /// starts discovery and can publish only a new checked generation.
    pub fn reset_connection(&mut self) -> Result<bool>
    {
        if !self.live
        {
            return Err(Error(NOT_SUPPORTED));
        }
        self.connected = false;
        if let Some(hardware) = self.hardware.as_mut()
        {
            if !hardware.closing
            {
                hardware.closing = true;
                if hardware.running
                {
                    hardware.resources.life.reset_complete = false;
                }
            }
            if !hardware.resources.cleanup()?
            {
                return Ok(false);
            }
            self.hardware = None;
        }
        let count = self.service.count()?;
        self.discovery = Discovery::new(count);
        self.terminal = false;
        self.refusal = None;
        self.diagnostic = Diagnostic::Discovering;
        Ok(true)
    }

    pub fn quiesce(&mut self) -> Result<()>
    {
        if !self.live
        {
            return Ok(());
        }
        self.connected = false;
        if let Some(hardware) = self.hardware.as_mut()
        {
            if !hardware.closing
            {
                hardware.closing = true;
                if hardware.running
                {
                    hardware.resources.life.reset_complete = false;
                }
            }
            let complete = hardware.resources.cleanup().map_err(|error| {
                                                            self.diagnostic = Diagnostic::Cleanup;
                                                            error
                                                        })?;
            if !complete
            {
                self.diagnostic = Diagnostic::Cleanup;
                return Err(Error(SERVICE_BUSY));
            }
        }
        self.hardware = None;
        self.live = false;
        self.arena.state.store(0, Ordering::Release);
        Ok(())
    }
}
impl Drop for Device
{
    fn drop(&mut self)
    {
        if !self.live || self.quiesce().is_ok()
        {
            return;
        }
        if let Some(hardware) = self.hardware.take()
        {
            // SAFETY: this Device still exclusively owns arena state 1. The
            // exact reset ticket, claim and DMA mapping survive failed Drop.
            unsafe {
                *self.arena.retained.get() = Some(hardware.resources);
            }
            self.arena.state.store(2, Ordering::Release);
        }
    }
}
impl Channel for Device
{
    fn monotonic_microseconds(&self) -> u64
    {
        monotonic_microseconds()
    }

    fn poll(&mut self) -> Result<bool>
    {
        if !self.live
        {
            return Err(Error(IO_ERROR));
        }
        // Closing hardware always makes progress before terminal discovery
        // refusal. A deadline never discards an outstanding reset ticket.
        if !self.hardware.as_ref().is_some_and(|hardware| hardware.closing)
        {
            if self.terminal
            {
                return Err(Error(NOT_SUPPORTED));
            }
            let now = monotonic_microseconds();
            if self.discovery.expired(now)
            {
                self.diagnostic = if self.hardware.as_ref().is_some_and(|hardware| hardware.running)
                {
                    Diagnostic::NameTimeout
                }
                else
                {
                    self.discovery.timeout_diagnostic()
                };
                self.terminal = true;
                self.refusal = Some(self.diagnostic);
                if let Some(hardware) = self.hardware.as_mut()
                {
                    hardware.closing = true;
                }
                else
                {
                    return Err(Error(NOT_SUPPORTED));
                }
            }
            if self.hardware.is_none()
            {
                if self.diagnostic == Diagnostic::Discovering
                {
                    self.diagnostic = Diagnostic::NoController;
                }
                return self.search(now);
            }
        }
        let hardware = self.hardware.as_mut().ok_or(Error(BAD_ABI))?;
        if hardware.closing
        {
            if self.refusal.is_none()
            {
                self.refusal = Some(self.diagnostic);
            }
            match hardware.resources.cleanup()
            {
                Ok(true) =>
                {
                    if hardware.restart
                    {
                        self.discovery.reconnect(hardware.ordinal);
                    }
                    self.hardware = None;
                    if let Some(refusal) = self.refusal.take()
                    {
                        self.diagnostic = refusal;
                    }
                    if self.terminal
                    {
                        return Err(Error(NOT_SUPPORTED));
                    }
                    return Ok(false);
                }
                Ok(false) =>
                {
                    self.diagnostic = Diagnostic::Cleanup;
                    return Err(Error(SERVICE_BUSY));
                }
                Err(error) =>
                {
                    self.diagnostic = Diagnostic::Cleanup;
                    return Err(error);
                }
            }
        }
        if !hardware.running
        {
            let reset = hardware.resources
                                .life
                                .reset(&mut hardware.resources.service)
                                .map_err(|error| {
                                    self.diagnostic = Diagnostic::Cleanup;
                                    error
                                })?;
            if !reset
            {
                return Ok(false);
            }
            if let Err(error) = hardware.configure()
            {
                hardware.closing = true;
                self.diagnostic = if error == Error(NOT_SUPPORTED)
                {
                    Diagnostic::Profile
                }
                else
                {
                    Diagnostic::Hardware
                };
                return Ok(false);
            }
        }
        let event = match hardware.progress(self.name)
        {
            Ok(event) => event,
            Err(error) =>
            {
                hardware.closing = true;
                self.connected = false;
                self.diagnostic = if error == Error(NOT_SUPPORTED)
                {
                    Diagnostic::Profile
                }
                else if error == Error(BAD_ABI)
                {
                    Diagnostic::MalformedControl
                }
                else
                {
                    Diagnostic::Hardware
                };
                if hardware.control.identified()
                {
                    self.terminal = true;
                    self.refusal = Some(self.diagnostic);
                }
                return Err(error);
            }
        };
        match event
        {
            Event::RejectName =>
            {
                hardware.closing = true;
                self.diagnostic = Diagnostic::NameMismatch;
                return Ok(false);
            }
            Event::Disconnected =>
            {
                hardware.closing = true;
                hardware.restart = true;
                self.connected = false;
                self.diagnostic = Diagnostic::Disconnected;
                return Err(Error(IO_ERROR));
            }
            Event::Progress => ()
        }
        if hardware.control.identified()
        {
            self.discovery.identified();
        }
        let connected = hardware.control.connected();
        if connected && !self.connected
        {
            let Some(generation) = self.generation.checked_add(1)
            else
            {
                hardware.closing = true;
                self.terminal = true;
                self.refusal = Some(Diagnostic::EpochExhausted);
                self.diagnostic = Diagnostic::EpochExhausted;
                return Err(Error(IO_ERROR));
            };
            self.generation = generation;
            self.diagnostic = Diagnostic::Ready;
        }
        self.connected = connected;
        Ok(connected)
    }

    fn send(&mut self, bytes: &[u8]) -> Result<usize>
    {
        if bytes.len() > FRAGMENT
        {
            return Err(Error(BAD_ARGUMENT));
        }
        if !self.connected
        {
            return Err(Error(SERVICE_BUSY));
        }
        let hardware = self.hardware.as_mut().ok_or(Error(IO_ERROR))?;
        let result = hardware.send_queue(1, bytes);
        if result.is_err()
        {
            hardware.closing = true;
            self.connected = false;
            self.diagnostic = Diagnostic::Hardware;
            self.terminal = true;
            self.refusal = Some(self.diagnostic);
        }
        result
    }

    fn receive(&mut self, bytes: &mut [u8]) -> Result<usize>
    {
        if !self.connected
        {
            return Err(Error(SERVICE_BUSY));
        }
        let hardware = self.hardware.as_mut().ok_or(Error(IO_ERROR))?;
        let result = hardware.receive_bytes(bytes);
        if result.is_err()
        {
            hardware.closing = true;
            self.connected = false;
            self.diagnostic = Diagnostic::Hardware;
            self.terminal = true;
            self.refusal = Some(self.diagnostic);
        }
        result
    }
}

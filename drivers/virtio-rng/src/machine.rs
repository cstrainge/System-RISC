//! Audited modern coherent VirtIO MMIO RNG boundary (VirtIO 1.3 section 5.4).
//! One queue and its payload fit in one pinned module page. Device-shared
//! storage is accessed only by raw pointers; no Rust reference aliases DMA.
#![allow(unsafe_code)]

use core::{cell::UnsafeCell,
           fmt::Write,
           mem::{offset_of, size_of},
           ptr::{addr_of_mut, copy_nonoverlapping, read_volatile, write_volatile}};

use mac_abi::{BAD_ABI, BAD_ARGUMENT, IO_ERROR, NOT_FOUND, NOT_SUPPORTED, SERVICE_BUSY};
use mac_driver::{Client, Completion, Error, Phase, Request, Result,
                 devices::{DeviceService, interface}};
use mac_entropy_interface as entropy;

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
    ring:  [u16; 1],
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
    ring:  [UsedEntry; 1],
    event: u16
}
#[repr(C, align(4096))]
struct Memory
{
    descriptor: Descriptor,
    available:  Available,
    used:       Used,
    bytes:      [u8; entropy::MAX_TRANSFER]
}
const _: () = assert!(size_of::<Memory>() == 4096);
struct Arena(UnsafeCell<Memory>);
// SAFETY: the module's unique Device is protected by DEVICE. Quiesce removes
// that Device only after reset/unmap/release. No other path acquires this arena.
unsafe impl Sync for Arena
{
}
static ARENA: Arena = Arena(UnsafeCell::new(Memory { descriptor: Descriptor { address: 0,
                                                                              length:  0,
                                                                              flags:   0,
                                                                              next:    0 },
                                                     available:  Available { flags: 0,
                                                                             index: 0,
                                                                             ring:  [0],
                                                                             event: 0 },
                                                     used:       Used { flags: 0,
                                                                        index: 0,
                                                                        ring:
                                                                            [UsedEntry { id:     0,
                                                                                         length: 0 }],
                                                                        event: 0 },
                                                     bytes:      [0; entropy::MAX_TRANSFER] }));

#[derive(Clone, Copy)]
enum Diagnostic
{
    Discovering,
    NoDevice,
    Profile,
    Configuring,
    Waiting,
    Ready,
    Queue,
    Cleanup
}
impl Diagnostic
{
    fn text(self) -> &'static str
    {
        match self
        {
            Self::Discovering => "Discovering an admitted VirtIO entropy device",
            Self::NoDevice =>
            {
                "No available modern coherent VirtIO MMIO entropy device; configure an RNG device \
                 and retry"
            }
            Self::Profile =>
            {
                "The entropy device does not support the required modern coherent VirtIO MMIO \
                 queue profile"
            }
            Self::Configuring => "Waiting for entropy device reset and queue initialization",
            Self::Waiting =>
            {
                "Waiting for an actual entropy device completion; no bytes have been fabricated"
            }
            Self::Ready =>
            {
                "The entropy device has supplied bytes; future source failures remain possible"
            }
            Self::Queue =>
            {
                "Entropy device status or used-ring completion was malformed; retiring the \
                 original DMA mapping"
            }
            Self::Cleanup =>
            {
                "Entropy reset, DMA unmap or claim release remains incomplete; the original \
                 storage and cleanup ticket remain retained"
            }
        }
    }
}

struct Hardware
{
    claim:          u64,
    mapping:        u64,
    reset_ticket:   u64,
    reset_complete: bool,
    registers:      usize,
    dma:            u64,
    running:        bool,
    device_owns:    bool,
    offered:        u16,
    consumed:       u16,
    capacity:       usize
}
impl Hardware
{
    fn cleanup_stage(&self) -> &'static str
    {
        if !self.reset_complete
        {
            "reset"
        }
        else if self.mapping != 0
        {
            "unmap"
        }
        else
        {
            "release"
        }
    }

    fn reset(&mut self, service: DeviceService) -> Result<bool>
    {
        if self.claim == 0 || self.reset_complete
        {
            return Ok(true);
        }
        let phase = if self.reset_ticket == 0 { Phase::Start } else { Phase::Poll };
        match service.reset(self.claim, &mut self.reset_ticket, phase)?
        {
            Completion::Complete(0) if self.reset_ticket == 0 =>
            {
                self.reset_complete = true;
                self.running = false;
                self.device_owns = false;
                Ok(true)
            }
            Completion::Pending if self.reset_ticket != 0 => Ok(false),
            _ => Err(Error(BAD_ABI))
        }
    }

    fn cleanup(&mut self, service: DeviceService) -> Result<bool>
    {
        if !self.reset(service)?
        {
            return Ok(false);
        }
        if self.mapping != 0
        {
            service.unmap(self.mapping)?;
            self.mapping = 0;
        }
        if self.claim != 0
        {
            service.release(self.claim)?;
            self.claim = 0;
        }
        Ok(true)
    }

    fn read(&self, offset: usize) -> u32
    {
        // SAFETY: this aligned offset lies in the exclusively claimed 4 KiB
        // MMIO window. All callers use the modern transport's fixed registers.
        unsafe { u32::from_le(read_volatile((self.registers + offset) as *const u32)) }
    }

    fn write(&self, offset: usize, value: u32)
    {
        // SAFETY: same validated, exclusively claimed MMIO window as read().
        unsafe { write_volatile((self.registers + offset) as *mut u32, value.to_le()) };
        fence();
    }

    fn address(&self, offset: usize, value: u64)
    {
        self.write(offset, value as u32);
        self.write(offset + 4, (value >> 32) as u32);
    }

    fn healthy(&self) -> bool
    {
        self.running && self.read(0x70) == 15
    }

    fn configure(&mut self, service: DeviceService) -> Result<bool>
    {
        if !self.reset(service)?
        {
            return Ok(false);
        }
        if self.read(0) != 0x7472_6976 || self.read(4) != 2 || self.read(8) != 4
        {
            return Err(Error(NOT_SUPPORTED));
        }
        // Status/queue writes can give the device ownership again. Every exit
        // after this point needs a fresh acknowledged reset before unmapping.
        self.reset_complete = false;
        self.write(0x70, 1);
        self.write(0x70, 3);
        self.write(0x14, 1);
        if self.read(0x10) & 1 == 0
        {
            return Err(Error(NOT_SUPPORTED));
        }
        // No entropy-specific features; negotiate only VIRTIO_F_VERSION_1.
        self.write(0x24, 0);
        self.write(0x20, 0);
        self.write(0x24, 1);
        self.write(0x20, 1);
        self.write(0x70, 11);
        if self.read(0x70) != 11
        {
            return Err(Error(NOT_SUPPORTED));
        }
        // SAFETY: acknowledged initial reset ended device ownership. The
        // unique Device owns the complete pinned arena, including padding.
        unsafe { core::ptr::write_bytes(ARENA.0.get().cast::<u8>(), 0, size_of::<Memory>()) };
        let mapping =
            service.map(self.claim, ARENA.0.get() as usize as u64, size_of::<Memory>() as u64)?;
        // Retain the exact handle before validating any returned property.
        self.mapping = mapping.handle;
        if mapping.length != size_of::<Memory>() as u64
           || mapping.address % 4096 != 0
           || mapping.address.checked_add(mapping.length).is_none()
        {
            return Err(Error(BAD_ABI));
        }
        self.dma = mapping.address;
        self.write(0x30, 0);
        if self.read(0x34) < 1 || self.read(0x44) != 0
        {
            return Err(Error(NOT_SUPPORTED));
        }
        self.write(0x38, 1);
        // Suppress notifications; the existing bounded broker pump polls this
        // device. No application spin loop or replacement worker is installed.
        unsafe { write_volatile(addr_of_mut!((*ARENA.0.get()).available.flags), 1u16.to_le()) };
        self.address(0x80, self.dma + offset_of!(Memory, descriptor) as u64);
        self.address(0x90, self.dma + offset_of!(Memory, available) as u64);
        self.address(0xa0, self.dma + offset_of!(Memory, used) as u64);
        fence();
        self.write(0x44, 1);
        self.write(0x70, 15);
        self.running = true;
        self.offered = 0;
        self.consumed = 0;
        if !self.healthy()
        {
            return Err(Error(IO_ERROR));
        }
        Ok(true)
    }

    fn post(&mut self, capacity: usize) -> Result<()>
    {
        if self.device_owns || capacity == 0 || capacity > entropy::MAX_TRANSFER
        {
            return Err(Error(BAD_ABI));
        }
        if !self.healthy()
        {
            return Err(Error(IO_ERROR));
        }
        let pointer = ARENA.0.get();
        self.capacity = capacity;
        self.device_owns = true;
        // SAFETY: the descriptor is not outstanding. Only the device-writable
        // payload is published; output slices never become DMA destinations.
        unsafe {
            write_volatile(addr_of_mut!((*pointer).descriptor),
                           Descriptor { address:
                                            (self.dma + offset_of!(Memory, bytes) as u64).to_le(),
                                        length:  (capacity as u32).to_le(),
                                        flags:   2u16.to_le(),
                                        next:    0 });
            write_volatile(addr_of_mut!((*pointer).available.ring[0]), 0);
        }
        fence();
        self.offered = self.offered.wrapping_add(1);
        unsafe { write_volatile(addr_of_mut!((*pointer).available.index), self.offered.to_le()) };
        fence();
        self.write(0x50, 0);
        Ok(())
    }

    fn receive(&mut self, output: &mut [u8], discard: bool) -> Result<Option<usize>>
    {
        if !self.device_owns || !self.healthy()
        {
            return Err(Error(IO_ERROR));
        }
        let pointer = ARENA.0.get();
        // SAFETY: volatile device-shared reads; the publication fence below
        // orders the payload after the observed used-index ownership transfer.
        let next = unsafe { u16::from_le(read_volatile(addr_of_mut!((*pointer).used.index))) };
        if next == self.consumed
        {
            return Ok(None);
        }
        fence();
        if next.wrapping_sub(self.consumed) != 1
        {
            return Err(Error(IO_ERROR));
        }
        let entry = unsafe { read_volatile(addr_of_mut!((*pointer).used.ring[0])) };
        let count = u32::from_le(entry.length) as usize;
        if u32::from_le(entry.id) != 0
           || count == 0
           || count > self.capacity
           || (!discard && count > output.len())
        {
            return Err(Error(IO_ERROR));
        }
        // SAFETY: the valid used entry returned this one descriptor to us.
        // The exclusively borrowed caller output is disjoint from module DMA.
        unsafe {
            let bytes = addr_of_mut!((*pointer).bytes).cast::<u8>();
            if !discard
            {
                copy_nonoverlapping(bytes, output.as_mut_ptr(), count);
            }
            for index in 0..count
            {
                write_volatile(bytes.add(index), 0);
            }
        }
        self.consumed = next;
        self.device_owns = false;
        let interrupts = self.read(0x60) & 3;
        if interrupts != 0
        {
            self.write(0x64, interrupts);
        }
        Ok(Some(count))
    }
}

fn fence()
{
    // Required device/CPU publication ordering, not diagnostic instrumentation.
    unsafe { core::arch::asm!("fence iorw, iorw", options(nostack)) };
}

#[derive(Clone, Copy)]
struct Work
{
    ticket:    u64,
    operation: u32,
    capacity:  usize,
    cancelled: bool,
    failure:   Option<Error>
}

pub(super) struct Device
{
    service:         DeviceService,
    slots:           u32,
    ordinal:         u32,
    hardware:        Option<Hardware>,
    work:            Option<Work>,
    ready:           bool,
    diagnostic:      Diagnostic,
    failure:         Option<Error>,
    cleanup_failure: Option<(&'static str, Error)>
}
impl Device
{
    pub(super) fn new(host: Client) -> Result<Self>
    {
        let service = DeviceService::new(host)?;
        Ok(Self { service,
                  slots: service.count()?,
                  ordinal: 0,
                  hardware: None,
                  work: None,
                  ready: false,
                  diagnostic: Diagnostic::Discovering,
                  failure: None,
                  cleanup_failure: None })
    }

    fn discover(&mut self) -> Result<bool>
    {
        if self.ordinal >= self.slots
        {
            self.diagnostic = Diagnostic::NoDevice;
            return Err(Error(NOT_FOUND));
        }
        let ordinal = self.ordinal;
        let description = match self.service.probe(ordinal)
        {
            Ok(value) => value,
            Err(Error(NOT_FOUND)) =>
            {
                self.ordinal += 1;
                return Ok(false);
            }
            Err(error) => return Err(error)
        };
        self.ordinal += 1;
        if description.bus.0 != *b"VMMI" || description.key[..4] != 4u32.to_le_bytes()
        {
            return Ok(false);
        }
        self.diagnostic = Diagnostic::Profile;
        let required = interface::COHERENT_DMA | interface::MMIO;
        if description.register_bytes != 4096
           || description.registers == 0
           || description.registers % 4 != 0
           || description.flags != required
        {
            return Err(Error(NOT_SUPPORTED));
        }
        let registers = usize::try_from(description.registers).map_err(|_| Error(BAD_ABI))?;
        let claim = self.service.claim(ordinal)?;
        // Record every acquired right before any further fallible validation.
        self.hardware = Some(Hardware { claim: claim.handle,
                                        mapping: 0,
                                        reset_ticket: 0,
                                        reset_complete: false,
                                        registers,
                                        dma: 0,
                                        running: false,
                                        device_owns: false,
                                        offered: 0,
                                        consumed: 0,
                                        capacity: 0 });
        if claim.bus != description.bus
           || claim.key != description.key
           || claim.registers != description.registers
           || claim.register_bytes != description.register_bytes
           || claim.flags != description.flags
        {
            return Err(Error(BAD_ABI));
        }
        self.diagnostic = Diagnostic::Configuring;
        Ok(false)
    }

    fn progress(&mut self, work: Work, output: &mut [u8]) -> Result<Option<usize>>
    {
        let Some(hardware) = self.hardware.as_mut()
        else
        {
            self.discover()?;
            return Ok(None);
        };
        if !hardware.running && !hardware.configure(self.service)?
        {
            return Ok(None);
        }
        self.diagnostic = Diagnostic::Waiting;
        if !hardware.device_owns
        {
            hardware.post(if work.operation == entropy::READY { 16 } else { work.capacity })?;
            return Ok(None);
        }
        let count = hardware.receive(output, work.operation == entropy::READY)?;
        if count.is_some()
        {
            self.ready = true;
            self.diagnostic = Diagnostic::Ready;
        }
        Ok(count.map(|n| if work.operation == entropy::READY { 0 } else { n }))
    }

    fn cleanup(&mut self) -> Result<bool>
    {
        self.ready = false;
        if let Some(hardware) = self.hardware.as_mut()
        {
            match hardware.cleanup(self.service)
            {
                Ok(false) => return Ok(false),
                Ok(true) => (),
                Err(error) =>
                {
                    self.cleanup_failure.get_or_insert((hardware.cleanup_stage(), error));
                    return Err(error);
                }
            }
        }
        self.hardware = None;
        self.ordinal = 0;
        Ok(true)
    }

    pub(super) fn quiesce(&mut self) -> Result<()>
    {
        self.diagnostic = Diagnostic::Cleanup;
        if !self.cleanup()?
        {
            return Err(Error(SERVICE_BUSY));
        }
        self.work = None;
        Ok(())
    }

    pub(super) fn request(&mut self, request: Request<'_>, output: &mut [u8])
                          -> Result<Completion>
    {
        if request.class != entropy::CLASS
           || !matches!(request.operation, entropy::READY | entropy::FILL | entropy::ERROR_TEXT)
        {
            return Err(Error(NOT_SUPPORTED));
        }
        if request.ticket == 0 || !request.input.is_empty()
        {
            return Err(Error(BAD_ARGUMENT));
        }
        if request.phase == Phase::Cancel
        {
            if let Some(work) = self.work.as_mut().filter(|w| w.ticket == request.ticket)
            {
                work.cancelled = true;
            }
            else
            {
                // This ticket has not acquired hardware; another live ticket
                // retains its own work and must not be cancelled here.
                return Ok(Completion::Complete(0));
            }
        }
        else if request.operation == entropy::ERROR_TEXT
        {
            struct Text
            {
                bytes:  [u8; entropy::MAX_ERROR_TEXT],
                length: usize
            }
            impl core::fmt::Write for Text
            {
                fn write_str(&mut self, text: &str) -> core::fmt::Result
                {
                    let end = self.length.checked_add(text.len()).ok_or(core::fmt::Error)?;
                    let destination =
                        self.bytes.get_mut(self.length..end).ok_or(core::fmt::Error)?;
                    destination.copy_from_slice(text.as_bytes());
                    self.length = end;
                    Ok(())
                }
            }
            let mut text = Text { bytes:  [0; entropy::MAX_ERROR_TEXT],
                                  length: 0 };
            text.write_str(self.diagnostic.text()).map_err(|_| Error(BAD_ABI))?;
            if let Some(error) = self.failure
            {
                write!(text, "; request status={}", error.0).map_err(|_| Error(BAD_ABI))?;
            }
            if let Some((stage, error)) = self.cleanup_failure
            {
                write!(text, "; {stage} status={}", error.0).map_err(|_| Error(BAD_ABI))?;
            }
            if output.len() < text.length || output.len() > entropy::MAX_ERROR_TEXT
            {
                return Err(Error(BAD_ARGUMENT));
            }
            output[..text.length].copy_from_slice(&text.bytes[..text.length]);
            return Ok(Completion::Complete(text.length));
        }
        else if (request.operation == entropy::READY && !output.is_empty())
                  || (request.operation == entropy::FILL
                      && (output.is_empty() || output.len() > entropy::MAX_TRANSFER))
        {
            return Err(Error(BAD_ARGUMENT));
        }
        if let Some(work) = self.work
        {
            if work.ticket != request.ticket
            {
                // The broker owns this waiting ticket. No caller buffers or
                // additional device work are retained by this provider.
                return Ok(Completion::Pending);
            }
        }
        else
        {
            if request.operation == entropy::READY
               && self.ready
               && self.hardware.as_ref().is_some_and(Hardware::healthy)
            {
                return Ok(Completion::Complete(0));
            }
            self.work = Some(Work { ticket:    request.ticket,
                                    operation: request.operation,
                                    capacity:  output.len(),
                                    cancelled: false,
                                    failure:   None });
            self.failure = None;
            self.cleanup_failure = None;
        }
        // The branch above always installs or retains this exact work record.
        let Some(mut work) = self.work
        else
        {
            return Err(Error(BAD_ABI));
        };
        if !work.cancelled && work.failure.is_none()
        {
            if work.operation != request.operation || work.capacity != output.len()
            {
                work.failure = Some(Error(BAD_ABI));
            }
            else
            {
                match self.progress(work, output)
                {
                    Ok(Some(count)) =>
                    {
                        self.work = None;
                        return Ok(Completion::Complete(count));
                    }
                    Ok(None) => return Ok(Completion::Pending),
                    Err(error) => work.failure = Some(error)
                }
            }
        }
        // A failed used entry or cancellation cannot return storage to a new
        // request until reset acknowledgement. Keep original work and causes.
        if work.failure.is_some()
           && matches!(self.diagnostic, Diagnostic::Waiting | Diagnostic::Ready)
        {
            self.diagnostic = Diagnostic::Queue;
        }
        self.work = Some(work);
        if self.failure.is_none()
        {
            self.failure = work.failure;
        }
        match self.cleanup()
        {
            Ok(true) =>
            {
                self.work = None;
                match work.failure
                {
                    Some(error) if !work.cancelled => Err(error),
                    _ => Ok(Completion::Complete(0))
                }
            }
            Ok(false) => Ok(Completion::Pending),
            Err(_) =>
            {
                // No forged terminal cancellation ACK. ERROR_TEXT and module
                // quiescence expose the retained cleanup failure; storage lives.
                self.diagnostic = Diagnostic::Cleanup;
                Ok(Completion::Pending)
            }
        }
    }
}

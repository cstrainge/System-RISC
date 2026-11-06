//! Audited modern coherent VirtIO MMIO GPU boundary (VirtIO 1.3 section 5.7,
//! 2D commands only). One control queue and its command/response buffers fit
//! in one pinned image page. Framebuffers are runtime Device-service allocations
//! charged to this module, with GPU completion and System CPU-view pins governing
//! release. No maximum-sized image/BSS framebuffer is embedded.
//! Device-shared storage is accessed only through raw pointers; no Rust
//! reference aliases DMA.
#![allow(unsafe_code)]

use core::{cell::UnsafeCell,
           mem::{offset_of, size_of},
           ptr::{addr_of_mut, copy_nonoverlapping, read_volatile, write_volatile}};

use mac_abi::{BAD_ABI, IO_ERROR, NOT_FOUND, NOT_SUPPORTED, SERVICE_BUSY};
use mac_driver::{Client, Completion, Error, Phase, Result,
                 devices::{DeviceService, interface}};
use mac_video_interface::{Attach, Damage, Outcome, Prepared, Size, Surface};

use crate::Hardware;

const QUEUE: usize = 8;
/// Command completion deadline; the host services the queue from its main loop.
const COMMAND_TIMEOUT_MICROSECONDS: u64 = 2_000_000;
const RESET_TIMEOUT_MICROSECONDS: u64 = 1_000_000;

const GET_DISPLAY_INFO: u32 = 0x100;
const RESOURCE_CREATE_2D: u32 = 0x101;
const RESOURCE_UNREF: u32 = 0x102;
const SET_SCANOUT: u32 = 0x103;
const RESOURCE_FLUSH: u32 = 0x104;
const TRANSFER_TO_HOST_2D: u32 = 0x105;
const RESOURCE_ATTACH_BACKING: u32 = 0x106;
const OK_NODATA: u32 = 0x1100;
const OK_DISPLAY_INFO: u32 = 0x1101;
/// 32-bit B8G8R8X8, the framebuffer's XRGB32 layout.
const FORMAT_XRGB: u32 = 2;

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
    ring:  [u16; QUEUE],
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
    ring:  [UsedEntry; QUEUE],
    event: u16
}
const COMMAND_BYTES: usize = 128;
const RESPONSE_BYTES: usize = 512;
#[repr(C, align(4096))]
struct Control
{
    descriptors: [Descriptor; QUEUE],
    available:   Available,
    used:        Used,
    command:     [u8; COMMAND_BYTES],
    response:    [u8; RESPONSE_BYTES]
}
const _: () = assert!(size_of::<Control>() == 4096);
struct Shared<T>(UnsafeCell<T>);
// SAFETY: the module's unique Device is protected by DEVICE in lib.rs. Quiesce
// removes that Device only after reset/unmap/release. No other path touches
// this control page. Framebuffer storage is allocated separately through VDEV
// and remains owned/pinned until actual GPU and CPU retirement.
unsafe impl<T> Sync for Shared<T>
{
}
static CONTROL: Shared<Control> =
    Shared(UnsafeCell::new(Control { descriptors: [Descriptor { address: 0,
                                                               length:  0,
                                                               flags:   0,
                                                               next:    0 };
                                                   QUEUE],
                                     available:   Available { flags: 0,
                                                              index: 0,
                                                              ring:  [0; QUEUE],
                                                              event: 0 },
                                     used:        Used { flags: 0,
                                                         index: 0,
                                                         ring:  [UsedEntry { id:     0,
                                                                            length: 0 };
                                                                 QUEUE],
                                                         event: 0 },
                                     command:     [0; COMMAND_BYTES],
                                     response:    [0; RESPONSE_BYTES] }));


/// Little-endian command builder over a fixed stack buffer.
struct Command
{
    bytes:  [u8; COMMAND_BYTES],
    length: usize
}
impl Command
{
    fn new(kind: u32) -> Self
    {
        let mut command = Self { bytes:  [0; COMMAND_BYTES],
                                 length: 0 };
        command.word(kind);
        command.word(0);
        command.quad(0);
        command.word(0);
        command.word(0);
        command
    }

    fn word(&mut self, value: u32)
    {
        self.bytes[self.length..self.length + 4].copy_from_slice(&value.to_le_bytes());
        self.length += 4;
    }

    fn quad(&mut self, value: u64)
    {
        self.bytes[self.length..self.length + 8].copy_from_slice(&value.to_le_bytes());
        self.length += 8;
    }

    fn rect(&mut self, x: u32, y: u32, width: u32, height: u32)
    {
        for value in [x, y, width, height]
        {
            self.word(value);
        }
    }
}

#[derive(Clone, Copy)]
struct Frame
{
    generation:        u64,
    memory:            interface::Allocation,
    size:              Size,
    resource:          u32,
    created:           bool,
    released:          bool,
    release_requested: bool,
    offered_to_system: bool
}
impl Frame
{
    fn surface(self, preferred: Size) -> Surface
    {
        Surface { generation: self.generation,
                  allocation: self.memory.handle,
                  address: self.memory.cpu_address,
                  capacity: self.memory.length,
                  preferred }
    }
}

pub(super) struct Device
{
    host:                  Client,
    service:               DeviceService,
    claim:                 u64,
    control_map:           u64,
    invocation_generation: u64,
    allocation_ticket:     u64,
    allocation_bytes:      u64,
    cancelled_allocation:  Option<interface::Allocation>,
    active:                Option<Frame>,
    candidate:             Option<Frame>,
    retired:               Option<Frame>,
    last_released:         Option<(u64, u64)>,
    last_aborted:          Option<(u64, u64)>,
    reset_ticket:          u64,
    reset_complete:        bool,
    registers:             usize,
    control_dma:           u64,
    running:               bool,
    wedged:                bool,
    offered:               u16,
    consumed:              u16,
    current:               Size,
    resource:              u32
}
impl Device
{
    pub(super) fn new(host: Client) -> Result<Self>
    {
        Ok(Self { host,
                  service: DeviceService::new(host)?,
                  claim: 0,
                  control_map: 0,
                  invocation_generation: 0,
                  allocation_ticket: 0,
                  allocation_bytes: 0,
                  cancelled_allocation: None,
                  active: None,
                  candidate: None,
                  retired: None,
                  last_released: None,
                  last_aborted: None,
                  reset_ticket: 0,
                  reset_complete: false,
                  registers: 0,
                  control_dma: 0,
                  running: false,
                  wedged: false,
                  offered: 0,
                  consumed: 0,
                  current: Size { width:  0,
                                  height: 0 },
                  resource: 0 })
    }

    fn read(&self, offset: usize) -> u32
    {
        // SAFETY: this aligned offset lies in the exclusively claimed 4 KiB
        // MMIO window (config space included). Callers use fixed modern
        // transport registers or the GPU config registers.
        unsafe { u32::from_le(read_volatile((self.registers + offset) as *const u32)) }
    }

    fn write(&self, offset: usize, value: u32)
    {
        // SAFETY: same validated, exclusively claimed MMIO window as read().
        unsafe { write_volatile((self.registers + offset) as *mut u32, value.to_le()) };
        mac_driver::arch::barrier();
    }

    fn address(&self, offset: usize, value: u64)
    {
        self.write(offset, value as u32);
        self.write(offset + 4, (value >> 32) as u32);
    }

    fn now(&self) -> Result<u64>
    {
        self.host.monotonic_microseconds()
    }

    /// Request and wait for acknowledged reset. Never grants reclamation
    /// without positive acknowledgement.
    fn reset(&mut self) -> Result<()>
    {
        if self.claim == 0 || self.reset_complete
        {
            return Ok(());
        }
        let deadline = self.now()?.checked_add(RESET_TIMEOUT_MICROSECONDS).ok_or(Error(BAD_ABI))?;
        loop
        {
            let phase = if self.reset_ticket == 0 { Phase::Start } else { Phase::Poll };
            match self.service.reset(self.claim, &mut self.reset_ticket, phase)?
            {
                Completion::Complete(0) if self.reset_ticket == 0 =>
                {
                    self.reset_complete = true;
                    self.running = false;
                    return Ok(());
                }
                Completion::Pending if self.reset_ticket != 0 =>
                {
                    if self.now()? >= deadline
                    {
                        return Err(Error(SERVICE_BUSY));
                    }
                }
                _ => return Err(Error(BAD_ABI))
            }
        }
    }

    fn discover(&mut self) -> Result<()>
    {
        let slots = self.service.count()?;
        for ordinal in 0..slots
        {
            let description = match self.service.probe(ordinal)
            {
                Ok(value) => value,
                Err(Error(NOT_FOUND)) => continue,
                Err(error) => return Err(error)
            };
            if description.bus.0 != *b"VMMI" || description.key[..4] != 16u32.to_le_bytes()
            {
                continue;
            }
            let required = interface::COHERENT_DMA | interface::MMIO;
            if description.register_bytes != 4096
               || description.registers == 0
               || description.registers % 4 != 0
               || description.flags != required
            {
                return Err(Error(NOT_SUPPORTED));
            }
            self.registers = usize::try_from(description.registers).map_err(|_| Error(BAD_ABI))?;
            let claim = self.service.claim(ordinal)?;
            // Record the acquired right before any further fallible validation.
            self.claim = claim.handle;
            if claim.bus != description.bus
               || claim.key != description.key
               || claim.registers != description.registers
               || claim.register_bytes != description.register_bytes
               || claim.flags != description.flags
            {
                return Err(Error(BAD_ABI));
            }
            return Ok(());
        }
        Err(Error(NOT_FOUND))
    }

    fn configure(&mut self) -> Result<()>
    {
        self.reset()?;
        if self.read(0) != 0x7472_6976 || self.read(4) != 2 || self.read(8) != 16
        {
            return Err(Error(NOT_SUPPORTED));
        }
        // Writes below can give the device ownership again; every exit needs a
        // fresh acknowledged reset before unmapping.
        self.reset_complete = false;
        self.write(0x70, 1);
        self.write(0x70, 3);
        self.write(0x14, 1);
        if self.read(0x10) & 1 == 0
        {
            return Err(Error(NOT_SUPPORTED));
        }
        // Negotiate only VIRTIO_F_VERSION_1; no 3D, EDID or blob features.
        self.write(0x24, 0);
        self.write(0x20, 0);
        self.write(0x24, 1);
        self.write(0x20, 1);
        self.write(0x70, 11);
        if self.read(0x70) != 11
        {
            return Err(Error(NOT_SUPPORTED));
        }
        // SAFETY: acknowledged reset ended device ownership. The unique Device
        // owns the complete pinned control page, including padding.
        unsafe { core::ptr::write_bytes(CONTROL.0.get().cast::<u8>(), 0, size_of::<Control>()) };
        let control =
            self.service
                .map(self.claim, CONTROL.0.get() as usize as u64, size_of::<Control>() as u64)?;
        // Retain the exact handle before validating any returned property.
        self.control_map = control.handle;
        if control.length != size_of::<Control>() as u64 || control.address % 4096 != 0
        {
            return Err(Error(BAD_ABI));
        }
        self.control_dma = control.address;
        self.write(0x30, 0);
        if self.read(0x34) < QUEUE as u32 || self.read(0x44) != 0
        {
            return Err(Error(NOT_SUPPORTED));
        }
        self.write(0x38, QUEUE as u32);
        // SAFETY: device not yet running; volatile store into the owned page.
        // No used-buffer notifications; completions are polled.
        unsafe { write_volatile(addr_of_mut!((*CONTROL.0.get()).available.flags), 1u16.to_le()) };
        self.address(0x80, self.control_dma + offset_of!(Control, descriptors) as u64);
        self.address(0x90, self.control_dma + offset_of!(Control, available) as u64);
        self.address(0xa0, self.control_dma + offset_of!(Control, used) as u64);
        mac_driver::arch::barrier();
        self.write(0x44, 1);
        self.write(0x70, 15);
        self.running = true;
        self.offered = 0;
        self.consumed = 0;
        if self.read(0x70) != 15
        {
            return Err(Error(IO_ERROR));
        }
        Ok(())
    }

    /// Submit one command and wait for its response type. A timed-out command
    /// leaves the device owning the buffers, so the device is wedged until the
    /// acknowledged reset in quiesce.
    fn submit(&mut self, command: &Command, response_bytes: usize) -> Result<u32>
    {
        if !self.running || self.wedged || self.read(0x70) != 15
        {
            return Err(Error(IO_ERROR));
        }
        debug_assert!(response_bytes <= RESPONSE_BYTES && command.length <= COMMAND_BYTES);
        let deadline =
            self.now()?.checked_add(COMMAND_TIMEOUT_MICROSECONDS).ok_or(Error(BAD_ABI))?;
        let control = CONTROL.0.get();
        let command_dma = self.control_dma + offset_of!(Control, command) as u64;
        let response_dma = self.control_dma + offset_of!(Control, response) as u64;
        // SAFETY: no command is outstanding (each submit waits or wedges), so
        // the device does not own the command, response, descriptors or ring
        // slot; the builder buffer is disjoint from device memory.
        unsafe {
            copy_nonoverlapping(command.bytes.as_ptr(),
                                addr_of_mut!((*control).command).cast::<u8>(),
                                command.length);
            write_volatile(addr_of_mut!((*control).response).cast::<u8>(), 0);
            write_volatile(addr_of_mut!((*control).descriptors[0]),
                           Descriptor { address: command_dma.to_le(),
                                        length:  (command.length as u32).to_le(),
                                        flags:   1u16.to_le(),
                                        next:    1u16.to_le() });
            write_volatile(addr_of_mut!((*control).descriptors[1]),
                           Descriptor { address: response_dma.to_le(),
                                        length:  (response_bytes as u32).to_le(),
                                        flags:   2u16.to_le(),
                                        next:    0 });
            write_volatile(addr_of_mut!((*control).available.ring
                                            [usize::from(self.offered) % QUEUE]),
                           0);
        }
        mac_driver::arch::barrier();
        self.offered = self.offered.wrapping_add(1);
        // SAFETY: publishing the ring index hands the chain to the device.
        unsafe { write_volatile(addr_of_mut!((*control).available.index), self.offered.to_le()) };
        mac_driver::arch::barrier();
        self.write(0x50, 0);
        loop
        {
            // SAFETY: volatile read of the device-shared used index.
            let used = unsafe { u16::from_le(read_volatile(addr_of_mut!((*control).used.index))) };
            if used != self.consumed
            {
                mac_driver::arch::barrier();
                if used.wrapping_sub(self.consumed) != 1
                {
                    self.wedged = true;
                    return Err(Error(IO_ERROR));
                }
                self.consumed = used;
                break;
            }
            match self.now()
            {
                Ok(now) if now < deadline => (),
                observed =>
                {
                    self.wedged = true;
                    return Err(observed.err().unwrap_or(Error(IO_ERROR)));
                }
            }
        }
        // SAFETY: the used entry returned the chain, so the device no longer
        // writes the response; the first word is the response type.
        let kind =
            unsafe { u32::from_le(read_volatile(addr_of_mut!((*control).response).cast::<u32>())) };
        Ok(kind)
    }

    fn expect_ok(&mut self, command: &Command) -> Result<bool>
    {
        Ok(self.submit(command, 24)? == OK_NODATA)
    }

    fn display_info(&mut self) -> Result<Size>
    {
        let command = Command::new(GET_DISPLAY_INFO);
        if self.submit(&command, 24 + 16 * 24)? != OK_DISPLAY_INFO
        {
            return Err(Error(IO_ERROR));
        }
        // SAFETY: address computation only; the response is device-complete.
        let response = unsafe { addr_of_mut!((*CONTROL.0.get()).response).cast::<u8>() };
        let word = |at: usize| {
            // SAFETY: completed response; offsets are within RESPONSE_BYTES.
            unsafe { u32::from_le(read_volatile(response.add(at).cast::<u32>())) }
        };
        // Scanout 0: rect at 24 (x, y, width, height), enabled at 40.
        Ok(if word(40) != 0
           {
               Size { width:  word(32),
                      height: word(36) }
           }
           else
           {
               Size { width:  0,
                      height: 0 }
           })
    }

    /// Create `resource` for `size`, back it with the framebuffer and fill it.
    /// Refusal retains the candidate until its actual unref/reset completion;
    /// no scanout change is submitted here.
    fn create_candidate(&mut self) -> Result<bool>
    {
        let frame = self.candidate.ok_or(Error(BAD_ABI))?;
        let mut create = Command::new(RESOURCE_CREATE_2D);
        create.word(frame.resource);
        create.word(FORMAT_XRGB);
        create.word(frame.size.width);
        create.word(frame.size.height);
        if !self.expect_ok(&create)?
        {
            return Ok(false);
        }
        self.candidate.as_mut().ok_or(Error(BAD_ABI))?.created = true;
        let mut backing = Command::new(RESOURCE_ATTACH_BACKING);
        backing.word(frame.resource);
        backing.word(1);
        backing.quad(frame.memory.device_address);
        let bytes = u64::from(frame.size.width).checked_mul(u64::from(frame.size.height))
                                               .and_then(|n| n.checked_mul(4))
                                               .ok_or(Error(BAD_ABI))?;
        backing.word(u32::try_from(bytes).map_err(|_| Error(BAD_ABI))?);
        backing.word(0);
        let mut transfer = Command::new(TRANSFER_TO_HOST_2D);
        transfer.rect(0, 0, frame.size.width, frame.size.height);
        transfer.quad(0);
        transfer.word(frame.resource);
        transfer.word(0);
        Ok(self.expect_ok(&backing)? && self.expect_ok(&transfer)?)
    }

    fn allocate_frame(&mut self, size: Size) -> Result<interface::Allocation>
    {
        let bytes = u64::from(size.width).checked_mul(u64::from(size.height))
                                         .and_then(|n| n.checked_mul(4))
                                         .filter(|n| *n != 0)
                                         .ok_or(Error(BAD_ABI))?;
        if let Some(active) = self.active
           && bytes <= active.memory.length
        {
            return Ok(active.memory);
        }
        if self.allocation_ticket != 0
        {
            return Err(Error(SERVICE_BUSY));
        }
        let deadline =
            self.now()?.checked_add(COMMAND_TIMEOUT_MICROSECONDS).ok_or(Error(BAD_ABI))?;
        self.allocation_bytes = bytes;
        loop
        {
            let phase = if self.allocation_ticket == 0 { Phase::Start } else { Phase::Poll };
            match self.service.allocate(self.claim, bytes, &mut self.allocation_ticket, phase)
            {
                Ok((Completion::Complete(_), Some(buffer))) if self.allocation_ticket == 0 =>
                {
                    self.allocation_bytes = 0;
                    return Ok(buffer);
                }
                Ok((Completion::Pending, None)) if self.allocation_ticket != 0 => (),
                Err(Error(SERVICE_BUSY)) if self.allocation_ticket != 0 => (),
                Err(error) =>
                {
                    if self.allocation_ticket != 0
                    {
                        let _ = self.cancel_allocation();
                    }
                    return Err(error);
                }
                _ => return Err(Error(BAD_ABI))
            }
            match self.now()
            {
                Ok(now) if now < deadline => (),
                result =>
                {
                    let _ = self.cancel_allocation();
                    return Err(result.err().unwrap_or(Error(IO_ERROR)));
                }
            }
        }
    }

    fn cancel_allocation(&mut self) -> Result<()>
    {
        if self.allocation_ticket != 0
        {
            let (completion, buffer) = self.service.allocate(self.claim,
                                                              self.allocation_bytes,
                                                              &mut self.allocation_ticket,
                                                              Phase::Cancel)?;
            match (completion, buffer)
            {
                (Completion::Complete(0), None) if self.allocation_ticket == 0 => (),
                (Completion::Complete(interface::ALLOCATION_BYTES), Some(buffer))
                    if self.allocation_ticket == 0 =>
                {
                    // Completion can win cancellation. Retain the ACTUAL reply
                    // before any fallible free; it was never exposed to GPU/CPU.
                    self.cancelled_allocation = Some(buffer);
                }
                _ => return Err(Error(SERVICE_BUSY))
            }
            self.allocation_bytes = 0;
        }
        if let Some(mut buffer) = self.cancelled_allocation
        {
            // SAFETY: this late completed allocation was never attached to a
            // resource or lent to System. Refusal retains the original handle.
            unsafe {
                self.service.free_completed(&mut buffer)?;
            }
            self.cancelled_allocation = None;
        }
        Ok(())
    }

    fn release_frame(&mut self, mut frame: Frame) -> core::result::Result<(), (Error, Frame)>
    {
        if !frame.released && frame.created && !self.reset_complete
        {
            if let Err(error) = self.unref(frame.resource)
            {
                return Err((error, frame));
            }
            frame.released = true;
        }
        let shared =
            self.active.is_some_and(|active| active.memory.handle == frame.memory.handle)
            || self.candidate
                   .is_some_and(|candidate| candidate.memory.handle == frame.memory.handle);
        if !shared
        {
            // SAFETY: either no backing command was submitted (created=false),
            // RESOURCE_UNREF completed on this single ordered control queue, or
            // the actual Device service acknowledged reset. CPU pins are still
            // checked independently by FREE_COMPLETED.
            if let Err(error) = unsafe { self.service.free_completed(&mut frame.memory) }
            {
                return Err((error, frame));
            }
        }
        Ok(())
    }

    fn collect_retired(&mut self) -> Result<()>
    {
        if self.retired.is_some_and(|frame| !frame.release_requested) && !self.reset_complete
        {
            return Err(Error(SERVICE_BUSY));
        }
        if let Some(frame) = self.retired.take()
        {
            if let Err((error, frame)) = self.release_frame(frame)
            {
                self.retired = Some(frame);
                return Err(error);
            }
        }
        Ok(())
    }

    fn unref(&mut self, resource: u32) -> Result<()>
    {
        let mut command = Command::new(RESOURCE_UNREF);
        command.word(resource);
        command.word(0);
        if self.expect_ok(&command)? { Ok(()) } else { Err(Error(IO_ERROR)) }
    }

    fn scan_out(&mut self, resource: u32, size: Size) -> Result<bool>
    {
        let mut command = Command::new(SET_SCANOUT);
        command.rect(0, 0, size.width, size.height);
        command.word(0);
        command.word(resource);
        self.expect_ok(&command)
    }

    fn flush(&mut self, resource: u32, x: u32, y: u32, width: u32, height: u32) -> Result<bool>
    {
        let mut command = Command::new(RESOURCE_FLUSH);
        command.rect(x, y, width, height);
        command.word(resource);
        command.word(0);
        self.expect_ok(&command)
    }

    pub(super) fn quiesce(&mut self) -> Result<()>
    {
        self.reset()?;
        if self.control_map != 0
        {
            self.service.unmap(self.control_map)?;
            self.control_map = 0;
        }
        self.cancel_allocation()?;
        if let Some(frame) = self.candidate.take()
        {
            if let Err((error, frame)) = self.release_frame(frame)
            {
                self.candidate = Some(frame);
                return Err(error);
            }
        }
        self.collect_retired()?;
        if let Some(frame) = self.active.take()
        {
            if let Err((error, frame)) = self.release_frame(frame)
            {
                self.active = Some(frame);
                return Err(error);
            }
        }
        if self.claim != 0
        {
            self.service.release(self.claim)?;
            self.claim = 0;
        }
        Ok(())
    }
}

impl Hardware for Device
{
    fn attach(&mut self, attach: &Attach, generation: u64) -> Result<Surface>
    {
        self.invocation_generation = generation;
        if self.claim != 0
        {
            self.quiesce()?;
        }
        let size = Size { width:  attach.width,
                          height: attach.height };
        self.discover()?;
        self.configure()?;
        let preferred = self.display_info()?;
        let memory = self.allocate_frame(size)?;
        self.candidate = Some(Frame { generation,
                                      memory,
                                      size,
                                      resource: 1,
                                      created: false,
                                      released: false,
                                      release_requested: false,
                                      offered_to_system: false });
        if !self.create_candidate()? || !self.scan_out(1, size)?
        {
            return Err(Error(IO_ERROR));
        }
        let frame = self.candidate.take().ok_or(Error(BAD_ABI))?;
        self.active = Some(frame);
        self.current = size;
        self.resource = 1;
        if !self.flush(1, 0, 0, size.width, size.height)?
        {
            return Err(Error(IO_ERROR));
        }
        Ok(frame.surface(preferred))
    }

    fn host_size(&mut self) -> Result<Option<Size>>
    {
        // events_read lives at config offset 0, events_clear at 4.
        if self.read(0x100) & 1 == 0
        {
            return Ok(None);
        }
        self.write(0x104, 1);
        Ok(Some(self.display_info()?))
    }

    fn present(&mut self, damage: Damage) -> Result<()>
    {
        let right = damage.x.saturating_add(damage.width).min(self.current.width);
        let bottom = damage.y.saturating_add(damage.height).min(self.current.height);
        if damage.x >= right || damage.y >= bottom
        {
            return Ok(());
        }
        let (width, height) = (right - damage.x, bottom - damage.y);
        let mut transfer = Command::new(TRANSFER_TO_HOST_2D);
        transfer.rect(damage.x, damage.y, width, height);
        transfer.quad((u64::from(damage.y) * u64::from(self.current.width) + u64::from(damage.x))
                      * 4);
        transfer.word(self.resource);
        transfer.word(0);
        if self.expect_ok(&transfer)?
           && self.flush(self.resource, damage.x, damage.y, width, height)?
        {
            Ok(())
        }
        else
        {
            Err(Error(IO_ERROR))
        }
    }

    fn prepare(&mut self, size: Size, generation: u64) -> Result<Prepared>
    {
        self.invocation_generation = generation;
        self.cancel_allocation()?;
        if self.candidate.is_some_and(|frame| !frame.offered_to_system)
        {
            let frame = self.candidate.take().ok_or(Error(BAD_ABI))?;
            if let Err((error, frame)) = self.release_frame(frame)
            {
                self.candidate = Some(frame);
                return Err(error);
            }
        }
        if self.candidate.is_some()
        {
            return Err(Error(SERVICE_BUSY));
        }
        self.collect_retired()?;
        let memory = self.allocate_frame(size)?;
        let resource = if self.resource == 1 { 2 } else { 1 };
        self.candidate = Some(Frame { generation,
                                      memory,
                                      size,
                                      resource,
                                      created: false,
                                      released: false,
                                      release_requested: false,
                                      offered_to_system: false });
        if !self.create_candidate()?
        {
            let candidate = self.candidate.take().ok_or(Error(BAD_ABI))?;
            if let Err((error, candidate)) = self.release_frame(candidate)
            {
                self.candidate = Some(candidate);
                return Err(error);
            }
            return Ok(Prepared { outcome: Outcome::DeviceRefused,
                                 surface: None });
        }
        self.candidate.as_mut().ok_or(Error(BAD_ABI))?.offered_to_system = true;
        Ok(Prepared { outcome: Outcome::Applied,
                      surface: Some(self.candidate.ok_or(Error(BAD_ABI))?.surface(size)) })
    }

    fn commit(&mut self, allocation: u64, generation: u64) -> Result<Outcome>
    {
        let frame =
            self.candidate
                .filter(|frame| frame.memory.handle == allocation && frame.generation == generation)
                .ok_or(Error(BAD_ABI))?;
        if self.retired.is_some()
        {
            return Err(Error(SERVICE_BUSY));
        }
        if !self.scan_out(frame.resource, frame.size)?
        {
            return Ok(Outcome::DeviceRefused);
        }
        self.retired = self.active.take();
        self.active = self.candidate.take();
        self.current = frame.size;
        self.resource = frame.resource;
        // Scanout ACK is the commit point. Presentation may be retried by the
        // ordinary damage path; it cannot retroactively refuse the mode.
        let _ = self.flush(frame.resource, 0, 0, frame.size.width, frame.size.height);
        Ok(Outcome::Applied)
    }

    fn abort(&mut self, allocation: u64, generation: u64) -> Result<()>
    {
        if self.last_aborted == Some((allocation, generation))
        {
            return Ok(());
        }
        let frame =
            self.candidate
                .filter(|frame| frame.memory.handle == allocation && frame.generation == generation)
                .ok_or(Error(BAD_ABI))?;
        self.candidate = None;
        if let Err((error, frame)) = self.release_frame(frame)
        {
            self.candidate = Some(frame);
            return Err(error);
        }
        self.last_aborted = Some((allocation, generation));
        Ok(())
    }

    fn release(&mut self, allocation: u64, generation: u64) -> Result<()>
    {
        if self.last_released == Some((allocation, generation))
        {
            return Ok(());
        }
        if self.retired.is_some_and(|frame| {
                           frame.memory.handle == allocation && frame.generation == generation
                       })
        {
            self.retired.as_mut().ok_or(Error(BAD_ABI))?.release_requested = true;
            self.collect_retired()?;
            self.last_released = Some((allocation, generation));
            Ok(())
        }
        else
        {
            Err(Error(BAD_ABI))
        }
    }

    fn pending(&self) -> bool
    {
        self.allocation_ticket != 0 || self.reset_ticket != 0 || self.cancelled_allocation.is_some()
    }

    fn cancel(&mut self, generation: u64) -> Result<()>
    {
        if generation != self.invocation_generation
        {
            return Ok(());
        }
        self.cancel_allocation()?;
        if self.reset_ticket != 0
        {
            match self.service.reset(self.claim, &mut self.reset_ticket, Phase::Cancel)?
            {
                Completion::Complete(0) if self.reset_ticket == 0 =>
                {
                    self.reset_complete = true;
                    self.running = false;
                }
                _ => return Err(Error(SERVICE_BUSY))
            }
        }
        if let Some(frame) = self.candidate
           && frame.generation == generation
        {
            self.abort(frame.memory.handle, generation)?;
        }
        Ok(())
    }

    fn microseconds(&mut self) -> Result<u64>
    {
        self.now()
    }
}

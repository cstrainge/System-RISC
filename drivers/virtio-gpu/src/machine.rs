//! Audited modern coherent VirtIO MMIO GPU boundary (VirtIO 1.3 section 5.7,
//! 2D commands only). One control queue and its command/response buffers fit
//! in one pinned page; the scan-out framebuffer is a second pinned static.
//! System cannot lend this driver its own memory (module DMA covers only the
//! module image), so the driver owns the framebuffer and reports its address.
//! Device-shared storage is accessed only through raw pointers; no Rust
//! reference aliases DMA.
#![allow(unsafe_code)]

use core::{cell::UnsafeCell,
           mem::{offset_of, size_of},
           ptr::{addr_of_mut, copy_nonoverlapping, read_volatile, write_volatile}};

use mac_abi::{BAD_ABI, IO_ERROR, NOT_FOUND, NOT_SUPPORTED, SERVICE_BUSY};
use mac_driver::{Client, Completion, Error, Phase, Result,
                 devices::{DeviceService, interface}};
use mac_video_interface::{Attach, Damage, Outcome, Size, Surface};

use crate::Hardware;

const QUEUE: usize = 8;
/// Largest scan-out the driver reserves memory for (1920x1200 XRGB32).
const FRAME_BYTES: usize = 1920 * 1200 * 4;
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
#[repr(C, align(4096))]
struct Frame([u8; FRAME_BYTES]);
struct Shared<T>(UnsafeCell<T>);
// SAFETY: the module's unique Device is protected by DEVICE in lib.rs. Quiesce
// removes that Device only after reset/unmap/release. No other path touches
// these statics; the framebuffer is additionally written by System through the
// address reported at ATTACH, which is the contract's purpose.
unsafe impl<T> Sync for Shared<T>
{
}
static CONTROL: Shared<Control> =
    Shared(UnsafeCell::new(Control { descriptors: [Descriptor { address: 0,
                                                                length:  0,
                                                                flags:   0,
                                                                next:    0 }; QUEUE],
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
static FRAME: Shared<Frame> = Shared(UnsafeCell::new(Frame([0; FRAME_BYTES])));

fn fence()
{
    // SAFETY: a full fence has no memory operands. It is the required
    // device/CPU publication ordering, not diagnostic instrumentation.
    unsafe { core::arch::asm!("fence iorw, iorw", options(nostack)) };
}

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

pub(super) struct Device
{
    host:           Client,
    service:        DeviceService,
    claim:          u64,
    control_map:    u64,
    frame_map:      u64,
    reset_ticket:   u64,
    reset_complete: bool,
    registers:      usize,
    control_dma:    u64,
    frame_dma:      u64,
    running:        bool,
    wedged:         bool,
    offered:        u16,
    consumed:       u16,
    current:        Size,
    resource:       u32
}
impl Device
{
    pub(super) fn new(host: Client) -> Result<Self>
    {
        Ok(Self { host,
                  service: DeviceService::new(host)?,
                  claim: 0,
                  control_map: 0,
                  frame_map: 0,
                  reset_ticket: 0,
                  reset_complete: false,
                  registers: 0,
                  control_dma: 0,
                  frame_dma: 0,
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
        fence();
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
        let deadline = self.now()?.saturating_add(RESET_TIMEOUT_MICROSECONDS);
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
            self.service.map(self.claim, CONTROL.0.get() as usize as u64, size_of::<Control>() as u64)?;
        // Retain the exact handle before validating any returned property.
        self.control_map = control.handle;
        if control.length != size_of::<Control>() as u64 || control.address % 4096 != 0
        {
            return Err(Error(BAD_ABI));
        }
        self.control_dma = control.address;
        let frame = self.service.map(self.claim, FRAME.0.get() as usize as u64, FRAME_BYTES as u64)?;
        self.frame_map = frame.handle;
        if frame.length != FRAME_BYTES as u64
           || frame.address % 4096 != 0
           || frame.address.checked_add(frame.length).is_none()
        {
            return Err(Error(BAD_ABI));
        }
        self.frame_dma = frame.address;
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
        fence();
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
            write_volatile(addr_of_mut!((*control).available.ring[usize::from(self.offered) % QUEUE]),
                           0);
        }
        fence();
        self.offered = self.offered.wrapping_add(1);
        // SAFETY: publishing the ring index hands the chain to the device.
        unsafe {
            write_volatile(addr_of_mut!((*control).available.index), self.offered.to_le())
        };
        fence();
        self.write(0x50, 0);
        let deadline = self.now()?.saturating_add(COMMAND_TIMEOUT_MICROSECONDS);
        loop
        {
            // SAFETY: volatile read of the device-shared used index.
            let used =
                unsafe { u16::from_le(read_volatile(addr_of_mut!((*control).used.index))) };
            if used != self.consumed
            {
                fence();
                if used.wrapping_sub(self.consumed) != 1
                {
                    self.wedged = true;
                    return Err(Error(IO_ERROR));
                }
                self.consumed = used;
                break;
            }
            if self.now()? >= deadline
            {
                self.wedged = true;
                return Err(Error(IO_ERROR));
            }
        }
        // SAFETY: the used entry returned the chain, so the device no longer
        // writes the response; the first word is the response type.
        let kind = unsafe {
            u32::from_le(read_volatile(addr_of_mut!((*control).response).cast::<u32>()))
        };
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
    /// On any failure the resource is released and the old mode is untouched.
    fn create(&mut self, resource: u32, size: Size) -> Result<bool>
    {
        let mut create = Command::new(RESOURCE_CREATE_2D);
        create.word(resource);
        create.word(FORMAT_XRGB);
        create.word(size.width);
        create.word(size.height);
        if !self.expect_ok(&create)?
        {
            return Ok(false);
        }
        let mut backing = Command::new(RESOURCE_ATTACH_BACKING);
        backing.word(resource);
        backing.word(1);
        backing.quad(self.frame_dma);
        backing.word(size.width * size.height * 4);
        backing.word(0);
        let mut transfer = Command::new(TRANSFER_TO_HOST_2D);
        transfer.rect(0, 0, size.width, size.height);
        transfer.quad(0);
        transfer.word(resource);
        transfer.word(0);
        if !self.expect_ok(&backing)? || !self.expect_ok(&transfer)?
        {
            self.unref(resource)?;
            return Ok(false);
        }
        Ok(true)
    }

    fn unref(&mut self, resource: u32) -> Result<()>
    {
        let mut command = Command::new(RESOURCE_UNREF);
        command.word(resource);
        command.word(0);
        self.expect_ok(&command).map(|_| ())
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
        if self.frame_map != 0
        {
            self.service.unmap(self.frame_map)?;
            self.frame_map = 0;
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
    fn attach(&mut self, attach: &Attach) -> Result<Surface>
    {
        if self.claim != 0
        {
            return Err(Error(SERVICE_BUSY));
        }
        let size = Size { width:  attach.width,
                          height: attach.height };
        self.discover()?;
        self.configure()?;
        let preferred = self.display_info()?;
        self.resource = 1;
        if !self.create(1, size)? || !self.scan_out(1, size)?
        {
            return Err(Error(IO_ERROR));
        }
        self.current = size;
        self.flush(1, 0, 0, size.width, size.height)?;
        Ok(Surface { address: FRAME.0.get() as usize as u64,
                     capacity: FRAME_BYTES as u64,
                     preferred })
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
        transfer.quad((u64::from(damage.y) * u64::from(self.current.width) + u64::from(damage.x)) * 4);
        transfer.word(self.resource);
        transfer.word(0);
        if self.expect_ok(&transfer)? && self.flush(self.resource, damage.x, damage.y, width, height)?
        {
            Ok(())
        }
        else
        {
            Err(Error(IO_ERROR))
        }
    }

    fn apply(&mut self, size: Size) -> Result<Outcome>
    {
        let old = self.resource;
        let new = if old == 1 { 2 } else { 1 };
        if !self.create(new, size)?
        {
            return Ok(Outcome::DeviceRefused);
        }
        if !self.scan_out(new, size)?
        {
            self.unref(new)?;
            return Ok(Outcome::DeviceRefused);
        }
        self.resource = new;
        self.current = size;
        // The host now shows the new resource. A failed flush or unref leaves
        // it consistent: the next presented damage retries the flush, and a
        // leaked old resource is released by the reset on quiesce.
        let _ = self.flush(new, 0, 0, size.width, size.height);
        let _ = self.unref(old);
        Ok(Outcome::Applied)
    }

    fn microseconds(&mut self) -> Result<u64>
    {
        self.now()
    }
}

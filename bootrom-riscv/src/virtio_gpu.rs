//! Boot-hart-only modern virtio-mmio 2D scanout. CPU addresses are physical
//! only in this M-mode binding. The control page and framebuffer are admitted
//! permanent boot reservations, never heap allocations or application memory.
use core::{arch::asm,
           cell::UnsafeCell,
           fmt,
           mem::{offset_of, size_of},
           ptr::{addr_of_mut, copy_nonoverlapping, read_volatile, write_volatile}};

const QUEUE: usize = 8;
const RESOURCE: u32 = 1;
const RESPONSE_BYTES: usize = 408;
const COMMAND_BYTES: usize = 64;
const FENCE: u32 = 1;
const OK: u32 = 0x1100;
const DISPLAY_INFO: u32 = 0x1101;

#[derive(Clone, Copy, Debug)]
pub enum Cause
{
    Clock,
    ClockRegression,
    Overflow,
    Version(u32),
    Features,
    Queue,
    Status(u32),
    Timeout,
    Used
    {
        delta: u16,
        id:    u32,
        bytes: u32
    },
    Response
    {
        expected: u32,
        observed: u32
    },
    Fence,
    NoScanout,
    Reservation
}
#[derive(Clone, Copy, Debug)]
pub struct Error
{
    pub stage: &'static str,
    pub cause: Cause
}
impl Error
{
    fn new(stage: &'static str, cause: Cause) -> Self
    {
        Self { stage,
               cause }
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        write!(out, "VirtIO GPU {}: ", self.stage)?;
        match self.cause
        {
            Cause::Clock => out.write_str("device-tree timebase is zero"),
            Cause::ClockRegression => out.write_str("architectural clock moved backwards"),
            Cause::Overflow => out.write_str("checked address, size, fence or deadline overflow"),
            Cause::Version(v) =>
            {
                write!(out, "MMIO version {v} is unsupported; modern version 2 required")
            }
            Cause::Features => out.write_str("device refused VERSION_1 features"),
            Cause::Queue =>
            {
                out.write_str("control queue is unavailable, already active or too small")
            }
            Cause::Status(s) => write!(out, "unexpected device status {s:#x}"),
            Cause::Timeout =>
            {
                out.write_str("device did not acknowledge before the native clock deadline")
            }
            Cause::Used { delta,
                          id,
                          bytes } =>
            {
                write!(out, "invalid completed chain: delta={delta} id={id} bytes={bytes}")
            }
            Cause::Response { expected,
                              observed } =>
            {
                let detail = match observed
                {
                    0x1200 => "unspecified device error",
                    0x1201 => "host resource memory exhausted",
                    0x1202 => "invalid scanout",
                    0x1203 => "invalid resource",
                    0x1204 => "invalid context",
                    0x1205 => "invalid parameter",
                    _ => "unexpected response"
                };
                write!(out, "{detail} ({observed:#x}); expected {expected:#x}")
            }
            Cause::Fence => out.write_str("completed response did not match the submitted fence"),
            Cause::NoScanout => out.write_str("scanout 0 is not enabled"),
            Cause::Reservation =>
            {
                out.write_str("DMA storage is outside the actual pinned boot reservations")
            }
        }
    }
}
/// The first error survives cleanup. A reset error means device ownership is
/// uncertain: no caller may reuse the buffers, fall back or enter System.
#[derive(Clone, Copy, Debug)]
pub struct Failure
{
    pub cause: Error,
    pub reset: Option<Error>
}
impl fmt::Display for Failure
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        self.cause.fmt(out)?;
        if let Some(error) = self.reset
        {
            write!(out, "; reset unacknowledged: {error}")?;
        }
        Ok(())
    }
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
struct Shared(UnsafeCell<Control>);
// SAFETY: only the primary boot hart accesses this page, after actual reset ACK
// and before the one-way System handoff. Other harts are parked. After handoff
// the entire page remains pinned and ROM never accesses the device again.
unsafe impl Sync for Shared
{
}
static CONTROL: Shared =
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
fn fence()
{
    unsafe { asm!("fence iorw, iorw", options(nostack)) };
}
fn ticks() -> u64
{
    let value;
    unsafe { asm!("rdtime {}", out(reg)value, options(nomem, nostack)) };
    value
}
fn read(base: usize, offset: usize) -> u32
{
    unsafe { u32::from_le(read_volatile((base + offset) as *const u32)) }
}
pub fn is_gpu(base: usize) -> bool
{
    read(base, 0) == 0x7472_6976 && read(base, 8) == 16
}
struct Command
{
    data:   [u8; COMMAND_BYTES],
    length: usize
}
impl Command
{
    fn new(kind: u32) -> Self
    {
        let mut c = Self { data:   [0; COMMAND_BYTES],
                           length: 24 };
        c.data[..4].copy_from_slice(&kind.to_le_bytes());
        c
    }

    fn word(&mut self, value: u32)
    {
        // All private command shapes below have a compile-time bounded length.
        self.data[self.length..self.length + 4].copy_from_slice(&value.to_le_bytes());
        self.length += 4;
    }

    fn quad(&mut self, value: u64)
    {
        self.word(value as u32);
        self.word((value >> 32) as u32);
    }

    fn rect(&mut self, x: u32, y: u32, w: u32, h: u32)
    {
        for n in [x, y, w, h]
        {
            self.word(n);
        }
    }
}
pub struct Device
{
    base:        usize,
    frame:       u64,
    width:       u32,
    height:      u32,
    hz:          u64,
    last:        u64,
    fence_id:    u64,
    index:       u16,
    needs_reset: bool
}
impl Device
{
    /// The caller already admitted this platform's real RAM/reservation table.
    /// Every byte offered to DMA must still be contained in a reserved region.
    pub fn open(base: usize, frame: u64, width: u32, height: u32, hz: u64,
                memory: mac_abi::boot::CheckedMemory<'_>)
                -> Result<Self, Failure>
    {
        let mut device = Self { base,
                                frame,
                                width,
                                height,
                                hz,
                                last: ticks(),
                                fence_id: 0,
                                index: 0,
                                needs_reset: false };
        let reserved = |base, length| {
            memory.reserved(mac_abi::BootMemoryRange { base,
                                                       length })
        };
        let bytes = u64::from(width).checked_mul(u64::from(height)).and_then(|n| n.checked_mul(4));
        if width == 0
           || height == 0
           || !reserved(CONTROL.0.get() as u64, size_of::<Control>() as u64)
           || !bytes.is_some_and(|n| n <= u64::from(u32::MAX) && reserved(frame, n))
        {
            return Err(Failure { cause: Error::new("admission", Cause::Reservation),
                                 reset: None });
        }
        let result = device.initialize();
        match result
        {
            Ok(()) => Ok(device),
            Err(cause) => Err(device.fail(cause))
        }
    }

    fn write(&self, offset: usize, value: u32)
    {
        unsafe { write_volatile((self.base + offset) as *mut u32, value.to_le()) };
        fence();
    }

    fn address(&self, offset: usize, value: u64)
    {
        self.write(offset, value as u32);
        self.write(offset + 4, (value >> 32) as u32);
    }

    fn deadline(&mut self, seconds: u64, stage: &'static str) -> Result<u64, Error>
    {
        if self.hz == 0
        {
            return Err(Error::new(stage, Cause::Clock));
        }
        let span = self.hz.checked_mul(seconds).ok_or(Error::new(stage, Cause::Overflow))?;
        self.now(stage)?.checked_add(span).ok_or(Error::new(stage, Cause::Overflow))
    }

    fn now(&mut self, stage: &'static str) -> Result<u64, Error>
    {
        let now = ticks();
        if now < self.last
        {
            return Err(Error::new(stage, Cause::ClockRegression));
        }
        self.last = now;
        Ok(now)
    }

    fn reset(&mut self) -> Result<(), Error>
    {
        let deadline = self.deadline(1, "reset")?;
        self.needs_reset = true;
        self.write(0x70, 0);
        while read(self.base, 0x70) != 0
        {
            if self.now("reset")? >= deadline
            {
                return Err(Error::new("reset", Cause::Timeout));
            }
            core::hint::spin_loop();
        }
        fence();
        self.needs_reset = false;
        let _ = fmt::write(&mut crate::Serial,
                           format_args!("BOOT_VIRTIO_GPU_RESET_ACK mmio={:#x} status=0\n",
                                        self.base));
        Ok(())
    }

    fn fail(&mut self, cause: Error) -> Failure
    {
        let reset = if self.needs_reset { self.reset().err() } else { None };
        Failure { cause,
                  reset }
    }

    fn initialize(&mut self) -> Result<(), Error>
    {
        // Initial reset also handles a device left running by earlier firmware.
        self.needs_reset = true;
        self.reset()?;
        let version = read(self.base, 4);
        if version != 2
        {
            return Err(Error::new("discovery", Cause::Version(version)));
        }
        self.needs_reset = true;
        self.write(0x70, 1);
        self.write(0x70, 3);
        self.write(0x14, 1);
        if read(self.base, 0x10) & 1 == 0
        {
            return Err(Error::new("negotiate", Cause::Features));
        }
        self.write(0x24, 0);
        self.write(0x20, 0);
        self.write(0x24, 1);
        self.write(0x20, 1);
        self.write(0x70, 11);
        if read(self.base, 0x70) != 11
        {
            return Err(Error::new("negotiate", Cause::Features));
        }
        self.write(0x30, 0);
        if read(self.base, 0x34) < QUEUE as u32 || read(self.base, 0x44) != 0
        {
            return Err(Error::new("queue", Cause::Queue));
        }
        // Actual reset ended all prior ownership. This is still physical M-mode
        // memory; it remains in the admitted boot reservation after MRET.
        unsafe { core::ptr::write_bytes(CONTROL.0.get().cast::<u8>(), 0, size_of::<Control>()) };
        let dma = CONTROL.0.get() as u64;
        unsafe { write_volatile(addr_of_mut!((*CONTROL.0.get()).available.flags), 1u16.to_le()) };
        self.write(0x38, QUEUE as u32);
        self.address(0x80, dma + offset_of!(Control, descriptors) as u64);
        self.address(0x90, dma + offset_of!(Control, available) as u64);
        self.address(0xa0, dma + offset_of!(Control, used) as u64);
        self.write(0x44, 1);
        self.write(0x70, 15);
        let mut info = Command::new(0x100);
        self.submit(&mut info, RESPONSE_BYTES, DISPLAY_INFO, "display-info")?;
        let response = unsafe { addr_of_mut!((*CONTROL.0.get()).response).cast::<u32>() };
        if unsafe { u32::from_le(read_volatile(response.add(10))) } == 0
        {
            return Err(Error::new("display-info", Cause::NoScanout));
        }
        let mut create = Command::new(0x101);
        for n in [RESOURCE, 2, self.width, self.height]
        {
            create.word(n);
        }
        self.submit(&mut create, 24, OK, "create-2d")?;
        let mut attach = Command::new(0x106);
        attach.word(RESOURCE);
        attach.word(1);
        attach.quad(self.frame);
        attach.word(self.width * self.height * 4);
        attach.word(0);
        self.submit(&mut attach, 24, OK, "attach-backing")?;
        let mut scanout = Command::new(0x103);
        scanout.rect(0, 0, self.width, self.height);
        scanout.word(0);
        scanout.word(RESOURCE);
        self.submit(&mut scanout, 24, OK, "set-scanout")?;
        self.present(0, 0, self.width, self.height)
    }

    fn submit(&mut self, command: &mut Command, capacity: usize, expected: u32,
              stage: &'static str)
              -> Result<(), Error>
    {
        let status = read(self.base, 0x70);
        if status != 15
        {
            return Err(Error::new(stage, Cause::Status(status)));
        }
        let deadline = self.deadline(2, stage)?;
        self.fence_id = self.fence_id.checked_add(1).ok_or(Error::new(stage, Cause::Overflow))?;
        command.data[4..8].copy_from_slice(&FENCE.to_le_bytes());
        command.data[8..16].copy_from_slice(&self.fence_id.to_le_bytes());
        let p = CONTROL.0.get();
        let dma = p as u64;
        // No outstanding chain: the previous submit either completed this exact
        // descriptor/fence or returned to the one reset/quarantine path.
        unsafe {
            copy_nonoverlapping(command.data.as_ptr(),
                                addr_of_mut!((*p).command).cast(),
                                command.length);
            core::ptr::write_bytes(addr_of_mut!((*p).response).cast::<u8>(), 0, RESPONSE_BYTES);
            write_volatile(addr_of_mut!((*p).descriptors[0]),
                           Descriptor { address:
                                            (dma + offset_of!(Control, command) as u64).to_le(),
                                        length:  (command.length as u32).to_le(),
                                        flags:   1u16.to_le(),
                                        next:    1u16.to_le() });
            write_volatile(addr_of_mut!((*p).descriptors[1]),
                           Descriptor { address:
                                            (dma + offset_of!(Control, response) as u64).to_le(),
                                        length:  (capacity as u32).to_le(),
                                        flags:   2u16.to_le(),
                                        next:    0 });
            write_volatile(addr_of_mut!((*p).available.ring[usize::from(self.index) % QUEUE]), 0);
        }
        fence();
        let next = self.index.wrapping_add(1);
        unsafe { write_volatile(addr_of_mut!((*p).available.index), next.to_le()) };
        fence();
        self.write(0x50, 0);
        let completed_bytes = loop
        {
            let used = unsafe { u16::from_le(read_volatile(addr_of_mut!((*p).used.index))) };
            if used != self.index
            {
                fence();
                let entry = unsafe {
                    read_volatile(addr_of_mut!((*p).used.ring[usize::from(self.index) % QUEUE]))
                };
                let id = u32::from_le(entry.id);
                let bytes = u32::from_le(entry.length);
                if used != next || id != 0 || bytes < 24 || bytes > capacity as u32
                {
                    return Err(Error::new(stage,
                                          Cause::Used { delta: used.wrapping_sub(self.index),
                                                        id,
                                                        bytes }));
                }
                self.index = next;
                break bytes;
            }
            if self.now(stage)? >= deadline
            {
                return Err(Error::new(stage, Cause::Timeout));
            }
            let status = read(self.base, 0x70);
            if status != 15
            {
                return Err(Error::new(stage, Cause::Status(status)));
            }
            core::hint::spin_loop();
        };
        let interrupt = read(self.base, 0x60) & 3;
        if interrupt != 0
        {
            self.write(0x64, interrupt);
        }
        let response = unsafe { addr_of_mut!((*p).response).cast::<u32>() };
        let word = |i| unsafe { u32::from_le(read_volatile(response.add(i))) };
        let kind = word(0);
        // Error replies are a complete 24-byte header, even for DISPLAY_INFO.
        if kind != expected
        {
            return Err(Error::new(stage,
                                  Cause::Response { expected,
                                                    observed: kind }));
        }
        if completed_bytes != capacity as u32
        {
            return Err(Error::new(stage,
                                  Cause::Used { delta: 1,
                                                id:    0,
                                                bytes: completed_bytes }));
        }
        if word(1) & FENCE == 0 || (u64::from(word(3)) << 32 | u64::from(word(2))) != self.fence_id
        {
            return Err(Error::new(stage, Cause::Fence));
        }
        Ok(())
    }

    pub fn update(&mut self, x: u32, y: u32, width: u32, height: u32) -> Result<(), Failure>
    {
        self.present(x, y, width, height).map_err(|cause| self.fail(cause))
    }

    fn present(&mut self, x: u32, y: u32, width: u32, height: u32) -> Result<(), Error>
    {
        if width == 0
           || height == 0
           || x.checked_add(width).is_none_or(|n| n > self.width)
           || y.checked_add(height).is_none_or(|n| n > self.height)
        {
            return Err(Error::new("damage", Cause::Overflow));
        }
        let mut transfer = Command::new(0x105);
        transfer.rect(x, y, width, height);
        transfer.quad((u64::from(y) * u64::from(self.width) + u64::from(x)) * 4);
        transfer.word(RESOURCE);
        transfer.word(0);
        self.submit(&mut transfer, 24, OK, "transfer")?;
        let mut flush = Command::new(0x104);
        flush.rect(x, y, width, height);
        flush.word(RESOURCE);
        flush.word(0);
        self.submit(&mut flush, 24, OK, "flush")
    }

    pub fn handoff(&self)
    {
        let _ = fmt::write(&mut crate::Serial,
                           format_args!("BOOT_VIRTIO_GPU_HANDOFF mmio={:#x} resource={} \
                                         queue={:#x} frame={:#x} completed={} \
                                         reserved_until_reset=true\n",
                                        self.base,
                                        RESOURCE,
                                        CONTROL.0.get() as u64,
                                        self.frame,
                                        self.index));
    }
}

//! The one audited machine adapter: the mailbox register window, the Device DMA
//! allocations, the CPU view of the firmware buffer and the cache instructions.
//! Everything above it is safe code that the host tests drive.
#![allow(unsafe_code)]
use core::{arch::asm, ptr::copy_nonoverlapping};

use mac_driver::{Client, Completion, Error, Phase, Result as DriverResult,
                 devices::{DeviceService, interface}};
use mac_video_interface::Size;
use mac_videocore_fb::{Channel, Error as MailboxError, Message, WORDS, registers::Mailbox};

use crate::{Bounds, Device, Engine, Refusal, library_mode,
            scanout::{Allocation, Frame, Plan, Platform}};

include!(concat!(env!("OUT_DIR"), "/platform-display.rs"));
include!(concat!(env!("OUT_DIR"), "/platform-virtual.rs"));

/// The `brcm,mbox` inventory key, the same value `module.json` matches on.
const MAILBOX_KEY: [u8; 16] = *b"brcm,mbox\0\0\0\0\0\0\0";
/// Deferred allocation deadline. The resident services the request from its own
/// loop; a mailbox that never answers is a defect, not a wait to extend.
const ALLOCATION_TIMEOUT_MICROSECONDS: u64 = 1_000_000;

/// Message storage. The firmware reads and writes it through the bus, so its
/// bus address is mapped from this storage rather than assumed equal to its
/// CPU address.
#[repr(align(16))]
struct Storage([u32; WORDS]);
static mut MESSAGE: Storage = Storage([0; WORDS]);

/// Clean `bytes` at `address` to the point of coherency, then wait for it. The
/// firmware reads the request from memory, so the stores must be visible before
/// the doorbell.
unsafe fn clean(address: usize, bytes: usize)
{
    unsafe {
        let ctr: u64;
        asm!("mrs {}, ctr_el0", out(reg) ctr, options(nostack, nomem));
        let line = 4usize << ((ctr >> 16) & 0xf);
        let end = address.saturating_add(bytes);
        let mut at = address & !(line - 1);
        while at < end
        {
            asm!("dc cvac, {}", in(reg) at, options(nostack));
            at += line;
        }
        asm!("dsb sy", options(nostack));
    }
}

/// Discard cached copies of `bytes` at `address` after the firmware wrote them.
unsafe fn invalidate(address: usize, bytes: usize)
{
    unsafe {
        let ctr: u64;
        asm!("mrs {}, ctr_el0", out(reg) ctr, options(nostack, nomem));
        let line = 4usize << ((ctr >> 16) & 0xf);
        let end = address.saturating_add(bytes);
        let mut at = address & !(line - 1);
        while at < end
        {
            asm!("dc ivac, {}", in(reg) at, options(nostack));
            at += line;
        }
        asm!("dsb sy", options(nostack));
    }
}

/// The mailbox with the message buffer's cache maintenance attached. The
/// library's encoder and decoder stay unchanged; only the transport knows that
/// the buffer is shared with another agent.
struct Coherent
{
    mailbox: Mailbox
}
impl Channel for Coherent
{
    fn transact(&mut self, address: u32, words: &mut [u32]) -> Result<(), MailboxError>
    {
        let pointer = words.as_mut_ptr() as usize;
        let bytes = words.len() * size_of::<u32>();
        // SAFETY: `words` is the message storage the caller owns and passes for
        // the duration of this call.
        unsafe { clean(pointer, bytes) };
        let result = self.mailbox.transact(address, words);
        // SAFETY: same storage; the firmware may have written the reply into it.
        unsafe { invalidate(pointer, bytes) };
        result
    }
}

/// The CPU view of firmware memory, through the platform's physical map.
fn physical_view(physical: u32) -> Option<usize>
{
    let physical = u64::from(physical);
    let window = VM_PHYSMAP_END.checked_sub(VM_PHYSMAP_START)?;
    if physical < VM_PHYSMAP_PHYSICAL_BASE || physical - VM_PHYSMAP_PHYSICAL_BASE >= window
    {
        return None;
    }
    usize::try_from(VM_PHYSMAP_START + (physical - VM_PHYSMAP_PHYSICAL_BASE)).ok()
}

pub struct Machine
{
    host:    Client,
    service: DeviceService,
    claim:   u64,
    message: u32,
    mailbox: Option<Coherent>
}
impl Machine
{
    pub fn new(host: Client) -> DriverResult<Self>
    {
        Ok(Self { host,
                  service: DeviceService::new(host)?,
                  claim: 0,
                  message: 0,
                  mailbox: None })
    }

    /// Find the mailbox the platform record declares, claim it and map the
    /// message buffer. The register window comes from the record; the claim
    /// covers the page that must contain it, and that containment is checked
    /// rather than assumed.
    fn discover(&mut self) -> DriverResult<()>
    {
        if self.mailbox.is_some()
        {
            return Ok(());
        }
        if !PLATFORM_DECLARES_MAILBOX
        {
            // The build script found no `bcm2835-mailbox` window in the selected
            // platform record, so there is no address this driver may touch.
            return Err(Error(mac_abi::NOT_SUPPORTED));
        }
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
               || description.key != MAILBOX_KEY
               || description.flags & interface::MMIO == 0
            {
                continue;
            }
            // The record's register window must sit inside the page the
            // inventory claims. That agreement is what makes the two record
            // entries describe one device instead of two.
            let page_end = description.registers
                                      .checked_add(description.register_bytes)
                                      .ok_or(Error(mac_abi::BAD_ABI))?;
            let window_start = MAILBOX_BASE as u64;
            let window_end =
                window_start.checked_add(MAILBOX_BYTES as u64).ok_or(Error(mac_abi::BAD_ABI))?;
            if window_start < description.registers || window_end > page_end
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
            // SAFETY: the module owns this static for its lifetime and the claim
            // is exclusive; the storage is 16-byte aligned by its attribute.
            let storage = unsafe { core::ptr::addr_of_mut!(MESSAGE.0) };
            let mapping =
                self.service.map(self.claim, storage as u64, (WORDS * size_of::<u32>()) as u64)?;
            self.message = u32::try_from(mapping.address).map_err(|_| Error(mac_abi::BAD_ABI))?;
            // SAFETY: the platform record's window, checked above to lie inside
            // the page this module exclusively claims.
            self.mailbox = Some(Coherent { mailbox: unsafe { Mailbox::new(MAILBOX_BASE) } });
            return Ok(());
        }
        Err(Error(mac_abi::NOT_FOUND))
    }

    fn allocate_deferred(&mut self, bytes: u64) -> Result<Allocation, Refusal>
    {
        let mut ticket = 0;
        let deadline = self.now()?.saturating_add(ALLOCATION_TIMEOUT_MICROSECONDS);
        loop
        {
            let phase = if ticket == 0 { Phase::Start } else { Phase::Poll };
            let (completion, buffer) = self.service
                                           .allocate(self.claim, bytes, &mut ticket, phase)
                                           .map_err(|error| Refusal::Dma(error.0))?;
            match (completion, buffer)
            {
                (Completion::Complete(_), Some(buffer)) =>
                {
                    return Ok(Allocation { handle:  buffer.handle,
                                           address: buffer.cpu_address,
                                           length:  buffer.length });
                }
                (Completion::Pending, _) if self.now()? < deadline => (),
                _ =>
                {
                    if ticket != 0
                    {
                        let _ =
                            self.service.allocate(self.claim, bytes, &mut ticket, Phase::Cancel);
                    }
                    return Err(Refusal::NoMemory);
                }
            }
        }
    }
}
impl Platform for Machine
{
    fn acquire(&mut self, geometry: Size, alignment: u32) -> Result<Frame, Refusal>
    {
        self.discover().map_err(|error| Refusal::Dma(error.0))?;
        let mailbox = self.mailbox.as_mut().ok_or(Refusal::NoMemory)?;
        // SAFETY: the boot hart and then this module are the only users of the
        // message storage, and `Message` borrows it for this call only.
        let words = unsafe { &mut *core::ptr::addr_of_mut!(MESSAGE.0) };
        let mut message = Message::new(words, self.message)?;
        let mode = library_mode(geometry);
        let framebuffer = mac_videocore_fb::acquire(mailbox, &mut message, mode, alignment)?;
        Ok(Frame { physical: framebuffer.base,
                   size: framebuffer.size,
                   pitch: framebuffer.pitch,
                   geometry })
    }

    fn allocate(&mut self, bytes: u64) -> Result<Allocation, Refusal>
    {
        self.discover().map_err(|error| Refusal::Dma(error.0))?;
        self.allocate_deferred(bytes)
    }

    fn free(&mut self, allocation: Allocation) -> Result<(), Refusal>
    {
        let mut allocation = interface::Allocation { handle:         allocation.handle,
                                                     cpu_address:    allocation.address,
                                                     device_address: 0,
                                                     length:         allocation.length };
        // SAFETY: the firmware scans out its own buffer, never this storage; the
        // Device service refuses while System holds a CPU pin, so a successful
        // free proves no CPU view remains.
        unsafe { self.service.free_completed(&mut allocation) }.map_err(|error| {
                                                                   Refusal::Dma(error.0)
                                                               })
    }

    fn blit(&mut self, allocation: Allocation, frame: &Frame, plan: &Plan) -> Result<(), Refusal>
    {
        let scanout = physical_view(frame.physical).ok_or(Refusal::NoMemory)?;
        let width_words = frame.geometry.width as usize;
        // SAFETY: both ranges are the storage this driver owns for this mode: the
        // surface is a Device allocation of at least `pitch * height` bytes, and
        // the firmware buffer is `size` bytes at a physical address the mailbox
        // reported, seen through the platform's physical map. The plan was
        // computed from the same geometry and pitch.
        unsafe {
            let source = allocation.address as *const u32;
            let destination = scanout as *mut u8;
            for row in 0..plan.rows
            {
                copy_nonoverlapping(source.add(plan.source_words + row * width_words).cast::<u8>(),
                                    destination.add(plan.destination_bytes
                                                    + row * frame.pitch as usize),
                                    plan.row_words * size_of::<u32>());
            }
        }
        Ok(())
    }

    fn clean(&mut self, frame: &Frame, offset: usize, bytes: usize)
    {
        if let Some(view) = physical_view(frame.physical)
        {
            // SAFETY: the range lies inside the firmware buffer, which the
            // platform's physical map covers.
            unsafe { clean(view + offset, bytes) };
        }
    }

    fn now(&mut self) -> Result<u64, Refusal>
    {
        self.host.monotonic_microseconds().map_err(|_| Refusal::Clock)
    }
}

/// The bounds the platform record declares, checked at build time.
pub const fn bounds() -> Bounds
{
    Bounds { initial: Size { width:  DISPLAY_WIDTH,
                             height: DISPLAY_HEIGHT },
             maximum: Size { width:  DISPLAY_MAX_WIDTH,
                             height: DISPLAY_MAX_HEIGHT } }
}

static DEVICE: spinning_top::Spinlock<Option<(Device<Machine>, Engine)>> =
    spinning_top::Spinlock::new(None);

pub struct VideoCoreFramebuffer;
pub static DRIVER: VideoCoreFramebuffer = VideoCoreFramebuffer;
impl mac_driver::Driver for VideoCoreFramebuffer
{
    fn initialize(&self, host: Client) -> DriverResult<()>
    {
        let mut slot = DEVICE.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
        if slot.is_some()
        {
            return Err(Error(mac_abi::SERVICE_BUSY));
        }
        // Construction resolves the services only; the mailbox is claimed and
        // the mode applied when System attaches.
        *slot = Some((Device::new(Machine::new(host)?, bounds()), Engine::new()));
        Ok(())
    }

    fn request(&self, request: mac_driver::Request<'_>, output: &mut [u8])
               -> DriverResult<Completion>
    {
        let mut slot = DEVICE.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
        let (device, engine) = slot.as_mut().ok_or(Error(mac_abi::NOT_FOUND))?;
        // The wire reply is a status code, so the engine's named refusal cannot
        // travel with it. It stays in the engine for a diagnostic caller; System
        // sees the status, which is what the contract defines.
        engine.request(device, request, output)
    }

    fn quiesce(&self) -> DriverResult<()>
    {
        let mut slot = DEVICE.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
        *slot = None;
        Ok(())
    }
}

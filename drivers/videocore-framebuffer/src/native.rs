//! The one audited machine adapter: the mailbox register window, the Device DMA
//! allocations and the CPU view of the firmware buffer. It contains no cache
//! instruction and no address it was compiled with: the message the firmware
//! reads lives in storage System allocates for a device to access, the
//! firmware's buffer is seen through a view System grants, and System keeps
//! both in step with the device. Everything above it is safe code.
#![allow(unsafe_code)]
use core::ptr::copy_nonoverlapping;

use mac_driver::{Client, Completion, Error, Phase, Result as DriverResult,
                 devices::{DeviceService, interface}};
use mac_video_interface::Size;
use mac_videocore_fb::{Channel, Error as MailboxError, Message, WORDS,
                       registers::{Mailbox, WINDOW_BYTES}};

use crate::{Bounds, Device, Engine, Refusal, library_mode,
            scanout::{Allocation, Frame, Plan, Platform}};

include!(concat!(env!("OUT_DIR"), "/platform-display.rs"));

/// The mailbox's inventory key, the same value `module.json` matches on.
///
/// The key is the first sixteen bytes of the name the machine's own tree gives
/// the part, `brcm,bcm2835-mbox`, so it ends one letter short. It used to be
/// `brcm,mbox`, a name no tree states, and the module was skipped as having no
/// device on the one machine it exists for.
const MAILBOX_KEY: [u8; 16] = *b"brcm,bcm2835-mbo";
/// Deferred allocation deadline. The resident services the request from its own
/// loop; a mailbox that never answers is a defect, not a wait to extend.
const ALLOCATION_TIMEOUT_MICROSECONDS: u64 = 1_000_000;

/// The mailbox as the library's channel.
///
/// The message it carries lives in storage System allocated for the firmware
/// to read and write. No cache holds a copy of that storage, so an exchange
/// needs no maintenance on either side of the doorbell.
struct Coherent
{
    mailbox: Mailbox
}
impl Channel for Coherent
{
    fn transact(&mut self, address: u32, words: &mut [u32]) -> Result<(), MailboxError>
    {
        self.mailbox.transact(address, words)
    }
}

/// The firmware's buffer for one mode, as System's view of it.
#[derive(Clone, Copy)]
struct View
{
    physical: u64,
    bytes:    u64,
    handle:   u64,
    cpu:      usize
}

pub struct Machine
{
    host:    Client,
    service: DeviceService,
    claim:   u64,
    /// The message storage: its address as the firmware uses it, and as this
    /// module does. An address rather than a pointer so the adapter stays
    /// `Send`; it is only ever turned back into the one message borrow.
    message: u32,
    words:   usize,
    mailbox: Option<Coherent>,
    view:    Option<View>
}
impl Machine
{
    pub fn new(host: Client) -> DriverResult<Self>
    {
        Ok(Self { host,
                  service: DeviceService::new(host)?,
                  claim: 0,
                  message: 0,
                  words: 0,
                  mailbox: None,
                  view: None })
    }

    /// Find the mailbox in the machine's inventory, claim it and map the
    /// message buffer. The register window is the one the claim states, which
    /// is the machine's tree speaking: this module is compiled with no device
    /// address and adds no offset of its own.
    fn discover(&mut self) -> DriverResult<()>
    {
        if self.mailbox.is_some()
        {
            return Ok(());
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
            // The tree must state at least the registers this driver reaches.
            // A shorter window is some other description of the part, and
            // reading past what the machine stated is not this driver's call.
            if description.register_bytes < WINDOW_BYTES as u64
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
            self.allocate_message()?;
            let base = usize::try_from(claim.registers).map_err(|_| Error(mac_abi::BAD_ABI))?;
            // SAFETY: the window this module exclusively claims, at the address
            // the claim states and long enough for every mailbox register.
            self.mailbox = Some(Coherent { mailbox: unsafe { Mailbox::new(base) } });
            return Ok(());
        }
        Err(Error(mac_abi::NOT_FOUND))
    }

    /// Storage for the one mailbox message, which the firmware reads and
    /// writes at an address it is handed. It is allocated as device-accessed,
    /// so on this board it is memory no cache holds, and whole pages, so the
    /// low four bits the mailbox uses for its channel are clear.
    fn allocate_message(&mut self) -> DriverResult<()>
    {
        let bytes = (WORDS * size_of::<u32>()) as u64;
        let clock = |host: Client| host.monotonic_microseconds().map_err(|_| Error(mac_abi::IO_ERROR));
        let mut ticket = 0;
        let deadline = clock(self.host)?.saturating_add(ALLOCATION_TIMEOUT_MICROSECONDS);
        loop
        {
            let phase = if ticket == 0 { Phase::Start } else { Phase::Poll };
            match self.service.allocate_device_accessed(self.claim, bytes, &mut ticket, phase)?
            {
                (Completion::Complete(_), Some(buffer)) =>
                {
                    self.words =
                        usize::try_from(buffer.cpu_address).map_err(|_| Error(mac_abi::BAD_ABI))?;
                    self.message =
                        u32::try_from(buffer.device_address).map_err(|_| Error(mac_abi::BAD_ABI))?;
                    return Ok(());
                }
                (Completion::Pending, _) if clock(self.host)? < deadline => (),
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

    /// System's view of the firmware buffer `frame` describes, asked for once
    /// per buffer. A mode change brings a different buffer, and the old view
    /// is given back before the new one is taken.
    fn view(&mut self, frame: &Frame) -> Result<View, Refusal>
    {
        let physical = u64::from(frame.physical);
        let bytes = u64::from(frame.size);
        if let Some(view) = self.view
        {
            if view.physical == physical && view.bytes == bytes
            {
                return Ok(view);
            }
            self.service.unmap(view.handle).map_err(|error| Refusal::Dma(error.0))?;
            self.view = None;
        }
        let shared = self.service
                         .shared(self.claim, physical, bytes)
                         .map_err(|error| Refusal::Dma(error.0))?;
        let view = View { physical,
                          bytes,
                          handle: shared.handle,
                          cpu: usize::try_from(shared.cpu_address).map_err(|_| Refusal::NoMemory)?
                        };
        self.view = Some(view);
        Ok(view)
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
                    return Ok(Allocation { handle:         buffer.handle,
                                           address:        buffer.cpu_address,
                                           device_address: buffer.device_address,
                                           length:         buffer.length });
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
        let storage = self.words as *mut [u32; WORDS];
        let mailbox = self.mailbox.as_mut().ok_or(Refusal::NoMemory)?;
        // SAFETY: `discover` succeeded, so this is the page-aligned allocation
        // of at least `WORDS` words System made for this module, which nothing
        // else addresses; `Message` borrows it for this call only.
        let words = unsafe { &mut *storage };
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
        // Hand the service back the allocation it issued. Reconstructing the
        // record with a substituted address would describe a different
        // allocation from the one that was made.
        let mut allocation = interface::Allocation { handle:         allocation.handle,
                                                     cpu_address:    allocation.address,
                                                     device_address: allocation.device_address,
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
        let scanout = self.view(frame)?.cpu;
        let width_words = frame.geometry.width as usize;
        // SAFETY: both ranges are the storage this driver owns for this mode: the
        // surface is a Device allocation of at least `pitch * height` bytes, and
        // the firmware buffer is `size` bytes at a physical address the mailbox
        // reported, seen through the view System granted for exactly that
        // span. The plan was computed from the same geometry and pitch.
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
        // The copy went into cached memory the firmware scans out from behind
        // the caches; System writes the lines out. A refusal here leaves a
        // rectangle to appear late, which the next present of it repairs, and
        // there is no caller to return it to.
        if let Ok(view) = self.view(frame)
        {
            let _ = self.service.sync_for_device(view.handle, offset as u64, bytes as u64);
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
        if let Some((device, _engine)) = slot.as_mut()
        {
            // Every allocation this device opened is handed back before the slot
            // is dropped. Dropping it instead leaves the surface storage owned by
            // nothing, which is the leak the entropy driver's quiesce had.
            device.shutdown().map_err(|refusal| Error(refusal.status()))?;
        }
        *slot = None;
        Ok(())
    }
}

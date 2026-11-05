//! A simulated VideoCore firmware and Device DMA service.
//!
//! The mailbox half is the real library: `Platform::acquire` builds a property
//! message with `mac_videocore_fb` and sends it to a `Channel` that answers the
//! way firmware writing guest memory does. So the encoder, the decoder, the
//! address masking and the driver's engine all run here; only the MMIO, the DMA
//! service and the cache instructions are stood in for.
#![allow(dead_code)]
use std::collections::BTreeMap;

use mac_video_interface::Size;
use mac_videocore_fb::{Channel, Error as MailboxError, Message, Mode, PixelOrder,
                       RESPONSE_SUCCESS, TAG_ALLOCATE_BUFFER, TAG_END, TAG_GET_PITCH,
                       TAG_SET_DEPTH, TAG_SET_PHYSICAL_SIZE, TAG_SET_PIXEL_ORDER,
                       TAG_SET_VIRTUAL_SIZE, WORDS};
use videocore_framebuffer::{Allocation, Bounds, Frame, Plan, Platform, Refusal};

/// The message buffer's fake bus address. Its only requirement is 16-byte
/// alignment.
pub const MESSAGE_ADDRESS: u32 = 0x0001_0000;

#[repr(align(16))]
struct Storage([u32; WORDS]);

/// What the simulated firmware answers with. The defaults are the values QEMU's
/// `raspi3b` model returned for an 800x600x32 request, with the base expressed
/// as the bus address firmware reports rather than the reduced one.
#[derive(Clone, Copy, Debug)]
pub struct Firmware
{
    /// Bus address in the reply. 0xC0100000 reduces to physical 0x00100000.
    pub base:              u32,
    /// Row padding added to the tightly packed stride. Real firmware may pad.
    pub pitch_padding:     u32,
    pub header:            u32,
    pub reject:            Option<u32>,
    pub accepted:          Option<Size>,
    pub depth:             Option<u32>,
    pub allocate_returned: u32,
    pub omit:              Option<u32>
}
impl Firmware
{
    pub const fn qemu() -> Self
    {
        Self { base:              0xc010_0000,
               pitch_padding:     0,
               header:            RESPONSE_SUCCESS,
               reject:            None,
               accepted:          None,
               depth:             None,
               allocate_returned: 8,
               omit:              None }
    }

    pub const fn with_base(base: u32) -> Self
    {
        Self { base,
               ..Self::qemu() }
    }
}

/// The geometry the request asked for, read out of the message being answered.
fn requested_geometry(words: &[u32]) -> (u32, u32)
{
    let mut at = 2;
    loop
    {
        match words[at]
        {
            TAG_SET_PHYSICAL_SIZE => return (words[at + 3], words[at + 4]),
            TAG_END => return (0, 0),
            _ => at += 3 + (words[at + 1] / 4) as usize
        }
    }
}

/// The firmware's side of one property message.
fn respond(words: &mut [u32], firmware: &Firmware)
{
    assert_eq!(words[0] as usize, words.len() * 4, "request size word");
    words[1] = firmware.header;
    if firmware.header != RESPONSE_SUCCESS
    {
        return;
    }
    let (width, height) = requested_geometry(words);
    let pitch = width * 4 + firmware.pitch_padding;
    let mut at = 2;
    loop
    {
        let id = words[at];
        if id == TAG_END
        {
            return;
        }
        let declared = words[at + 1];
        let value = at + 3;
        let count = (declared / 4) as usize;
        let returned = match id
        {
            TAG_SET_PHYSICAL_SIZE | TAG_SET_VIRTUAL_SIZE =>
            {
                if let Some(size) = firmware.accepted
                {
                    words[value] = size.width;
                    words[value + 1] = size.height;
                }
                declared
            }
            TAG_SET_DEPTH =>
            {
                if let Some(depth) = firmware.depth
                {
                    words[value] = depth;
                }
                declared
            }
            TAG_SET_PIXEL_ORDER => declared,
            TAG_ALLOCATE_BUFFER =>
            {
                words[value] = firmware.base;
                words[value + 1] = pitch * height;
                firmware.allocate_returned
            }
            TAG_GET_PITCH =>
            {
                words[value] = pitch;
                declared
            }
            _ => 0
        };
        let refused = firmware.reject == Some(id);
        words[at + 2] = if refused { 0 } else { RESPONSE_SUCCESS | returned };
        if firmware.omit == Some(id)
        {
            words[at] = 0x0004_0009;
        }
        at = value + count;
    }
}

struct SimChannel
{
    firmware: Firmware
}
impl Channel for SimChannel
{
    fn transact(&mut self, _address: u32, words: &mut [u32]) -> Result<(), MailboxError>
    {
        respond(words, &self.firmware);
        Ok(())
    }
}

#[derive(Debug)]
pub struct SimAllocation
{
    pub address: u64,
    pub pixels:  Vec<u32>
}

pub struct SimPlatform
{
    pub firmware:      Firmware,
    /// Every acquired mode, in order: (geometry, pitch, physical base).
    pub acquired:      Vec<(Size, u32, u32)>,
    pub scanout:       Vec<u32>,
    pub pitch:         u32,
    pub physical:      u32,
    pub allocations:   BTreeMap<u64, SimAllocation>,
    /// Every cache-clean request: (physical, offset, bytes).
    pub cleaned:       Vec<(u32, usize, usize)>,
    pub freed:         Vec<u64>,
    /// Refuse the next Device DMA allocation.
    pub out_of_memory: bool,
    next_handle:       u64,
    next_address:      u64,
    clock:             u64
}
impl SimPlatform
{
    pub fn new(firmware: Firmware) -> Self
    {
        Self { firmware,
               acquired: Vec::new(),
               scanout: Vec::new(),
               pitch: 0,
               physical: 0,
               allocations: BTreeMap::new(),
               cleaned: Vec::new(),
               freed: Vec::new(),
               out_of_memory: false,
               next_handle: 0x1000_0000_0000,
               next_address: 0xffff_0000_0000_0000,
               clock: 1 }
    }

    /// The drawing surface for `handle`.
    pub fn surface(&mut self, handle: u64) -> &mut [u32]
    {
        self.allocations.get_mut(&handle).expect("the surface is still owned").pixels.as_mut_slice()
    }

    /// Read one pixel from the firmware's buffer, using the pitch it reported.
    pub fn scanout_pixel(&self, x: usize, y: usize) -> u32
    {
        self.scanout[y * (self.pitch as usize / 4) + x]
    }

    pub fn holds(&self, handle: u64) -> bool
    {
        self.allocations.contains_key(&handle)
    }

    /// The CPU address the Device service would have reported for `handle`.
    pub fn address_of(&self, handle: u64) -> u64
    {
        self.allocations.get(&handle).map_or(0, |allocation| allocation.address)
    }

    pub fn length_of(&self, handle: u64) -> u64
    {
        self.allocations.get(&handle).map_or(0, |allocation| allocation.pixels.len() as u64 * 4)
    }
}
impl Platform for SimPlatform
{
    fn acquire(&mut self, geometry: Size, alignment: u32) -> Result<Frame, Refusal>
    {
        let mut storage = Box::new(Storage([0; WORDS]));
        let mut message = Message::new(&mut storage.0, MESSAGE_ADDRESS)?;
        let mode = Mode { width:       geometry.width,
                          height:      geometry.height,
                          depth:       32,
                          pixel_order: PixelOrder::Bgr };
        let mut channel = SimChannel { firmware: self.firmware };
        let framebuffer = mac_videocore_fb::acquire(&mut channel, &mut message, mode, alignment)?;
        // A new mode means a new buffer: the previous contents are gone.
        let words = (framebuffer.pitch as usize / 4) * geometry.height as usize;
        self.scanout = vec![0xdead_beef; words];
        self.pitch = framebuffer.pitch;
        self.physical = framebuffer.base;
        self.acquired.push((geometry, framebuffer.pitch, framebuffer.base));
        Ok(Frame { physical: framebuffer.base,
                   size: framebuffer.size,
                   pitch: framebuffer.pitch,
                   geometry })
    }

    fn allocate(&mut self, bytes: u64) -> Result<Allocation, Refusal>
    {
        if self.out_of_memory
        {
            return Err(Refusal::NoMemory);
        }
        let handle = self.next_handle;
        self.next_handle += 1;
        let address = self.next_address;
        self.next_address += 0x10_0000;
        let words = (bytes as usize).div_ceil(4);
        self.allocations.insert(handle,
                                SimAllocation { address,
                                                pixels: vec![0; words] });
        Ok(Allocation { handle,
                        address,
                        length: bytes })
    }

    fn free(&mut self, allocation: Allocation) -> Result<(), Refusal>
    {
        if self.allocations.remove(&allocation.handle).is_none()
        {
            return Err(Refusal::UnknownAllocation { allocation: allocation.handle });
        }
        self.freed.push(allocation.handle);
        Ok(())
    }

    fn blit(&mut self, allocation: Allocation, frame: &Frame, plan: &Plan) -> Result<(), Refusal>
    {
        let source = self.allocations
                         .get(&allocation.handle)
                         .ok_or(Refusal::UnknownAllocation { allocation: allocation.handle })?;
        let width = frame.geometry.width as usize;
        let pitch_words = frame.pitch as usize / 4;
        for row in 0..plan.rows
        {
            let from = plan.source_words + row * width;
            let to = plan.destination_bytes / 4 + row * pitch_words;
            self.scanout[to..to + plan.row_words].copy_from_slice(&source.pixels
                                                                      [from..from
                                                                             + plan.row_words]);
        }
        Ok(())
    }

    fn clean(&mut self, frame: &Frame, offset: usize, bytes: usize)
    {
        self.cleaned.push((frame.physical, offset, bytes));
    }

    fn now(&mut self) -> Result<u64, Refusal>
    {
        self.clock += 1;
        Ok(self.clock)
    }
}

/// The bounds `platforms/qemu_arm64.json` declares, so the tests exercise the
/// same range the record does without reading the file.
pub const RECORD_BOUNDS: Bounds = Bounds { initial: Size { width:  800,
                                                           height: 600 },
                                           maximum: Size { width:  1920,
                                                           height: 1200 } };

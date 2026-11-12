//! Mailbox framebuffer init: ask the firmware for its linear framebuffer at
//! the platform record's mode and paint the boot screen into it.
//!
//! The bootloader does nothing else with the display. It keeps no display
//! state: the kernel re-acquires through its own videocore-framebuffer driver
//! and its own mailbox request, and the kernel's mode is authoritative. The
//! paint here is the desktop pattern with the happy Mac at its centre, drawn
//! by the shared boot-screen crate so this board shows what every other
//! platform's boot stage shows while the System loads.
use mac_devicetree::Tree;
use mac_videocore_fb::{Channel, Error as MailboxError, Framebuffer, Message, Mode, PixelOrder,
                       WORDS, registers::Mailbox};

use crate::{DISPLAY_HEIGHT, DISPLAY_WIDTH, cache};

/// The mailbox `compatible` the firmware tree names. The window itself comes
/// from the tree; this is only the vocabulary the tree uses.
const MAILBOX_COMPATIBLE: &str = "brcm,bcm2835-mbox";

/// The message buffer the firmware reads and writes through the bus.
///
/// It is aligned to a cache line and fills whole lines (64 words is four
/// 64-byte lines), and it is a static, not a stack local. Both matter on real
/// hardware. The transport cleans the buffer before the doorbell and
/// invalidates it after the reply; a buffer that shares a line with anything
/// else lets an unrelated store dirty that line while the firmware is
/// answering, and the stale line then replaces the firmware's reply. Observed
/// on a BCM2837 with the buffer on the stack at 16-byte alignment: the
/// firmware acknowledged the request and the reply still read back as the
/// request, response code zero. An emulator has no caches, so only hardware
/// shows it. The mailbox's own requirement, 16-byte alignment, is covered.
#[repr(align(64))]
struct Storage([u32; WORDS]);
static mut MESSAGE: Storage = Storage([0; WORDS]);

/// Why the display init refused, named so the serial line can say it.
pub enum Error
{
    /// The tree names no mailbox window.
    NoMailbox,
    /// The tree walk itself refused.
    Tree(mac_devicetree::Error),
    /// The mailbox property exchange refused.
    Mailbox(MailboxError)
}

impl core::fmt::Display for Error
{
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::NoMailbox => write!(formatter, "no mailbox window in the device tree"),
            Self::Tree(error) => write!(formatter, "device tree refused: {error}"),
            Self::Mailbox(error) => write!(formatter, "mailbox refused: {error}")
        }
    }
}

/// The mailbox with the message buffer's cache maintenance attached. The
/// library's encoder and decoder stay unchanged; only the transport knows
/// that the buffer is shared with another agent.
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
        // The firmware reads the request from memory, so the stores must be
        // visible before the doorbell; the firmware writes the reply into the
        // same storage, so the cached copy is discarded after.
        cache::clean(pointer, bytes);
        let result = self.mailbox.transact(address, words);
        cache::invalidate(pointer, bytes);
        result
    }
}

/// Ask the firmware for the framebuffer at the platform record's mode and
/// paint the boot screen. The mode is the record's, not a guess: the kernel
/// asks for the same mode through its own driver, and the kernel's mode is
/// authoritative from then on.
pub fn init(tree: &Tree) -> Result<Framebuffer, Error>
{
    let Some((base, _bytes)) = tree.window(MAILBOX_COMPATIBLE).map_err(Error::Tree)?
    else
    {
        return Err(Error::NoMailbox);
    };
    // SAFETY: the primary core is the only user of this window for the whole
    // run, and the address comes from the machine's tree.
    let mut channel = Coherent { mailbox: unsafe { Mailbox::new(base as usize) } };
    let mode = Mode { width:       DISPLAY_WIDTH,
                      height:      DISPLAY_HEIGHT,
                      depth:       32,
                      pixel_order: PixelOrder::Bgr };
    // SAFETY: the bootrom is single-threaded and this is the only use of the
    // static, so the exclusive borrow cannot alias. Its address is physical
    // under the boot identity map.
    let storage = unsafe { &mut *core::ptr::addr_of_mut!(MESSAGE) };
    let address = storage.0.as_ptr() as u32;
    let mut message = Message::new(&mut storage.0, address).map_err(Error::Mailbox)?;
    let framebuffer =
        mac_videocore_fb::acquire(&mut channel, &mut message, mode, 4096).map_err(Error::Mailbox)?;
    paint(&framebuffer);
    USB_POWER.store(power_usb(&mut channel, &mut message),
                    core::sync::atomic::Ordering::Relaxed);
    Ok(framebuffer)
}

/// What the firmware answered when asked to power the USB controller: bit 0
/// set is on, bit 1 set is "no such device". `u32::MAX` when it was not asked
/// or gave no answer.
pub static USB_POWER: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(u32::MAX);

/// The firmware's power-state property, and its name for the USB controller.
const TAG_SET_POWER_STATE: u32 = 0x0002_8001;
const POWER_DEVICE_USB: u32 = 3;
/// Turn it on, and answer only once it is stable.
const POWER_ON_AND_WAIT: u32 = 0b11;

/// Ask the firmware to power the USB controller.
///
/// The controller's registers do not answer until its power domain is on, and
/// the firmware owns that domain. This is asked here, while the boot stage
/// holds the mailbox, because nothing in the System shares the mailbox yet:
/// its one claimant is the display driver. It belongs in the System, behind a
/// service a USB driver can ask, and should move there when one exists.
fn power_usb(channel: &mut Coherent, message: &mut Message) -> u32
{
    message.reset();
    if message.request(TAG_SET_POWER_STATE, &[POWER_DEVICE_USB, POWER_ON_AND_WAIT]).is_err()
       || message.finish().is_err()
       || message.transact(channel).is_err()
       || message.check().is_err()
    {
        return u32::MAX;
    }
    message.reply(TAG_SET_POWER_STATE, 2).map_or(u32::MAX, |words| words[1])
}

/// The boot screen: the desktop pattern and the happy Mac, from the shared
/// boot-screen crate. Drawing goes through the pitch the
/// firmware returned, so a padded row is honoured rather than assumed away.
/// A framebuffer this code cannot describe as 32-bit words is left as the
/// firmware made it: the kernel's driver is the authoritative display path.
fn paint(framebuffer: &Framebuffer)
{
    let pitch = framebuffer.pitch as usize;
    let height = framebuffer.height as usize;
    let Some(bytes) = pitch.checked_mul(height)
    else
    {
        return;
    };
    if framebuffer.depth != 32 || pitch % 4 != 0 || bytes > framebuffer.size as usize
    {
        return;
    }
    // SAFETY: the firmware allocated `size` bytes at `base` for this scanout,
    // the checks above keep the slice inside that allocation, the base is
    // word-aligned because the allocation was requested page-aligned, and
    // nothing else in the bootrom touches the framebuffer.
    let pixels =
        unsafe { core::slice::from_raw_parts_mut(framebuffer.base as usize as *mut u32, bytes / 4) };
    if let Some(mut raster) =
        mac_boot_screen::Raster::strided(pixels, framebuffer.width as usize, height, pitch / 4)
    {
        // The boot stage's screen, as the other platforms' boot stages draw it:
        // the desktop pattern with the happy Mac alone at the centre, at its
        // native 32 by 32. The welcome panel with the title and the progress
        // bar is the System's, drawn once it is running.
        raster.background();
        let left = (framebuffer.width as usize).saturating_sub(32) / 2;
        let top = height.saturating_sub(32) / 2;
        raster.icon(mac_boot_screen::HAPPY, left as i16, top as i16, 1);
    }
    // The scanout reads DRAM, so the painted lines only reach the display
    // after they are cleaned to the point of coherency.
    cache::clean(framebuffer.base as usize, bytes);
}

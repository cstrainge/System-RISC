//! Property-channel queries over the VideoCore mailbox. The encoder and the
//! reply checks live in `mac-videocore-fb`; this module only names the tags the
//! self-test asks for and reduces each reply to the value the report prints.
use mac_videocore_fb::{Error, Message, WORDS, registers::Mailbox};

use crate::platform::MAILBOX_BASE;

pub const TAG_BOARD_REVISION: u32 = 0x0001_0002;
pub const TAG_FIRMWARE_REVISION: u32 = 0x0000_0001;
pub const TAG_GET_CLOCK_RATE: u32 = 0x0003_0002;
pub const TAG_GET_ARM_MEMORY: u32 = 0x0001_0005;
/// Clock id for the core clock, as the firmware names it.
pub const CLOCK_CORE: u32 = 4;

/// The message buffer the firmware reads and writes through the bus. It lives
/// on the stack, so with the MMU off its address is already physical.
#[repr(align(16))]
pub struct Storage(pub [u32; WORDS]);

impl Storage
{
    pub const fn new() -> Self
    {
        Self([0; WORDS])
    }
}

pub fn mailbox() -> Mailbox
{
    // SAFETY: the primary core is the only user of this window for the whole
    // run, and the address comes from the platform record.
    unsafe { Mailbox::new(MAILBOX_BASE) }
}

pub fn message(storage: &mut Storage) -> Result<Message<'_>, Error>
{
    let address = storage.0.as_ptr() as u32;
    Message::new(&mut storage.0, address)
}

/// Send one read-only tag and return its first two value words. The caller has
/// already checked the reply's structure through `Message::check`.
pub fn query(message: &mut Message, mailbox: &mut Mailbox, tag: u32, request: &[u32])
             -> Result<[u32; 2], Error>
{
    message.reset();
    message.request(tag, request)?;
    message.finish()?;
    message.transact(mailbox)?;
    message.check()?;
    let words = message.reply(tag, request.len())?;
    let mut value = [0u32; 2];
    for (index, word) in words.iter().enumerate()
    {
        value[index] = *word;
    }
    Ok(value)
}

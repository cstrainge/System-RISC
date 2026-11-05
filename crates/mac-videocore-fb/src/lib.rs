//! VideoCore IV mailbox property-channel framebuffer acquisition.
//!
//! The BCM2837-class firmware owns display bring-up. The ARM side asks it for a
//! single linear framebuffer over mailbox channel 8 and is told where that
//! buffer is. This crate builds those property messages and decodes the reply.
//! It performs no I/O and no allocation: the caller supplies the transport (the
//! register block, from the platform record) and a fixed message buffer.
//!
//! The hardware interface is described in `docs/VIDEOCORE-2D-DESIGN.md`,
//! section 4. The facts this module depends on, and the pitfalls they create:
//!
//! * A message is a 32-bit word buffer whose *bus* address is handed to the mailbox. Word 0 is the
//!   total buffer size in bytes, word 1 the request/response code, then tags, then a zero tag.
//! * A tag's value-buffer size is the buffer's *capacity*, not the request's shape. `allocate
//!   buffer` sends a four-byte alignment but the firmware replies with eight bytes (base and size),
//!   so its value buffer is declared as two words and the second is left zero.
//! * Every reply carries its own code. Bit 31 clear means the firmware did not honour the tag; bits
//!   30:0 are the number of bytes it actually returned. Both are checked, never assumed.
//! * A setter that cannot apply the request returns the previous value instead of failing, so the
//!   echoed value is compared with the request.
//! * The allocate reply's base is a bus address. On a 1 GiB part the 0x0-based RAM alias equals the
//!   ARM physical address, so the address is reduced by masking with [`BUS_ADDRESS_MASK`]. The
//!   0xC0000000 uncached alias is *not* added: masking is correct under both conventions, adding is
//!   not.
#![no_std]
#![deny(unsafe_code)]
use core::fmt;

pub mod registers;

/// Mailbox channel carrying ARM requests and firmware responses.
pub const PROPERTY_CHANNEL: u32 = 8;
/// A mailbox word carries the message's 16-byte aligned bus address in bits
/// 31:4, so a message buffer must be aligned to this many bytes.
pub const ADDRESS_ALIGNMENT: u32 = 16;
/// Reduction from the bus alias to the ARM physical address. Correct for both
/// the 0x0-based alias and the 0xC0000000 uncached alias on a 1 GiB part.
pub const BUS_ADDRESS_MASK: u32 = 0x3fff_ffff;
/// Request/response code bit meaning "this was answered". In the header word it
/// is the whole success code; in a tag word it prefixes the returned length.
pub const RESPONSE_SUCCESS: u32 = 0x8000_0000;
/// Bits of a tag response code holding the number of bytes actually returned.
pub const RESPONSE_LENGTH_MASK: u32 = 0x7fff_ffff;

/// End of the tag list.
pub const TAG_END: u32 = 0x0000_0000;
/// Set the visible mode. Value: width, height.
pub const TAG_SET_PHYSICAL_SIZE: u32 = 0x0004_8003;
/// Set the buffer geometry, which may exceed the visible mode. Width, height.
pub const TAG_SET_VIRTUAL_SIZE: u32 = 0x0004_8004;
/// Bits per pixel. Value: depth.
pub const TAG_SET_DEPTH: u32 = 0x0004_8005;
/// 0 = RGB, 1 = BGR. Value: order.
pub const TAG_SET_PIXEL_ORDER: u32 = 0x0004_8006;
/// Request value: alignment. Reply: base bus address, size in bytes.
pub const TAG_ALLOCATE_BUFFER: u32 = 0x0004_0001;
/// Bytes per line. The reply is authoritative; it may exceed width × depth.
pub const TAG_GET_PITCH: u32 = 0x0004_0008;
/// Scroll inside the virtual buffer. Value: x, y.
pub const TAG_SET_VIRTUAL_OFFSET: u32 = 0x0004_8009;

/// Words in the fixed message buffer. The acquisition request occupies 32
/// words; the remainder bounds a reply the firmware may pad.
pub const WORDS: usize = 64;
/// Words in the message header: total size and the request/response code.
const HEADER_WORDS: usize = 2;

/// Channel pixel order, as the firmware names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelOrder
{
    Rgb,
    Bgr
}
impl PixelOrder
{
    const fn code(self) -> u32
    {
        match self
        {
            Self::Rgb => 0,
            Self::Bgr => 1
        }
    }
}

/// The geometry requested from the firmware. The accepted geometry comes back
/// in [`Framebuffer`]; the firmware may refuse a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode
{
    pub width:       u32,
    pub height:      u32,
    pub depth:       u32,
    pub pixel_order: PixelOrder
}

/// A framebuffer the firmware allocated, as the ARM must use it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Framebuffer
{
    /// ARM physical base. Already reduced from the reply's bus address.
    pub base:   u32,
    /// Bytes allocated, at least `pitch * height`.
    pub size:   u32,
    /// Bytes per line. May exceed `width * depth / 8`.
    pub pitch:  u32,
    pub width:  u32,
    pub height: u32,
    pub depth:  u32
}
impl Framebuffer
{
    /// Number of 32-bit pixels when the scanout is a tightly packed 32-bit
    /// buffer, or `None` when the depth differs or the pitch carries padding a
    /// caller must account for.
    pub fn packed_pixels(&self) -> Option<usize>
    {
        if self.depth != 32 || self.width.checked_mul(4) != Some(self.pitch)
        {
            return None;
        }
        usize::try_from(self.width.checked_mul(self.height)?).ok()
    }
}

/// Every refusal this crate can produce. A failure is always named; nothing
/// falls back to a zeroed descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    /// The message buffer's bus address is not 16-byte aligned.
    MessageAlignment
    {
        address: u32
    },
    /// The requested allocation alignment is zero.
    AllocationAlignment
    {
        alignment: u32
    },
    /// A tag did not fit in the fixed message buffer.
    TooLong
    {
        tag: u32
    },
    /// The message was delivered before [`Message::finish`].
    Unfinished,
    /// The reply's size word does not match the buffer it occupies.
    SizeMismatch
    {
        declared: u32, observed: u32
    },
    /// The header response code was not the success code.
    RequestRefused
    {
        code: u32
    },
    /// The firmware did not honour a tag (its response code has bit 31 clear).
    TagRefused
    {
        tag: u32, code: u32
    },
    /// A tag the acquisition needs is absent from the reply.
    MissingTag
    {
        tag: u32
    },
    /// The firmware returned fewer bytes than the tag's value must contain.
    ShortValue
    {
        tag: u32, needed: u32, returned: u32
    },
    /// The firmware returned more bytes than the tag's value buffer can hold.
    ReturnedTooLong
    {
        tag: u32, declared: u32, returned: u32
    },
    /// A setter echoed a different value than the one requested.
    ValueRefused
    {
        tag: u32, word: u8, requested: u32, observed: u32
    },
    /// The tag list is not well formed at this word offset.
    Malformed
    {
        at: usize
    },
    /// The firmware reported a zero base or a zero size.
    EmptyAllocation,
    /// The pitch cannot hold one row of the accepted geometry.
    PitchTooSmall
    {
        pitch: u32, minimum: u32
    },
    /// The allocation cannot hold `height` rows of `pitch` bytes.
    AllocationTooSmall
    {
        size: u32, minimum: u32
    },
    /// The requested mode is empty or its byte geometry overflows.
    Geometry
    {
        width: u32, height: u32, depth: u32
    },
    /// The mailbox write FIFO never reported room for the request.
    MailboxFull,
    /// The response FIFO stayed empty; the firmware did not answer.
    NoResponse,
    /// The first response word is not this request's address and channel.
    ForeignResponse
    {
        word: u32
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::MessageAlignment { address } =>
            {
                write!(f, "mailbox message address {address:#010x} is not 16-byte aligned")
            }
            Self::AllocationAlignment { alignment } =>
            {
                write!(f, "framebuffer allocation alignment {alignment} must not be zero")
            }
            Self::TooLong { tag } =>
            {
                write!(f, "property tag {tag:#010x} does not fit in the fixed message buffer")
            }
            Self::Unfinished =>
            {
                f.write_str("property message was delivered before its end tag was written")
            }
            Self::SizeMismatch { declared,
                                 observed } =>
            {
                write!(f, "property reply declares {declared} bytes but occupies {observed} bytes")
            }
            Self::RequestRefused { code } =>
            {
                write!(f, "mailbox property request failed with response code {code:#010x}")
            }
            Self::TagRefused { tag,
                               code } =>
            {
                write!(f, "firmware did not honour property tag {tag:#010x} (code {code:#010x})")
            }
            Self::MissingTag { tag } =>
            {
                write!(f, "property reply does not contain tag {tag:#010x}")
            }
            Self::ShortValue { tag,
                               needed,
                               returned } =>
            {
                write!(f, "property tag {tag:#010x} returned {returned} bytes, needs {needed}")
            }
            Self::ReturnedTooLong { tag,
                                    declared,
                                    returned } =>
            {
                write!(f,
                       "property tag {tag:#010x} claims {returned} returned bytes in a {declared} \
                        byte buffer")
            }
            Self::ValueRefused { tag,
                                 word,
                                 requested,
                                 observed } =>
            {
                write!(f,
                       "firmware refused property tag {tag:#010x} word {word}: requested \
                        {requested}, reported {observed}")
            }
            Self::Malformed { at } =>
            {
                write!(f, "property message tag list is malformed at word {at}")
            }
            Self::EmptyAllocation =>
            {
                f.write_str("firmware reported an empty framebuffer allocation")
            }
            Self::PitchTooSmall { pitch,
                                  minimum } =>
            {
                write!(f, "framebuffer pitch {pitch} cannot hold a {minimum} byte row")
            }
            Self::AllocationTooSmall { size,
                                       minimum } =>
            {
                write!(f, "framebuffer allocation of {size} bytes cannot hold {minimum} bytes")
            }
            Self::Geometry { width,
                             height,
                             depth } =>
            {
                write!(f, "requested display mode {width}x{height}x{depth} is not usable")
            }
            Self::MailboxFull =>
            {
                f.write_str("mailbox write FIFO never reported room for the request")
            }
            Self::NoResponse =>
            {
                f.write_str("mailbox property channel did not answer within the bounded poll")
            }
            Self::ForeignResponse { word } =>
            {
                write!(f, "mailbox returned response word {word:#010x} for another request")
            }
        }
    }
}
impl core::error::Error for Error
{
}

/// The transport the caller supplies. On the target this is
/// [`registers::Mailbox`], built from the platform record's register window. A
/// host test supplies a responder that stands in for firmware memory.
///
/// `words` is the whole message, header included, and `address` is its bus
/// address. A real mailbox only needs the address: the firmware reads and
/// writes that memory through the bus, so the slice is untouched. A simulated
/// mailbox writes the reply into the slice instead.
pub trait Channel
{
    fn transact(&mut self, address: u32, words: &mut [u32]) -> Result<(), Error>;
}

/// A tag as it appears in a message, bounded by the words actually present.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tag<'a>
{
    pub id:       u32,
    /// Value-buffer capacity, in bytes, as the request declared it.
    pub declared: u32,
    /// Request code in a request, response code after a transaction.
    pub code:     u32,
    pub value:    &'a [u32]
}
impl Tag<'_>
{
    /// Bytes the firmware says it returned.
    pub const fn returned(&self) -> u32
    {
        self.code & RESPONSE_LENGTH_MASK
    }

    pub const fn succeeded(&self) -> bool
    {
        self.code & RESPONSE_SUCCESS != 0
    }
}

struct Walk<'a>
{
    words: &'a [u32],
    at:    usize,
    done:  bool
}
impl<'a> Walk<'a>
{
    /// A walk always starts after the message header.
    const fn new(words: &'a [u32]) -> Self
    {
        Self { words,
               at: HEADER_WORDS,
               done: false }
    }

    fn next_tag(&mut self) -> Result<Option<Tag<'a>>, Error>
    {
        if self.done
        {
            return Ok(None);
        }
        let at = self.at;
        let Some(header) = self.words.get(at..at + 3)
        else
        {
            return Err(Error::Malformed { at });
        };
        let (id, declared, code) = (header[0], header[1], header[2]);
        self.at += 3;
        if id == TAG_END
        {
            if declared != 0 || code != 0
            {
                return Err(Error::Malformed { at });
            }
            self.done = true;
            if self.at != self.words.len()
            {
                return Err(Error::Malformed { at: self.at });
            }
            return Ok(None);
        }
        if declared & 3 != 0
        {
            return Err(Error::Malformed { at });
        }
        let words = (declared / 4) as usize;
        let Some(value) = self.words.get(self.at..self.at + words)
        else
        {
            return Err(Error::Malformed { at });
        };
        self.at += words;
        Ok(Some(Tag { id,
                      declared,
                      code,
                      value }))
    }
}

/// One fixed property message and its bus address.
///
/// The buffer lives with the caller so its address can be taken once: on the
/// target it is static storage whose physical address the boot code knows, and
/// nothing in the acquisition path allocates.
pub struct Message
{
    address:  u32,
    words:    [u32; WORDS],
    len:      usize,
    finished: bool
}
impl fmt::Debug for Message
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        f.debug_struct("Message")
         .field("address", &self.address)
         .field("bytes", &(self.len * 4))
         .field("finished", &self.finished)
         .finish()
    }
}
impl Message
{
    /// `address` is the buffer's bus address. On a 1 GiB BCM2837 the 0x0-based
    /// RAM alias equals the ARM physical address.
    pub fn new(address: u32) -> Result<Self, Error>
    {
        if address & (ADDRESS_ALIGNMENT - 1) != 0
        {
            return Err(Error::MessageAlignment { address });
        }
        Ok(Self { address,
                  words: [0; WORDS],
                  len: HEADER_WORDS,
                  finished: false })
    }

    pub const fn address(&self) -> u32
    {
        self.address
    }

    /// The words this message occupies: the request while encoding, the reply
    /// after a transaction.
    pub fn words(&self) -> &[u32]
    {
        &self.words[..self.len]
    }

    /// Discard the previous contents and start a fresh request in the same
    /// buffer. The address is unchanged.
    pub fn reset(&mut self)
    {
        self.words.fill(0);
        self.len = HEADER_WORDS;
        self.finished = false;
    }

    fn push_tag(&mut self, id: u32, value: &[u32]) -> Result<(), Error>
    {
        let need = 3 + value.len();
        // One more word is always kept for the end tag.
        if self.len + need + 1 > WORDS
        {
            return Err(Error::TooLong { tag: id });
        }
        self.words[self.len] = id;
        self.words[self.len + 1] = (value.len() * 4) as u32;
        self.words[self.len + 2] = 0;
        self.words[self.len + 3..self.len + 3 + value.len()].copy_from_slice(value);
        self.len += need;
        Ok(())
    }

    /// Visible mode and buffer geometry, depth and channel order.
    pub fn set_mode(&mut self, mode: Mode) -> Result<(), Error>
    {
        self.push_tag(TAG_SET_PHYSICAL_SIZE, &[mode.width, mode.height])?;
        self.push_tag(TAG_SET_VIRTUAL_SIZE, &[mode.width, mode.height])?;
        self.push_tag(TAG_SET_DEPTH, &[mode.depth])?;
        self.push_tag(TAG_SET_PIXEL_ORDER, &[mode.pixel_order.code()])
    }

    /// The request word is an alignment, not a size. The value buffer is two
    /// words because the reply carries base and size; the second word is zero
    /// in the request.
    pub fn allocate(&mut self, alignment: u32) -> Result<(), Error>
    {
        self.push_tag(TAG_ALLOCATE_BUFFER, &[alignment, 0])
    }

    /// Ask for the authoritative row stride.
    pub fn pitch(&mut self) -> Result<(), Error>
    {
        self.push_tag(TAG_GET_PITCH, &[0])
    }

    /// Scroll inside the virtual buffer.
    pub fn offset(&mut self, x: u32, y: u32) -> Result<(), Error>
    {
        self.push_tag(TAG_SET_VIRTUAL_OFFSET, &[x, y])
    }

    /// Write the end tag and the total size, making the message deliverable.
    pub fn finish(&mut self) -> Result<(), Error>
    {
        if self.finished
        {
            return Ok(());
        }
        self.push_tag(TAG_END, &[])?;
        self.words[0] = (self.len * 4) as u32;
        self.words[1] = 0;
        self.finished = true;
        Ok(())
    }

    /// Push the finished message and wait for the firmware's reply, which
    /// lands in this same buffer.
    pub fn transact(&mut self, channel: &mut impl Channel) -> Result<(), Error>
    {
        if !self.finished
        {
            return Err(Error::Unfinished);
        }
        channel.transact(self.address, &mut self.words[..self.len])
    }

    /// Check the reply's structure and codes without interpreting its values.
    /// Every tag must have been honoured and every returned length must fit the
    /// buffer the request declared.
    pub fn check(&self) -> Result<(), Error>
    {
        if self.len < HEADER_WORDS + 1
        {
            return Err(Error::Malformed { at: 0 });
        }
        let observed = (self.len * 4) as u32;
        if self.words[0] != observed
        {
            return Err(Error::SizeMismatch { declared: self.words[0],
                                             observed });
        }
        if self.words[1] != RESPONSE_SUCCESS
        {
            return Err(Error::RequestRefused { code: self.words[1] });
        }
        let mut walk = Walk::new(&self.words[..self.len]);
        while let Some(tag) = walk.next_tag()?
        {
            if !tag.succeeded()
            {
                return Err(Error::TagRefused { tag:  tag.id,
                                               code: tag.code });
            }
            if tag.returned() > tag.declared
            {
                return Err(Error::ReturnedTooLong { tag:      tag.id,
                                                    declared: tag.declared,
                                                    returned: tag.returned() });
            }
        }
        Ok(())
    }

    /// The first tag with this identifier, in a reply that passed [`Self::check`].
    pub fn find(&self, id: u32) -> Result<Tag<'_>, Error>
    {
        let mut walk = Walk::new(&self.words[..self.len]);
        while let Some(tag) = walk.next_tag()?
        {
            if tag.id == id
            {
                return Ok(tag);
            }
        }
        Err(Error::MissingTag { tag: id })
    }

    /// Decode the reply to an [`acquire`] request for `mode`. Every setter's
    /// echo is compared with the request, the allocate reply is reduced to an
    /// ARM physical address, and the pitch is checked against the geometry.
    pub fn framebuffer(&self, mode: Mode) -> Result<Framebuffer, Error>
    {
        self.check()?;
        let allocate = self.value(TAG_ALLOCATE_BUFFER, 8)?;
        let size = allocate.value[1];
        let pitch = self.value(TAG_GET_PITCH, 4)?.value[0];
        self.echo(TAG_SET_PHYSICAL_SIZE, &[mode.width, mode.height])?;
        self.echo(TAG_SET_VIRTUAL_SIZE, &[mode.width, mode.height])?;
        self.echo(TAG_SET_DEPTH, &[mode.depth])?;
        self.echo(TAG_SET_PIXEL_ORDER, &[mode.pixel_order.code()])?;
        let base = allocate.value[0] & BUS_ADDRESS_MASK;
        if base == 0 || size == 0
        {
            return Err(Error::EmptyAllocation);
        }
        let minimum_pitch = mode.width
                                .checked_mul(mode.depth)
                                .map(|bits| bits.div_ceil(8))
                                .ok_or(Error::Geometry { width:  mode.width,
                                                         height: mode.height,
                                                         depth:  mode.depth })?;
        if pitch < minimum_pitch
        {
            return Err(Error::PitchTooSmall { pitch,
                                              minimum: minimum_pitch });
        }
        let minimum_size =
            pitch.checked_mul(mode.height).ok_or(Error::Geometry { width:  mode.width,
                                                                    height: mode.height,
                                                                    depth:  mode.depth })?;
        if size < minimum_size
        {
            return Err(Error::AllocationTooSmall { size,
                                                   minimum: minimum_size });
        }
        Ok(Framebuffer { base,
                         size,
                         pitch,
                         width: mode.width,
                         height: mode.height,
                         depth: mode.depth })
    }

    fn value(&self, id: u32, needed: u32) -> Result<Tag<'_>, Error>
    {
        let tag = self.find(id)?;
        if tag.returned() < needed
        {
            return Err(Error::ShortValue { tag: id,
                                           needed,
                                           returned: tag.returned() });
        }
        Ok(tag)
    }

    fn echo(&self, id: u32, requested: &[u32]) -> Result<(), Error>
    {
        let tag = self.value(id, (requested.len() * 4) as u32)?;
        for (index, request) in requested.iter().enumerate()
        {
            let observed = tag.value[index];
            if observed != *request
            {
                return Err(Error::ValueRefused { tag: id,
                                                 word: index as u8,
                                                 requested: *request,
                                                 observed });
            }
        }
        Ok(())
    }
}

fn validate(mode: Mode) -> Result<(), Error>
{
    if mode.width == 0 || mode.height == 0 || mode.depth == 0
    {
        return Err(Error::Geometry { width:  mode.width,
                                     height: mode.height,
                                     depth:  mode.depth });
    }
    Ok(())
}

/// Request `mode`, allocate the scanout and read back its pitch.
///
/// The request is the sequence the design document measured against the QEMU
/// model: physical size, virtual size, depth, pixel order, allocate buffer, get
/// pitch. The virtual offset is deliberately *not* part of it — it is a scroll
/// control, sent by [`scroll`] once the scanout exists.
pub fn acquire(channel: &mut impl Channel, message: &mut Message, mode: Mode, alignment: u32)
               -> Result<Framebuffer, Error>
{
    validate(mode)?;
    if alignment == 0
    {
        return Err(Error::AllocationAlignment { alignment });
    }
    message.reset();
    message.set_mode(mode)?;
    message.allocate(alignment)?;
    message.pitch()?;
    message.finish()?;
    message.transact(channel)?;
    message.framebuffer(mode)
}

/// Scroll the visible region inside the virtual buffer and check that the
/// firmware applied the requested offset.
pub fn scroll(channel: &mut impl Channel, message: &mut Message, x: u32, y: u32)
              -> Result<(), Error>
{
    message.reset();
    message.offset(x, y)?;
    message.finish()?;
    message.transact(channel)?;
    message.check()?;
    message.echo(TAG_SET_VIRTUAL_OFFSET, &[x, y])
}

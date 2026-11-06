//! Host tests for the property channel: a simulated firmware stands in for the
//! device's memory writes, so the encoder, the transport seam and the decoder
//! are driven through the same sequence the bootrom will use.
use mac_videocore_fb::{Channel, Error, Framebuffer, Message, Mode, PixelOrder, RESPONSE_SUCCESS,
                       TAG_ALLOCATE_BUFFER, TAG_END, TAG_GET_PITCH, TAG_SET_DEPTH,
                       TAG_SET_PHYSICAL_SIZE, TAG_SET_PIXEL_ORDER, TAG_SET_VIRTUAL_OFFSET,
                       TAG_SET_VIRTUAL_SIZE, Tag, WORDS, acquire, scroll};

const ADDRESS: u32 = 0x0001_0000;
const MODE: Mode = Mode { width:       800,
                          height:      600,
                          depth:       32,
                          pixel_order: PixelOrder::Bgr };
const ALIGNMENT: u32 = 4096;

/// The message storage a boot path would own statically. It must be 16-byte
/// aligned, which is the alignment the mailbox requires of the address.
#[repr(align(16))]
struct Storage([u32; WORDS]);
impl Storage
{
    const fn new() -> Self
    {
        Self([0; WORDS])
    }
}

/// What the simulated firmware answers with. The defaults are the values the
/// QEMU 11.1.0 `raspi3b` model returned for an 800x600x32 request.
#[derive(Clone, Copy)]
struct Device
{
    base:              u32,
    size:              u32,
    pitch:             u32,
    /// Header response code.
    header:            u32,
    /// A tag the firmware does not honour; its response code is left at zero.
    reject:            Option<u32>,
    /// A geometry the firmware settles on instead of the requested one.
    accepted:          Option<(u32, u32)>,
    /// A depth the firmware settles on instead of the requested one.
    depth:             Option<u32>,
    /// Bytes the allocate tag claims to have returned.
    allocate_returned: u32,
    /// Rename this tag in the reply, so the decoder cannot find it.
    omit:              Option<u32>
}
impl Device
{
    const fn qemu() -> Self
    {
        Self { base:              0x3c10_0000,
               size:              0x001d_4c00,
               pitch:             0x0c80,
               header:            RESPONSE_SUCCESS,
               reject:            None,
               accepted:          None,
               depth:             None,
               allocate_returned: 8,
               omit:              None }
    }
}

/// The device's side of one property message: read the request out of `words`
/// and write the reply back into the same buffer, exactly as firmware writing
/// guest memory through the bus does.
fn respond(words: &mut [u32], device: &Device)
{
    assert_eq!(words[0] as usize, words.len() * 4, "request size word");
    words[1] = device.header;
    if device.header != RESPONSE_SUCCESS
    {
        return;
    }
    let mut at = 2;
    loop
    {
        let id = words[at];
        if id == TAG_END
        {
            assert_eq!(at + 3, words.len(), "end tag terminates the request");
            return;
        }
        let declared = words[at + 1];
        let value = at + 3;
        let count = (declared / 4) as usize;
        let returned = match id
        {
            TAG_SET_PHYSICAL_SIZE | TAG_SET_VIRTUAL_SIZE =>
            {
                if let Some((width, height)) = device.accepted
                {
                    words[value] = width;
                    words[value + 1] = height;
                }
                declared
            }
            TAG_SET_DEPTH =>
            {
                if let Some(depth) = device.depth
                {
                    words[value] = depth;
                }
                declared
            }
            TAG_SET_PIXEL_ORDER | TAG_SET_VIRTUAL_OFFSET => declared,
            TAG_ALLOCATE_BUFFER =>
            {
                words[value] = device.base;
                words[value + 1] = device.size;
                device.allocate_returned
            }
            TAG_GET_PITCH =>
            {
                words[value] = device.pitch;
                declared
            }
            // An unknown tag is left unhonoured, which is what the firmware
            // does for a tag it does not implement.
            _ => 0
        };
        let refused = device.reject == Some(id);
        words[at + 2] = if refused { 0 } else { RESPONSE_SUCCESS | returned };
        if device.omit == Some(id)
        {
            // An unused identifier keeps the list well formed but makes the tag
            // absent from the decoder's point of view.
            words[at] = 0x0004_0009;
        }
        at = value + count;
    }
}

struct Sim
{
    device:  Device,
    address: Option<u32>
}
impl Sim
{
    const fn new(device: Device) -> Self
    {
        Self { device,
               address: None }
    }
}
impl Channel for Sim
{
    fn transact(&mut self, address: u32, words: &mut [u32]) -> Result<(), Error>
    {
        self.address = Some(address);
        respond(words, &self.device);
        Ok(())
    }
}

/// A channel that answers plausibly and then tampers with the reply, so the
/// decoder's structural checks can be driven from the outside.
struct Tamper(fn(&mut [u32]));
impl Channel for Tamper
{
    fn transact(&mut self, _address: u32, words: &mut [u32]) -> Result<(), Error>
    {
        respond(words, &Device::qemu());
        (self.0)(words);
        Ok(())
    }
}

/// Independent tag walker: the tests must not reuse the decoder they check.
fn tags(words: &[u32]) -> Vec<(u32, u32, u32, &[u32])>
{
    let mut result = Vec::new();
    let mut at = 2;
    loop
    {
        let id = words[at];
        let declared = words[at + 1];
        let code = words[at + 2];
        let count = (declared / 4) as usize;
        if id == TAG_END
        {
            assert_eq!(at + 3, words.len(), "end tag must be the last tag");
            result.push((id, declared, code, &words[at + 3..at + 3]));
            return result;
        }
        result.push((id, declared, code, &words[at + 3..at + 3 + count]));
        at += 3 + count;
    }
}

fn offset_of(words: &[u32], id: u32) -> usize
{
    let mut at = 2;
    loop
    {
        let current = words[at];
        if current == id
        {
            return at;
        }
        assert_ne!(current, TAG_END, "tag {id:#x} is absent");
        at += 3 + (words[at + 1] / 4) as usize;
    }
}

fn encode(storage: &mut Storage) -> Message<'_>
{
    let mut message = Message::new(&mut storage.0, ADDRESS).unwrap();
    message.set_mode(MODE).unwrap();
    message.allocate(ALIGNMENT).unwrap();
    message.pitch().unwrap();
    message.finish().unwrap();
    message
}

#[test]
fn the_request_is_the_documented_tag_sequence_and_ends_with_the_end_tag()
{
    let mut storage = Storage::new();
    let message = encode(&mut storage);
    let words = message.words();
    assert_eq!(words[0] as usize, words.len() * 4, "size word covers the buffer");
    assert_eq!(words[1], 0, "a request carries no response code");
    let tags = tags(words);
    let ids: Vec<u32> = tags.iter().map(|tag| tag.0).collect();
    assert_eq!(ids,
               [TAG_SET_PHYSICAL_SIZE, TAG_SET_VIRTUAL_SIZE, TAG_SET_DEPTH, TAG_SET_PIXEL_ORDER,
                TAG_ALLOCATE_BUFFER, TAG_GET_PITCH, TAG_END]);
    let sizes: Vec<u32> = tags.iter().map(|tag| tag.1).collect();
    assert_eq!(sizes, [8, 8, 4, 4, 8, 4, 0]);
    for (id, _, code, _) in &tags
    {
        assert_eq!(*code, 0, "request tag {id:#x} carries no response code");
    }
    assert_eq!(tags[0].3, [800, 600], "visible mode");
    assert_eq!(tags[1].3, [800, 600], "buffer geometry");
    assert_eq!(tags[2].3, [32], "depth");
    assert_eq!(tags[3].3, [1], "pixel order 1 is BGR");
    // allocate buffer's request value is one alignment word; the second word is
    // the reply's size slot and is zero in the request.
    assert_eq!(tags[4].1, 8, "allocate buffer reserves base and size");
    assert_eq!(tags[4].3, [ALIGNMENT, 0]);
    assert_eq!(tags[5].3, [0], "get pitch sends an empty value");
    assert!(tags[6].3.is_empty(), "the end tag has no value");
    assert_eq!(words.len(), 32, "32 words, 128 bytes");
}

#[test]
fn the_acquisition_sequence_yields_the_physical_base_size_and_pitch()
{
    let mut storage = Storage::new();
    let mut message = Message::new(&mut storage.0, ADDRESS).unwrap();
    let mut sim = Sim::new(Device::qemu());
    let framebuffer = acquire(&mut sim, &mut message, MODE, ALIGNMENT).unwrap();
    assert_eq!(sim.address, Some(ADDRESS), "the mailbox receives the buffer address");
    assert_eq!(framebuffer,
               Framebuffer { base:   0x3c10_0000,
                             size:   0x001d_4c00,
                             pitch:  0x0c80,
                             width:  800,
                             height: 600,
                             depth:  32 });
    assert_eq!(framebuffer.packed_pixels(), Some(800 * 600));
    // The reply the simulator produced is well formed by the decoder's rules.
    message.check().unwrap();
}

#[test]
fn the_bus_address_is_masked_below_and_above_the_0x40000000_boundary()
{
    for (bus, physical) in [(0x3c10_0000u32, 0x3c10_0000u32), // QEMU's 0x0-based alias
                            (0x3fff_f000, 0x3fff_f000),       // below the mask boundary
                            (0xc010_0000, 0x0010_0000),       // 0xC0000000 uncached alias
                            (0xc000_0000, 0x0000_0000)]
    // alias of physical zero
    {
        let mut storage = Storage::new();
        let mut message = Message::new(&mut storage.0, ADDRESS).unwrap();
        let mut sim = Sim::new(Device { base: bus,
                                        ..Device::qemu() });
        let result = acquire(&mut sim, &mut message, MODE, ALIGNMENT);
        if physical == 0
        {
            // Masking an alias of physical zero produces the empty allocation,
            // which must be refused rather than used as a framebuffer.
            assert_eq!(result, Err(Error::EmptyAllocation));
            continue;
        }
        assert_eq!(result.unwrap().base, physical, "bus {bus:#x} reduces to {physical:#x}");
        assert_ne!(result.unwrap().base, bus | 0xc000_0000, "the alias is never added");
    }
}

#[test]
fn a_failed_request_is_a_named_error_and_never_a_zeroed_descriptor()
{
    let cases: [(Device, Error); 8] = [(Device { header: 0x8000_0001,
                                                 ..Device::qemu() },
                                        Error::RequestRefused { code: 0x8000_0001 }),
                                       (Device { header: 0,
                                                 ..Device::qemu() },
                                        Error::RequestRefused { code: 0 }),
                                       (Device { reject: Some(TAG_ALLOCATE_BUFFER),
                                                 ..Device::qemu() },
                                        Error::TagRefused { tag:  TAG_ALLOCATE_BUFFER,
                                                            code: 0 }),
                                       (Device { depth: Some(16),
                                                 ..Device::qemu() },
                                        Error::ValueRefused { tag:       TAG_SET_DEPTH,
                                                              word:      0,
                                                              requested: 32,
                                                              observed:  16 }),
                                       (Device { accepted: Some((640, 480)),
                                                 ..Device::qemu() },
                                        Error::ValueRefused { tag:       TAG_SET_PHYSICAL_SIZE,
                                                              word:      0,
                                                              requested: 800,
                                                              observed:  640 }),
                                       (Device { allocate_returned: 4,
                                                 ..Device::qemu() },
                                        Error::ShortValue { tag:      TAG_ALLOCATE_BUFFER,
                                                            needed:   8,
                                                            returned: 4 }),
                                       (Device { omit: Some(TAG_GET_PITCH),
                                                 ..Device::qemu() },
                                        Error::MissingTag { tag: TAG_GET_PITCH }),
                                       (Device { base: 0,
                                                 ..Device::qemu() },
                                        Error::EmptyAllocation)];
    for (device, expected) in cases
    {
        let mut storage = Storage::new();
        let mut message = Message::new(&mut storage.0, ADDRESS).unwrap();
        let mut sim = Sim::new(device);
        let result = acquire(&mut sim, &mut message, MODE, ALIGNMENT);
        assert_eq!(result, Err(expected), "expected {expected:?}");
        let text = expected.to_string();
        assert!(!text.is_empty(), "every refusal has a message");
    }
}

#[test]
fn geometry_the_allocation_cannot_hold_is_refused()
{
    let mut storage = Storage::new();
    let mut message = Message::new(&mut storage.0, ADDRESS).unwrap();
    // A pitch narrower than one row of the accepted mode.
    let mut sim = Sim::new(Device { pitch: 0x0c00,
                                    ..Device::qemu() });
    assert_eq!(acquire(&mut sim, &mut message, MODE, ALIGNMENT),
               Err(Error::PitchTooSmall { pitch:   0x0c00,
                                          minimum: 0x0c80 }));
    // A pitch that fits one row but an allocation that does not fit the rows.
    let mut sim = Sim::new(Device { size: 0x0c80 * 599,
                                    ..Device::qemu() });
    assert_eq!(acquire(&mut sim, &mut message, MODE, ALIGNMENT),
               Err(Error::AllocationTooSmall { size:    0x0c80 * 599,
                                               minimum: 0x0c80 * 600 }));
}

#[test]
fn a_reply_whose_structure_does_not_match_the_request_is_refused()
{
    type Corruption = (fn(&mut [u32]), &'static str);
    let corruptions: [Corruption; 4] =
        [(|words: &mut [u32]| words[0] = 4, "size word disagrees with the buffer"),
         (|words: &mut [u32]| words[offset_of(words, TAG_GET_PITCH) + 1] = 6,
          "a tag length that is not word aligned"),
         (|words: &mut [u32]| {
              let at = offset_of(words, TAG_ALLOCATE_BUFFER);
              words[at + 2] = RESPONSE_SUCCESS | 16;
          },
          "a returned length larger than the declared buffer"),
         (|words: &mut [u32]| {
              let last = words.len() - 3;
              words[last] = TAG_SET_DEPTH;
              words[last + 1] = 4;
              words[last + 2] = RESPONSE_SUCCESS | 4;
          },
          "a missing end tag")];
    for (tamper, description) in corruptions
    {
        let mut storage = Storage::new();
        let mut message = Message::new(&mut storage.0, ADDRESS).unwrap();
        let mut channel = Tamper(tamper);
        let result = acquire(&mut channel, &mut message, MODE, ALIGNMENT);
        assert!(result.is_err(), "{description} must be refused");
        assert!(matches!(result,
                         Err(Error::Malformed { .. })
                         | Err(Error::SizeMismatch { .. })
                         | Err(Error::ReturnedTooLong { .. })),
                "{description} produced {result:?}");
    }
}

#[test]
fn the_message_buffer_and_alignment_are_checked_before_delivery()
{
    let mut storage = Storage::new();
    assert_eq!(Message::new(&mut storage.0, ADDRESS + 4).unwrap_err(),
               Error::MessageAlignment { address: ADDRESS + 4 });
    // The storage's own alignment matters too: an unaligned array cannot carry
    // a 16-byte aligned address under any page mapping.
    let mut words = [0u32; WORDS + 1];
    let unaligned: &mut [u32; WORDS] = (&mut words[1..]).try_into().unwrap();
    assert!(Message::new(unaligned, ADDRESS).is_err());
    let mut message = Message::new(&mut storage.0, ADDRESS).unwrap();
    message.set_mode(MODE).unwrap();
    assert_eq!(message.transact(&mut Sim::new(Device::qemu())).unwrap_err(), Error::Unfinished);
    assert_eq!(acquire(&mut Sim::new(Device::qemu()),
                       &mut message,
                       Mode { width: 0,
                              ..MODE },
                       0),
               Err(Error::Geometry { width:  0,
                                     height: 600,
                                     depth:  32 }));
    assert_eq!(acquire(&mut Sim::new(Device::qemu()), &mut message, MODE, 0),
               Err(Error::AllocationAlignment { alignment: 0 }));
}

#[test]
fn the_fixed_buffer_refuses_a_message_that_does_not_fit()
{
    let mut storage = Storage::new();
    let mut message = Message::new(&mut storage.0, ADDRESS).unwrap();
    let mut refused = None;
    for _ in 0..16
    {
        if let Err(error) = message.pitch()
        {
            refused = Some(error);
            break;
        }
    }
    assert_eq!(refused, Some(Error::TooLong { tag: TAG_GET_PITCH }));
    assert!(message.words().len() <= WORDS, "no partial tag is written past the buffer");
}

#[test]
fn scrolling_checks_that_the_firmware_applied_the_offset()
{
    let mut storage = Storage::new();
    let mut message = Message::new(&mut storage.0, ADDRESS).unwrap();
    let mut sim = Sim::new(Device::qemu());
    scroll(&mut sim, &mut message, 0, 12).unwrap();
    let tags = tags(message.words());
    assert_eq!(tags.len(), 2, "one offset tag and the end tag");
    assert_eq!(tags[0].0, TAG_SET_VIRTUAL_OFFSET);
    assert_eq!(tags[0].3, [0, 12]);
    // A firmware that ignores the scroll echoes the offset it kept.
    struct Ignored;
    impl Channel for Ignored
    {
        fn transact(&mut self, _address: u32, words: &mut [u32]) -> Result<(), Error>
        {
            let at = offset_of(words, TAG_SET_VIRTUAL_OFFSET);
            words[1] = RESPONSE_SUCCESS;
            words[at + 2] = RESPONSE_SUCCESS | 8;
            words[at + 4] = 0;
            Ok(())
        }
    }
    assert_eq!(scroll(&mut Ignored, &mut message, 0, 12),
               Err(Error::ValueRefused { tag:       TAG_SET_VIRTUAL_OFFSET,
                                         word:      1,
                                         requested: 12,
                                         observed:  0 }));
}

#[test]
fn the_doorbell_word_carries_the_aligned_address_and_the_property_channel()
{
    use mac_videocore_fb::registers::doorbell;
    assert_eq!(doorbell(0x0001_0000), 0x0001_0008);
    assert_eq!(doorbell(0x0001_0000 | 0xf), 0x0001_0008, "the channel bits are not the address");
}

#[test]
fn tag_accessors_expose_the_returned_length_and_honour_flag()
{
    let tag = Tag { id:       TAG_GET_PITCH,
                    declared: 4,
                    code:     RESPONSE_SUCCESS | 4,
                    value:    &[3200] };
    assert!(tag.succeeded());
    assert_eq!(tag.returned(), 4);
    let refused = Tag { code: 0,
                        ..tag };
    assert!(!refused.succeeded());
    assert_eq!(refused.returned(), 0);
}

/// A tag this crate does not model is still a property tag: the caller states
/// the value buffer and reads the reply back by name.
const TAG_BOARD_REVISION: u32 = 0x0001_0002;

struct Revision;
impl Channel for Revision
{
    fn transact(&mut self, _address: u32, words: &mut [u32]) -> Result<(), Error>
    {
        assert_eq!(words[0] as usize, words.len() * 4, "request size word");
        words[1] = RESPONSE_SUCCESS;
        words[4] = RESPONSE_SUCCESS | 4;
        words[5] = 0x00a0_2082;
        Ok(())
    }
}

#[test]
fn a_tag_the_crate_does_not_model_round_trips_through_the_message()
{
    let mut storage = Storage::new();
    let mut message = Message::new(&mut storage.0, ADDRESS).unwrap();
    message.request(TAG_BOARD_REVISION, &[0]).unwrap();
    message.finish().unwrap();
    message.transact(&mut Revision).unwrap();
    message.check().unwrap();
    assert_eq!(message.reply(TAG_BOARD_REVISION, 1).unwrap(), &[0x00a0_2082]);
    assert!(matches!(message.reply(TAG_BOARD_REVISION, 2),
                     Err(Error::ShortValue { tag:      TAG_BOARD_REVISION,
                                             needed:   8,
                                             returned: 4 })));
}

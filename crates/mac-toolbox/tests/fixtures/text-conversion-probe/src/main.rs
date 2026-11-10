//! Guest conversion specifications recovered from the removed host suite.
//! Inputs pass through black_box to retain runtime evaluation in release/LTO.
use std::{error::Error as StdError, hint::black_box};

use mac_text::{Error, MacRomanText, PascalStr};
use mac_toolbox::System;
type Result<T> = std::result::Result<T, Box<dyn StdError>>;
const EXPECTED_CASES: usize = 42;
const GOLDEN: &[u8] = include_bytes!("../../../data/system7-macroman-scalars.bin");
fn require(condition: bool, message: &'static str) -> Result<()>
{
    if condition { Ok(()) } else { Err(message.into()) }
}
struct Probe<'a>
{
    system: System<'a>,
    cases:  usize
}
impl Probe<'_>
{
    fn case(&mut self, name: &str, run: impl FnOnce() -> Result<()>) -> Result<()>
    {
        if let Err(cause) = run()
        {
            self.system.log(&format!("TEXT_CONVERSION_FAIL case={name} cause={cause}\n"))?;
            return Err(cause);
        }
        self.cases += 1;
        self.system.log(&format!("TEXT_CONVERSION_CASE case={name} status=pass\n"))?;
        Ok(())
    }
}
fn run(system: System<'_>) -> Result<usize>
{
    let mut p = Probe { system,
                        cases: 0 };
    p.case("complete-scalar-mapping", || {
         require(GOLDEN.len() == 1024, "independent mapping corpus length")?;
         for code in 0..=255u8
         {
             let start = usize::from(code) * 4;
             let scalar = u32::from_le_bytes(GOLDEN[start..start + 4].try_into()?);
             let expected = char::from_u32(scalar).ok_or("independent mapping scalar invalid")?;
             require(mac_text::character(black_box(code)) == expected,
                     "MacRoman scalar differs from independent mapping")?;
             require(mac_text::byte(black_box(expected)) == Some(code),
                     "inverse scalar differs from independent mapping")?;
         }
         Ok(())
     })?;
    p.case("complete-counted-text-roundtrip", || {
        let bytes: Vec<u8> = (0..=255u8).collect();
        let mut expected = String::new();
        for value in GOLDEN.chunks_exact(4)
        {
            expected.push(char::from_u32(u32::from_le_bytes(value.try_into()?)).ok_or("independent scalar invalid")?);
        }
        let mut utf8 = [0xa5; 768];
        let used = mac_text::decode_into(black_box(&bytes), &mut utf8)?;
        require(&utf8[..used] == expected.as_bytes(), "complete UTF-8 mapping output")?;
        require(utf8[used..].iter().all(|&b| b == 0xa5), "decode modified output tail")?;
        let mut encoded = [0xa5; 256];
        let used = mac_text::encode_into(black_box(&expected), &mut encoded)?;
        require(used == 256 && encoded.as_slice() == bytes.as_slice(), "counted text must not inherit Pascal's 255 limit")
    })?;
    p.case("original-nonascii-sample", || {
         let raw = black_box(&b"\x07Caf\x8e\xdb\xf0\xff"[..]);
         let value = PascalStr::from_exact(raw, 255)?;
         let mut utf8 = [0xa5; 32];
         let used = value.decode_into(&mut utf8)?;
         require(&utf8[..used] == "Café¤\u{f8ff}ˇ".as_bytes(), "original non-ASCII sample")?;
         let mut out = [0xa5; 32];
         let used = mac_text::encode_pascal(black_box("Café¤\u{f8ff}ˇ"), 255, &mut out)?;
         require(&out[..used] == raw && out[used..].iter().all(|&b| b == 0xa5),
                 "non-ASCII Pascal roundtrip and tail")
     })?;
    p.case("original-file-and-path", || {
         for (raw, expected) in [(&b"\x0bFolder/Caf\x8e"[..], "Folder/Café"),
                                 (&b"\x0c/Folder/Caf\x8e"[..], "/Folder/Café")]
         {
             let mut out = [0xa5; 64];
             let used = PascalStr::from_exact(black_box(raw), 255)?.decode_into(&mut out)?;
             require(&out[..used] == expected.as_bytes(), "path conversion")?;
             let used = mac_text::encode_pascal(black_box(expected), 255, &mut out)?;
             require(&out[..used] == raw, "path Pascal conversion")?;
         }
         Ok(())
     })?;
    p.case("counted-control-bytes", || {
         let mut out = [0xa5; 8];
         let used = mac_text::encode_pascal(black_box("\0\r\n"), 255, &mut out)?;
         require(&out[..used] == b"\x03\0\r\n",
                 "codec changed counted control bytes or line endings")
     })?;
    p.case("empty-pascal", || {
         let mut out = [0xa5; 4];
         require(mac_text::encode_pascal(black_box(""), 255, &mut out)? == 1,
                 "empty Pascal length")?;
         require(out == [0, 0xa5, 0xa5, 0xa5], "empty Pascal output and tail")?;
         require(PascalStr::from_exact(black_box(&out[..1]), 255)?.payload().is_empty(),
                 "empty Pascal read")
     })?;
    for capacity in [0u8, 27, 31, 63, 255]
    {
        p.case(&format!("capacity-{capacity}-boundary"), || {
             let text = "é".repeat(usize::from(capacity));
             let mut out = [0xa5; 256];
             let used = mac_text::encode_pascal(black_box(&text), capacity, &mut out)?;
             require(used == usize::from(capacity) + 1 && out[0] == capacity,
                     "capacity boundary length")?;
             require(out[1..used].iter().all(|&b| b == 0x8e), "capacity boundary encoding")?;
             require(out[used..].iter().all(|&b| b == 0xa5), "capacity boundary tail")?;
             let mut utf8 = [0xa5; 765];
             let n =
                 PascalStr::from_exact(black_box(&out[..used]), capacity)?.decode_into(&mut utf8)?;
             require(&utf8[..n] == text.as_bytes(), "capacity boundary readback")
         })?;
        p.case(&format!("capacity-{capacity}-overflow"), || {
             let text = "é".repeat(usize::from(capacity) + 1);
             let mut out = [0xa5; 256];
             let result = mac_text::encode_pascal(black_box(&text), capacity, &mut out);
             require(result
                     == Err(Error::TooLong { capacity: usize::from(capacity),
                                             required: usize::from(capacity) + 1 }),
                     "capacity overflow refusal")?;
             require(out == [0xa5; 256], "capacity overflow modified output")
         })?;
    }
    p.case("missing-length", || {
         require(PascalStr::from_exact(black_box(&[]), 255) == Err(Error::MissingLength),
                 "missing length accepted")
     })?;
    p.case("length-overrun", || {
         require(PascalStr::from_exact(black_box(b"\x03ab"), 255)
                 == Err(Error::LengthOverrun { declared:  3,
                                               available: 2 }),
                 "overrun accepted")
     })?;
    p.case("trailing-exact-data", || {
         require(PascalStr::from_exact(black_box(b"\0trailing"), 255)
                 == Err(Error::TrailingBytes { used:      1,
                                               available: 9 }),
                 "trailing exact data accepted")
     })?;
    p.case("declared-capacity", || {
         require(PascalStr::from_exact(black_box(b"\x02ab"), 1)
                 == Err(Error::TooLong { capacity: 1,
                                         required: 2 }),
                 "declared capacity ignored")
     })?;
    p.case("padded-storage-distinct-from-exact", || {
         require(PascalStr::from_buffer(black_box(b"\x01a\xff\xff"), 31)?.payload() == b"a",
                 "padded storage input")?;
         require(PascalStr::from_exact(black_box(b"\x01a\xff\xff"), 31)
                 == Err(Error::TrailingBytes { used:      2,
                                               available: 4 }),
                 "padded storage silently treated as exact")
     })?;
    p.case("unrepresentable-position", || {
         let mut out = [0xa5; 16];
         let result = mac_text::encode_into(black_box("é🦀"), &mut out);
         require(result
                 == Err(Error::Unrepresentable { character:       '🦀',
                                                 character_index: 1,
                                                 byte_offset:     2 }),
                 "unrepresentable position lost")?;
         require(out == [0xa5; 16], "unrepresentable input modified output")
     })?;
    p.case("pre-85-currency-no-euro-substitution", || {
         require(mac_text::character(black_box(0xdb)) == '¤', "currency table changed")?;
         require(mac_text::byte(black_box('€')).is_none(), "euro silently substituted")
     })?;
    p.case("no-normalization", || {
         require(matches!(MacRomanText::new(black_box("e\u{301}")),
                          Err(Error::Unrepresentable { character:       '\u{301}',
                                                       character_index: 1,
                                                       byte_offset:     1 })),
                 "codec silently normalized unrepresentable text")
     })?;
    p.case("short-encode-output", || {
         let mut out = [0xa5; 1];
         require(mac_text::encode_into(black_box("éé"), &mut out)
                 == Err(Error::OutputTooSmall { required:  2,
                                                available: 1 }),
                 "short encoded output accepted")?;
         require(out == [0xa5; 1], "short encoded output modified")
     })?;
    p.case("short-decode-output", || {
         let mut out = [0xa5; 1];
         require(mac_text::decode_into(black_box(&[0x8e]), &mut out)
                 == Err(Error::OutputTooSmall { required:  2,
                                                available: 1 }),
                 "short decoded output accepted")?;
         require(out == [0xa5; 1], "short decoded output modified")
     })?;
    p.case("short-pascal-output", || {
         let mut out = [0xa5; 2];
         require(mac_text::encode_pascal(black_box("abc"), 255, &mut out)
                 == Err(Error::OutputTooSmall { required:  4,
                                                available: 2 }),
                 "short Pascal output accepted")?;
         require(out == [0xa5; 2], "short Pascal output modified")
     })?;
    for (name, character, byte, utf8_length) in
        [("currency-255", '¤', 0xdb, 510), ("fi-255", 'ﬁ', 0xde, 765), ("fl-255", 'ﬂ', 0xdf, 765)]
    {
        p.case(name, || {
             let mut raw = [byte; 256];
             raw[0] = 255;
             let mut utf8 = [0xa5; 768];
             let used = PascalStr::from_exact(black_box(&raw), 255)?.decode_into(&mut utf8)?;
             let expected = character.to_string().repeat(255);
             require(used == utf8_length && &utf8[..used] == expected.as_bytes(),
                     "maximum UTF-8 expansion")?;
             require(utf8[used..].iter().all(|&b| b == 0xa5), "maximum UTF-8 tail")?;
             let mut encoded = [0xa5; 258];
             let used = mac_text::encode_pascal(black_box(&expected), 255, &mut encoded)?;
             require(used == 256 && encoded[..256] == raw && encoded[256..] == [0xa5; 2],
                     "maximum Pascal roundtrip and tail")
         })?;
    }
    for (character, byte, count) in [('a', b'a', 30),
                                     ('a', b'a', 31),
                                     ('a', b'a', 32),
                                     ('é', 0x8e, 31),
                                     ('é', 0x8e, 32),
                                     ('a', b'a', 255)]
    {
        p.case(&format!("str31-{}-{count}", character as u32), || {
             let original = character.to_string().repeat(count);
             let before = original.clone();
             let mut out = [0xa5; 40];
             let used = mac_text::project_filename_31(black_box(&original), &mut out)?;
             require(used == count.min(31) + 1 && out[0] as usize == count.min(31),
                     "Str31 projection length")?;
             require(out[1..used].iter().all(|&b| b == byte)
                     && out[used..].iter().all(|&b| b == 0xa5),
                     "Str31 prefix/tail")?;
             require(original == before, "projection changed original name")
         })?;
    }
    for (name, text, index) in
        [("str31-unrepresentable", "🦀".to_owned(), 0),
         ("str31-unrepresentable-suffix", format!("{}🦀", "a".repeat(31)), 31)]
    {
        p.case(name, || {
             let before = text.clone();
             let mut out = [0xa5; 40];
             require(mac_text::project_filename_31(black_box(&text), &mut out)
                     == Err(Error::Unrepresentable { character:       '🦀',
                                                     character_index: index,
                                                     byte_offset:     index }),
                     "Str31 failed to validate entire name")?;
             require(out == [0xa5; 40] && text == before, "Str31 refusal changed output/source")
         })?;
    }
    p.case("str31-short-output", || {
         let text = "a".repeat(32);
         let mut out = [0xa5; 31];
         require(mac_text::project_filename_31(black_box(&text), &mut out)
                 == Err(Error::OutputTooSmall { required:  32,
                                                available: 31 }),
                 "short Str31 output accepted")?;
         require(out == [0xa5; 31], "short Str31 output modified")
     })?;
    p.case("str31-native-name-limit", || {
         let text = "a".repeat(256);
         let mut out = [0xa5; 40];
         require(mac_text::project_filename_31(black_box(&text), &mut out)
                 == Err(Error::TooLong { capacity: 255,
                                         required: 256 }),
                 "native name limit ignored")?;
         require(out == [0xa5; 40], "overlong native name modified output")
     })?;
    p.case("owned-format-values", || {
         require(mac_formats::text::encode(black_box("Café"))?.as_slice() == b"Caf\x8e",
                 "owned format encoder")?;
         require(mac_formats::text::decode(black_box(b"Caf\x8e")) == "Café",
                 "owned format decoder")?;
         require(mac_formats::text::pstring(black_box("Café"))?.as_slice() == b"\x04Caf\x8e",
                 "owned Pascal builder")
     })?;
    p.case("owned-format-refusal-causes", || {
        require(matches!(mac_formats::text::encode(black_box("é🦀")), Err(mac_formats::Error::TextConversion(Error::Unrepresentable { character: '🦀', character_index: 1, byte_offset: 2 }))), "owned format lost unrepresentable character or position")?;
        let text = "a".repeat(256);
        require(matches!(mac_formats::text::pstring(black_box(&text)), Err(mac_formats::Error::TextConversion(Error::TooLong { capacity: 255, required: 256 }))), "owned Pascal builder lost capacity refusal")
    })?;
    require(p.cases == EXPECTED_CASES, "guest case inventory disagrees with expected count")?;
    Ok(p.cases)
}
fn main() -> Result<()>
{
    let system = System::new()?;
    match run(system)
    {
        Ok(cases) =>
        {
            system.log(&format!("TEXT_CONVERSION_PASS cases={cases}\n"))?;
            system.log("DESKTOP_READY: text conversion probe pass\n")?;
            Ok(())
        }
        Err(cause) =>
        {
            system.log("DESKTOP_READY: text conversion probe failed\n")?;
            Err(cause)
        }
    }
}

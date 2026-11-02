//! Version 3 JSON preferences persist `font_key`, the resident ASCII family
//! identity, never an installed session ID. Loading resolves only available
//! families; a missing key is an error. Writes use the File Manager transaction.
use core::fmt::{self, Write};

use mac_toolbox::{files::{DataFileSpec, Fork, ROOT_ID},
                  quickdraw::fonts::FontSelection,
                  *};
use serde::{Deserialize, Deserializer, Serialize, Serializer,
            de::{self, Visitor}};
fn diagnostic(system: &System<'_>, stage: &str, error: Error) -> Error
{
    let mut text = crate::Text::<256>::default();
    let _ = writeln!(&mut text, "TEXT_EDIT_PREFERENCES_FAILED stage={stage} error={error}");
    let _ = system.log(text.as_str());
    error
}

pub use crate::font_choices::SIZES;
const KEY_BYTES: usize = 128;
#[derive(Clone, Copy)]
struct FontKey
{
    bytes:  [u8; KEY_BYTES],
    length: u8
}
impl FontKey
{
    fn new(key: &str) -> Result<Self, Error>
    {
        if key.is_empty() || key.len() > KEY_BYTES || !key.is_ascii()
        {
            return Err(Error::CorruptData);
        }
        let mut bytes = [0; KEY_BYTES];
        bytes[..key.len()].copy_from_slice(key.as_bytes());
        Ok(Self { bytes,
                  length: key.len() as u8 })
    }

    fn as_str(&self) -> Result<&str, Error>
    {
        core::str::from_utf8(&self.bytes[..usize::from(self.length)]).map_err(|_| {
                                                                         Error::CorruptData
                                                                     })
    }
}
impl Serialize for FontKey
{
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error>
    {
        serializer.serialize_str(self.as_str().map_err(serde::ser::Error::custom)?)
    }
}
impl<'de> Deserialize<'de> for FontKey
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error>
    {
        struct KeyVisitor;
        impl Visitor<'_> for KeyVisitor
        {
            type Value = FontKey;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result
            {
                formatter.write_str("a nonempty ASCII font key of at most 128 bytes")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<FontKey, E>
            {
                FontKey::new(value).map_err(E::custom)
            }
        }
        deserializer.deserialize_str(KeyVisitor)
    }
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings
{
    version:      u32,
    font_key:     FontKey,
    #[serde(skip)]
    font_family:  i16,
    font_size:    i16,
    wrap:         bool,
    line_numbers: bool,
    ruler_kind:   u8,
    ruler_value:  i32
}
impl Default for Settings
{
    fn default() -> Self
    {
        let mut bytes = [0; KEY_BYTES];
        bytes[..9].copy_from_slice(b"bundled:4");
        Self { version:      3,
               font_key:     FontKey { bytes,
                                       length: 9 },
               font_family:  4,
               font_size:    12,
               wrap:         true,
               line_numbers: false,
               ruler_kind:   0,
               ruler_value:  0 }
    }
}
impl Settings
{
    pub fn font(self) -> FontSelection
    {
        FontSelection { family: self.font_family,
                        size:   self.font_size,
                        face:   0 }
    }

    pub fn from_font(system: &System<'_>, font: FontSelection) -> Result<Self, Error>
    {
        let key = system.quickdraw()
                        .fonts()
                        .key(font.family)
                        .map_err(|error| diagnostic(system, "font_key", error))?;
        Ok(Self { font_key: FontKey::new(key.name())?,
                  font_family: font.family,
                  font_size: font.size,
                  ..Self::default() })
    }

    pub fn options(self) -> Result<textedit::DisplayOptions, Error>
    {
        let ruler = match (self.ruler_kind, self.ruler_value)
        {
            (0, 0) => textedit::Ruler::Off,
            (1, n) if n > 0 && n <= i32::from(u16::MAX) => textedit::Ruler::Column(n as u16),
            (2, n) if n >= 0 && n <= i32::from(i16::MAX) => textedit::Ruler::X(n as i16),
            _ => return Err(Error::CorruptData)
        };
        Ok(textedit::DisplayOptions { wrap: self.wrap,
                                      line_numbers: self.line_numbers,
                                      ruler })
    }

    pub fn with_options(mut self, options: textedit::DisplayOptions) -> Self
    {
        self.wrap = options.wrap;
        self.line_numbers = options.line_numbers;
        (self.ruler_kind, self.ruler_value) = match options.ruler
        {
            textedit::Ruler::Off => (0, 0),
            textedit::Ruler::Column(n) => (1, i32::from(n)),
            textedit::Ruler::X(x) => (2, i32::from(x))
        };
        self
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Error>
    {
        let (settings, used): (Self, usize) =
            serde_json_core::from_slice(bytes).map_err(|_| Error::CorruptData)?;
        if !bytes[used..].iter().all(u8::is_ascii_whitespace)
           || settings.version != 3
           || !SIZES.contains(&settings.font_size)
           || settings.options().is_err()
        {
            return Err(Error::CorruptData);
        }
        Ok(settings)
    }

    pub fn encode(self, bytes: &mut [u8]) -> Result<usize, Error>
    {
        serde_json_core::to_slice(&self, bytes).map_err(|_| Error::NoMemory)
    }

    fn location(system: &System<'_>) -> Result<standard_file::Location, Error>
    {
        let files = system.files();
        for at in 0..files.volume_count()?
        {
            let volume = files.volume_at(at)?;
            if volume.startup
            {
                let folder = files.lookup(volume.id, ROOT_ID, "System Folder/Preferences")?;
                return Ok(standard_file::Location { volume:    volume.id,
                                                    directory: folder.id });
            }
        }
        Err(Error::NotFound)
    }

    pub fn load(system: &System<'_>) -> Result<Self, Error>
    {
        let location =
            Self::location(system).map_err(|error| diagnostic(system, "location", error))?;
        let mut name = [0; 256];
        let name = crate::text(system, 5, &mut name)?;
        let entry = match system.files().lookup(location.volume, location.directory, name)
        {
            Ok(entry) => entry,
            Err(Error::NotFound) => return Self::default().resolve(system),
            Err(error) => return Err(error)
        };
        let mut bytes = [0; 1024];
        let mut file = system.files().open(location.volume, entry.id, Fork::Data)?;
        let length = usize::try_from(file.len()?).map_err(|_| Error::CorruptData)?;
        if length > bytes.len()
        {
            return Err(Error::CorruptData);
        }
        let mut at = 0;
        while at < length
        {
            let read = file.read(&mut bytes[at..length])?;
            if read == 0
            {
                return Err(Error::CorruptData);
            }
            at += read;
        }
        if file.read(&mut [0; 1])? != 0
        {
            return Err(Error::ScanStale);
        }
        file.close()?;
        Self::decode(&bytes[..length])?.resolve(system)
    }

    fn resolve(mut self, system: &System<'_>) -> Result<Self, Error>
    {
        let fonts = system.quickdraw().fonts();
        self.font_family =
            fonts.key_id(self.font_key.as_str()?)
                 .map_err(|error| diagnostic(system, "font_key_resolve", error))?;
        fonts.metrics(self.font()).map_err(|error| diagnostic(system, "font_metrics", error))?;
        Ok(self)
    }

    pub fn save(self, system: &System<'_>) -> Result<(), Error>
    {
        let location =
            Self::location(system).map_err(|error| diagnostic(system, "location", error))?;
        let mut name = [0; 256];
        let name = crate::text(system, 5, &mut name)?;
        let replacing = match system.files().lookup(location.volume, location.directory, name)
        {
            Ok(entry) => Some(entry.id),
            Err(Error::NotFound) => None,
            Err(error) => return Err(error)
        };
        let mut bytes = [0; 1024];
        let length = self.encode(&mut bytes)?;
        let mut writer = system.files()
                               .create_data(DataFileSpec { volume: location.volume,
                                                           parent: location.directory,
                                                           name,
                                                           replacing,
                                                           file_type: TypeCode::new(*b"JSON"),
                                                           creator: TypeCode::new(*b"TEDT") },
                                            length)
                               .map_err(|error| diagnostic(system, "create", error))?;
        writer.write_all(&bytes[..length]).map_err(|error| diagnostic(system, "write", error))?;
        writer.finish().map_err(|error| diagnostic(system, "commit", error))?;
        Ok(())
    }
}

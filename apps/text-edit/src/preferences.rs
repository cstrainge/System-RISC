//! Small, versioned JSON preferences; writes use the File Manager transaction.
use core::fmt::Write;

use mac_toolbox::{files::{DataFileSpec, Fork, ROOT_ID},
                  quickdraw::fonts::FontSelection,
                  *};
use serde::{Deserialize, Serialize};
fn diagnostic(system: &System<'_>, stage: &str, error: Error) -> Error
{
    let mut text = crate::Text::<256>::default();
    let _ = writeln!(&mut text, "TEXT_EDIT_PREFERENCES_FAILED stage={stage} error={error}");
    let _ = system.log(text.as_str());
    error
}

pub use crate::font_choices::{FAMILIES, SIZES};
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings
{
    version:      u32,
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
        Self { version:      2,
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

    pub fn from_font(font: FontSelection) -> Self
    {
        Self { version: 2,
               font_family: font.family,
               font_size: font.size,
               ..Self::default() }
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
           || settings.version != 2
           || !FAMILIES.contains(&settings.font_family)
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
            Err(Error::NotFound) => return Ok(Self::default()),
            Err(error) => return Err(error)
        };
        let mut bytes = [0; 512];
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
        let settings = Self::decode(&bytes[..length])?;
        system.quickdraw().fonts().metrics(settings.font())?;
        Ok(settings)
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
        let mut bytes = [0; 256];
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

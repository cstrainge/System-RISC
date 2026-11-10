//! Versioned Terminal preferences saved through File Manager transactions.
use mac_toolbox::{Error, System, TypeCode,
                  files::{DataFileSpec, Fork, ROOT_ID},
                  standard_file::Location};
use serde::{Deserialize, Serialize};

const NAME: &str = "Terminal Prefs";
const MAX_BYTES: usize = 4096;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Settings
{
    version:    u32,
    pub scheme: String
}
impl Default for Settings
{
    fn default() -> Self
    {
        Self { version: 1,
               scheme:  "traditional".into() }
    }
}
impl Settings
{
    pub fn selected(&self) -> Result<&'static crate::schemes::Scheme, Error>
    {
        if self.version != 1
        {
            return Err(Error::CorruptData);
        }
        crate::schemes::SCHEMES.iter()
                               .find(|scheme| scheme.id == self.scheme)
                               .ok_or(Error::CorruptData)
    }

    fn location(system: &System<'_>) -> Result<Location, Error>
    {
        let files = system.files();
        for index in 0..files.volume_count()?
        {
            let volume = files.volume_at(index)?;
            if volume.startup
            {
                let folder = files.lookup(volume.id, ROOT_ID, "System Folder/Preferences")?;
                return Ok(Location { volume:    volume.id,
                                     directory: folder.id });
            }
        }
        Err(Error::NotFound)
    }

    pub fn load(system: &System<'_>) -> Result<Self, Error>
    {
        let location = Self::location(system)?;
        let entry = match system.files().lookup(location.volume, location.directory, NAME)
        {
            Ok(entry) => entry,
            Err(Error::NotFound) => return Ok(Self::default()),
            Err(error) => return Err(error)
        };
        let mut file = system.files().open(location.volume, entry.id, Fork::Data)?;
        let length = usize::try_from(file.len()?).map_err(|_| Error::CorruptData)?;
        if length > MAX_BYTES
        {
            return Err(Error::CorruptData);
        }
        let mut bytes = [0u8; MAX_BYTES];
        let mut at = 0;
        while at < length
        {
            let count = file.read(&mut bytes[at..length])?;
            if count == 0
            {
                return Err(Error::CorruptData);
            }
            at += count;
        }
        if file.read(&mut [0u8; 1])? != 0
        {
            return Err(Error::ScanStale);
        }
        file.close()?;
        let settings: Self =
            serde_json::from_slice(&bytes[..length]).map_err(|_| Error::CorruptData)?;
        settings.selected()?;
        Ok(settings)
    }

    pub fn save(&self, system: &System<'_>) -> Result<(), Error>
    {
        self.selected()?;
        let location = Self::location(system)?;
        let replacing = match system.files().lookup(location.volume, location.directory, NAME)
        {
            Ok(entry) => Some(entry.id),
            Err(Error::NotFound) => None,
            Err(error) => return Err(error)
        };
        let bytes = serde_json::to_vec(self).map_err(|_| Error::NoMemory)?;
        if bytes.len() > MAX_BYTES
        {
            return Err(Error::NoMemory);
        }
        let mut writer =
            system.files().create_data(DataFileSpec { volume: location.volume,
                                                       parent: location.directory,
                                                       name: NAME,
                                                       replacing,
                                                       file_type: TypeCode::new(*b"JSON"),
                                                       creator: TypeCode::new(*b"TRML") },
                                        bytes.len())?;
        writer.write_all(&bytes)?;
        writer.finish()?;
        Ok(())
    }
}

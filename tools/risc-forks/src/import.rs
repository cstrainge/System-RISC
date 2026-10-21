use std::{io::Read,
          path::Path,
          time::{SystemTime, UNIX_EPOCH}};

use cap_std::fs::Dir;
use mac_fork_file::ReadAt;

use crate::{Entry, Kind, Metadata, Result, Volume, error,
            io::{FileIo, Slice},
            volume::Creation};
fn date(time: SystemTime) -> Result<u64>
{
    let (seconds, microseconds) = match time.duration_since(UNIX_EPOCH)
    {
        Ok(d) => (i64::try_from(d.as_secs())?, d.subsec_micros()),
        Err(error) =>
        {
            let d = error.duration();
            let seconds = i64::try_from(d.as_secs())?;
            if d.subsec_nanos() == 0
            {
                (-seconds, 0)
            }
            else
            {
                (-seconds.checked_add(1).ok_or_else(|| crate::error("source date overflow"))?,
                 1_000_000 - d.subsec_nanos().div_ceil(1000))
            }
        }
    };
    Ok(mac_hfs_b::time::Timestamp::from_unix(seconds, microseconds)?.0)
}
fn number(value: &serde_json::Value, field: &str) -> Result<u64>
{
    value[field].as_u64().ok_or_else(|| error(format!("missing/invalid {field}")))
}
fn template(value: &serde_json::Value) -> Result<Metadata>
{
    if value["format"] != "system-risc-forks" || value["version"] != 1
    {
        return Err(error("unsupported fork bundle; recreate/export with current tools"));
    }
    let kind = if value["directory"].as_bool().ok_or_else(|| error("missing directory flag"))?
    {
        Kind::Directory
    }
    else
    {
        Kind::File
    };
    let mut meta = Metadata::new(kind,
                                 16,
                                 [2; 16],
                                 value["name"].as_str().ok_or_else(|| error("missing name"))?,
                                 number(value, "created_mac_microseconds")?)?;
    meta.modified = number(value, "modified_mac_microseconds")?;
    meta.backup = if value["backup_mac_microseconds"].is_null()
    {
        None
    }
    else
    {
        Some(number(value, "backup_mac_microseconds")?)
    };
    meta.attributes = u16::try_from(number(value, "flags")?)?;
    let finder = value["finder_info"].as_array().ok_or_else(|| error("missing Finder info"))?;
    if finder.len() != 32
    {
        return Err(error("Finder info must contain32 bytes"));
    }
    for (out, value) in meta.finder_info.iter_mut().zip(finder)
    {
        *out = u8::try_from(value.as_u64().ok_or_else(|| error("invalid Finder info byte"))?)?;
    }
    meta.comment = value["comment"].as_str().ok_or_else(|| error("missing comment"))?.into();
    meta.validate()?;
    Ok(meta)
}
impl Volume
{
    pub fn import_macbinary(&self, parent: &str, source: &Path) -> Result<Entry>
    {
        let (directory, name) = source_parent(source)?;
        let source = regular(&directory, name)?;
        if source.metadata()?.len() > 64 * 1024 * 1024
        {
            return Err(error("MacBinary import budget exceeded"));
        }
        let mut bytes = Vec::new();
        source.take(64 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 64 * 1024 * 1024
        {
            return Err(error("MacBinary import budget exceeded"));
        }
        let file = mac_formats::macbinary::decode(&bytes)?;
        let mut meta = Metadata::new(Kind::File,
                                     16,
                                     [2; 16],
                                     &mac_formats::text::decode(&file.name),
                                     u64::from(file.created) * 1_000_000)?;
        meta.modified = u64::from(file.modified) * 1_000_000;
        meta.finder_info[..4].copy_from_slice(&file.file_type);
        meta.finder_info[4..8].copy_from_slice(&file.creator);
        meta.finder_info[8..10].copy_from_slice(&file.flags.to_be_bytes());
        self.create(parent,
                    &meta.name,
                    Kind::File,
                    &mut Slice(&file.data),
                    &mut Slice(&file.resources),
                    Some(&meta))
    }

    pub fn import(&self, parent: &str, source: &Path) -> Result<Entry>
    {
        let mut left = 100_000;
        let (directory, name) = source_parent(source)?;
        self.creation()?.import_at(parent, &directory, name, 0, &mut left)
    }
}

fn source_parent(source: &Path) -> Result<(Dir, &Path)>
{
    let name = Path::new(source.file_name().ok_or_else(|| error("source has no component"))?);
    let parent = source.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    Ok((Dir::open_ambient_dir(parent, cap_std::ambient_authority())?, name))
}

fn regular(directory: &Dir, path: &Path) -> Result<std::fs::File>
{
    if !directory.symlink_metadata(path)?.is_file()
    {
        return Err(error("source must be a regular file, not a link"));
    }
    let file = directory.open(path)?.into_std();
    if !file.metadata()?.is_file()
    {
        return Err(error("source must be a regular file"));
    }
    Ok(file)
}

fn folder(directory: &Dir, path: &Path) -> Result<Dir>
{
    if !directory.symlink_metadata(path)?.is_dir()
    {
        return Err(error("source must be a directory, not a link"));
    }
    Ok(directory.open_dir(path)?)
}

impl Creation<'_>
{
    fn import_at(&mut self, parent: &str, directory: &Dir, source: &Path, depth: usize,
                 left: &mut usize)
                 -> Result<Entry>
    {
        if depth > 128 || *left == 0
        {
            return Err(error("recursive import budget exceeded"));
        }
        *left -= 1;
        let info = directory.symlink_metadata(source)?;
        if info.file_type().is_symlink()
        {
            return Err(error("import does not follow symlinks"));
        }
        let source_dir = if info.is_dir() { Some(folder(directory, source)?) } else { None };
        let bundle =
            source_dir.as_ref().filter(|dir| dir.symlink_metadata("metadata.json").is_ok());
        if let Some(bundle) = bundle
        {
            let mut bytes = Vec::new();
            regular(bundle, Path::new("metadata.json"))?.take(16 * 1024 + 1)
                                                        .read_to_end(&mut bytes)?;
            if bytes.len() > 16 * 1024
            {
                return Err(error("fork metadata budget exceeded"));
            }
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            let meta = template(&value)?;
            let entry = if meta.kind == Kind::File
            {
                let mut data = FileIo::snapshot(regular(bundle, Path::new("data"))?)?;
                let mut resources = FileIo::snapshot(regular(bundle, Path::new("resources"))?)?;
                if data.len() != number(&value, "data_length")?
                   || resources.len() != number(&value, "resource_length")?
                {
                    return Err(error("fork bundle lengths do not match metadata"));
                }
                self.create(parent, &meta.name, Kind::File, &mut data, &mut resources, Some(&meta))?
            }
            else
            {
                self.create(parent,
                            &meta.name,
                            Kind::Directory,
                            &mut Slice(&[]),
                            &mut Slice(&[]),
                            Some(&meta))?
            };
            if meta.kind == Kind::Directory
            {
                let parent = format!("{}/{}", parent.trim_end_matches('/'), meta.name);
                let children = folder(bundle, Path::new("children"))?;
                for child in children.entries()?
                {
                    self.import_at(&parent,
                                   &children,
                                   Path::new(&child?.file_name()),
                                   depth + 1,
                                   left)?;
                }
            }
            return Ok(entry);
        }
        let name = source.file_name()
                         .and_then(|n| n.to_str())
                         .ok_or_else(|| error("source has no UTF-8 component"))?;
        let kind = if info.is_dir()
        {
            Kind::Directory
        }
        else if info.is_file()
        {
            Kind::File
        }
        else
        {
            return Err(error("source is not a regular file or folder"));
        };
        let modified = date(info.modified()?.into_std())?;
        let created = match info.created()
        {
            Ok(time) => date(time.into_std())?,
            Err(_) => modified
        };
        let mut meta = Metadata::new(kind, 16, [2; 16], name, created)?;
        meta.modified = modified;
        if kind == Kind::File
        {
            meta.finder_info[..4].copy_from_slice(b"BINA");
            let mut data = FileIo::snapshot(regular(directory, source)?)?;
            self.create(parent, name, kind, &mut data, &mut Slice(&[]), Some(&meta))
        }
        else
        {
            let entry =
                self.create(parent, name, kind, &mut Slice(&[]), &mut Slice(&[]), Some(&meta))?;
            let parent = format!("{}/{}", parent.trim_end_matches('/'), name);
            let children = source_dir.as_ref().ok_or_else(|| error("source folder disappeared"))?;
            for child in children.entries()?
            {
                self.import_at(&parent, children, Path::new(&child?.file_name()), depth + 1, left)?;
            }
            Ok(entry)
        }
    }
}

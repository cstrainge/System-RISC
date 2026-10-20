//! Recursive host interchange. Ordinary host files become data forks; exported
//! bundles preserve every fork and native metadata field. Copies are streaming,
//! not whole-tree transactions; errors leave completed entries available.
use std::{fs::{self, File},
          path::Path,
          time::{SystemTime, UNIX_EPOCH}};

use mac_hfs_b::{time::Timestamp, *};
use serde_json::{Value, json};

use crate::{HostResult, read_stream, write_stream};
const MAX_DEPTH: usize = 128;
const MAX_ENTRIES: usize = 100_000;
pub fn entry_json(e: &Entry) -> Value
{
    json!({"format":"system-risc-forks","version":1,"id":e.id,"parent":e.parent,"name":e.name,"directory":e.directory,"flags":e.flags,
        "children":e.children,"created_mac_microseconds":e.created.0,"modified_mac_microseconds":e.modified.0,
        "backup_mac_microseconds":e.backup.map(|t|t.0),"finder_info":e.finder_info.to_vec(),
        "data_length":e.data.length,"resource_length":e.resources.length})
}
pub fn resolve<D: BlockDevice>(v: &mut Volume<'_, D>, path: &str) -> HostResult<Entry>
{
    let mut e = v.entry(2)?;
    for name in path.split('/').filter(|s| !s.is_empty())
    {
        if name == "." || name == ".."
        {
            return Err("guest paths must not contain dot components".into());
        }
        e = v.lookup(e.id, name)?;
    }
    Ok(e)
}
fn visit(depth: usize, left: &mut usize) -> HostResult<()>
{
    if depth > MAX_DEPTH || *left == 0
    {
        return Err("recursive copy budget exceeded".into());
    }
    *left -= 1;
    Ok(())
}
fn export_entry<D: BlockDevice>(v: &mut Volume<'_, D>, id: u32, destination: &Path, depth: usize,
                                left: &mut usize)
                                -> HostResult<()>
{
    visit(depth, left)?;
    let e = v.entry(id)?;
    fs::create_dir(destination)?;
    let mut metadata = entry_json(&e);
    metadata["comment"] = json!(v.comment(id)?);
    if e.directory
    {
        fs::create_dir(destination.join("children"))?;
        let mut after = None;
        while let Some(child) = v.next_child(id, after.as_deref())?
        {
            export_entry(v,
                         child.id,
                         &destination.join("children").join(&child.name),
                         depth + 1,
                         left)?;
            after = Some(child.name);
        }
    }
    else
    {
        for (kind, name) in [(ForkKind::Data, "data"), (ForkKind::Resource, "resources")]
        {
            read_stream(v, id, kind, File::create(destination.join(name))?)?;
        }
    }
    // Written last: a failed extraction is visibly incomplete, never valid.
    fs::write(destination.join("metadata.json"), serde_json::to_vec_pretty(&metadata)?)?;
    Ok(())
}
/// Extract a file or directory as a lossless bundle. Destination must not exist.
pub fn copy_out<D: BlockDevice>(v: &mut Volume<'_, D>, id: u32, destination: &Path)
                                -> HostResult<()>
{
    let mut left = MAX_ENTRIES;
    export_entry(v, id, destination, 0, &mut left)
}
fn ordinary(path: &Path) -> HostResult<fs::Metadata>
{
    let m = fs::symlink_metadata(path)?;
    if !m.is_file() && !m.is_dir()
    {
        return Err("copy does not follow symlinks or special files".into());
    }
    Ok(m)
}
fn host_time(t: SystemTime) -> HostResult<Timestamp>
{
    let micros = match t.duration_since(UNIX_EPOCH)
    {
        Ok(d) => i128::try_from(d.as_micros())?,
        Err(e) => -i128::try_from(e.duration().as_micros())?
    };
    Ok(Timestamp::from_unix(i64::try_from(micros.div_euclid(1_000_000))?,
                            micros.rem_euclid(1_000_000) as u32)?)
}
fn number(m: &Value, key: &str) -> HostResult<u64>
{
    m[key].as_u64().ok_or_else(|| format!("missing or invalid {key}").into())
}
fn metadata(path: &Path) -> HostResult<Option<Value>>
{
    let p = path.join("metadata.json");
    let info = match fs::symlink_metadata(&p)
    {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into())
    };
    if !info.is_file() || info.len() > 16 * 1024
    {
        return Ok(None);
    }
    let m: Value = match serde_json::from_slice(&fs::read(p)?)
    {
        Ok(m) => m,
        Err(_) => return Ok(None)
    };
    if m["format"] != "system-risc-forks"
    {
        return Ok(None);
    }
    if m["version"] != 1
    {
        return Err("incompatible fork bundle; recreate export".into());
    }
    Ok(Some(m))
}
fn children(path: &Path, left: usize) -> HostResult<Vec<std::path::PathBuf>>
{
    if !ordinary(path)?.is_dir()
    {
        return Err("expected host directory".into());
    }
    let mut out = Vec::new();
    for item in fs::read_dir(path)?
    {
        if out.len() >= left
        {
            return Err("recursive copy entry budget exceeded".into());
        }
        out.push(item?.path());
    }
    out.sort();
    Ok(out)
}
fn target<D: WritableBlockDevice>(v: &mut Volume<'_, D>, parent: u32, name: &str, directory: bool,
                                  created: Timestamp, replace: bool)
                                  -> HostResult<Entry>
{
    match v.lookup(parent, name)
    {
        Ok(e) if e.directory && directory => Ok(e),
        Ok(e) if !e.directory && !directory && replace =>
        {
            v.remove(e.id, created)?;
            Ok(v.create(parent, name, false, created)?)
        }
        Ok(_) => Err("destination exists; use --replace to replace files".into()),
        Err(Error::NotFound) => Ok(v.create(parent, name, directory, created)?),
        Err(e) => Err(e.into())
    }
}
fn import_entry<D: WritableBlockDevice>(v: &mut Volume<'_, D>, parent: u32, source: &Path,
                                        replace: bool, depth: usize, left: &mut usize)
                                        -> HostResult<Entry>
{
    visit(depth, left)?;
    let info = ordinary(source)?;
    if info.is_dir()
       && let Some(m) = metadata(source)?
    {
        let directory = m["directory"].as_bool().ok_or("missing entry kind")?;
        let name = m["name"].as_str().ok_or("missing filename")?;
        let created = Timestamp(number(&m, "created_mac_microseconds")?);
        let modified = Timestamp(number(&m, "modified_mac_microseconds")?);
        let backup = if m["backup_mac_microseconds"].is_null()
        {
            None
        }
        else
        {
            Some(Timestamp(number(&m, "backup_mac_microseconds")?))
        };
        let flags = u16::try_from(number(&m, "flags")?)?;
        let raw = m["finder_info"].as_array().ok_or("missing Finder metadata")?;
        if raw.len() != 32
        {
            return Err("invalid Finder metadata size".into());
        }
        let mut finder_info = [0; 32];
        for (out, n) in finder_info.iter_mut().zip(raw)
        {
            *out = u8::try_from(n.as_u64().ok_or("invalid Finder metadata")?)?;
        }
        let comment = m["comment"].as_str().ok_or("missing comment")?;
        let entries =
            if directory { children(&source.join("children"), *left)? } else { Vec::new() };
        // Validate fork files before creating or replacing the destination.
        if !directory
        {
            for (file, key) in [("data", "data_length"), ("resources", "resource_length")]
            {
                let f = ordinary(&source.join(file))?;
                if !f.is_file() || f.len() != number(&m, key)?
                {
                    return Err("exported fork length mismatch".into());
                }
            }
        }
        let e = target(v, parent, name, directory, created, replace)?;
        if directory
        {
            for path in entries
            {
                import_entry(v, e.id, &path, replace, depth + 1, left)?;
            }
        }
        else
        {
            for (kind, file) in [(ForkKind::Data, "data"), (ForkKind::Resource, "resources")]
            {
                write_stream(v, e.id, kind, File::open(source.join(file))?, modified)?;
            }
        }
        v.set_comment(e.id, comment, modified)?;
        return Ok(v.set_metadata(e.id, flags, finder_info, created, modified, backup)?);
    }
    let name = source.file_name().and_then(|n| n.to_str()).ok_or("host filename must be UTF-8")?;
    let modified = host_time(info.modified()?)?;
    let created = host_time(info.created().unwrap_or(info.modified()?))?;
    let entries = if info.is_dir() { children(source, *left)? } else { Vec::new() };
    let e = target(v, parent, name, info.is_dir(), created, replace)?;
    if info.is_dir()
    {
        for path in entries
        {
            import_entry(v, e.id, &path, replace, depth + 1, left)?;
        }
    }
    else
    {
        write_stream(v, e.id, ForkKind::Data, File::open(source)?, modified)?;
    }
    Ok(v.set_metadata(e.id, e.flags, e.finder_info, created, modified, e.backup)?)
}
/// Copy an ordinary host tree or an extracted lossless bundle into a directory.
/// Existing directories merge; replacing existing files requires explicit opt-in.
pub fn copy_in<D: WritableBlockDevice>(v: &mut Volume<'_, D>, parent: u32, source: &Path,
                                       replace: bool)
                                       -> HostResult<Entry>
{
    let mut left = MAX_ENTRIES;
    import_entry(v, parent, source, replace, 0, &mut left)
}

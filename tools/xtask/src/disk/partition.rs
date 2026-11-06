//! Partitioned boot media: an MBR, a FAT32 boot partition with the declared
//! files, and a System7 partition built by the shared volume path.

use std::{fs::{self, File},
          io::{Read, Seek, SeekFrom, Write},
          path::Path};

use mac_build::{Result, error, package,
                platform::storage::{Entry, Partition, Source, Table, TableKind}};
use serde_json::{Value, json};

use super::build_system7;
use crate::{Workspace, crates::Installed};

const SECTOR: u64 = 512;
const ALIGN: u64 = 2048 * SECTOR; // 1 MiB
const CLUSTER_SECTORS: u64 = 1; // 512-byte clusters: a 256 MiB FAT32 partition needs >65525 clusters
const RESERVED_SECTORS: u64 = 32;
const FAT_COUNT: u64 = 2;

struct Placement<'a>
{
    partition: &'a Partition,
    offset:    u64,
    size:      u64
}

pub(super) fn build(workspace: &Workspace, path: &Path, total: u64, table: &Table,
                    partitions: &[Partition], installed: &[Installed])
                    -> Result<Value>
{
    match table.kind
    {
        TableKind::Mbr =>
        {}
        TableKind::Gpt =>
        {
            return Err(error("partitioned media writer implements the MBR table; this record \
                              declares gpt"));
        }
    }
    if partitions.len() > 4
    {
        return Err(error("an MBR holds at most four primary partitions"));
    }
    let placements = plan(total, partitions)?;
    let mut file = File::create(path)?;
    file.set_len(total)?;
    write_mbr(&mut file, &placements, table.boot)?;
    let mut report = Vec::new();
    for placement in &placements
    {
        let label = placement.partition.label();
        match placement.partition
        {
            Partition::Fat32 { files,
                               .. } =>
            {
                let files = resolve_files(workspace, files)?;
                let written = write_fat32(&mut file,
                                          placement.offset,
                                          placement.size,
                                          placement.offset / SECTOR,
                                          &files)?;
                report.push(json!({"label": label,
                                   "kind": "Fat32",
                                   "offset": placement.offset,
                                   "size": placement.size,
                                   "files": written}));
            }
            Partition::System7Filesystem { .. } =>
            {
                let scratch = path.with_extension(format!("{label}.hfsb"));
                let volume = build_system7(workspace, &scratch, placement.size, installed)?;
                copy_into(&mut file, &scratch, placement.offset)?;
                fs::remove_file(&scratch)?;
                report.push(json!({"label": label,
                                   "kind": "System7Filesystem",
                                   "offset": placement.offset,
                                   "size": placement.size,
                                   "system7": volume}));
            }
            Partition::Raw { offset,
                             payload,
                             .. } =>
            {
                let offset = mac_build::platform::storage::quantity(offset)?;
                let bytes = resolve_source(workspace, payload)?;
                if offset.checked_add(bytes.len() as u64).is_none_or(|end| end > placement.size)
                {
                    return Err(error(format!("partition '{label}' payload does not fit inside \
                                              its {} byte partition",
                                             placement.size)));
                }
                file.seek(SeekFrom::Start(placement.offset + offset))?;
                file.write_all(&bytes)?;
                report.push(json!({"label": label,
                                   "kind": "Raw",
                                   "offset": placement.offset,
                                   "size": placement.size,
                                   "payload_bytes": bytes.len()}));
            }
        }
    }
    file.flush()?;
    Ok(json!({"table": {"kind": format!("{:?}", table.kind), "boot": table.boot},
              "partitions": report}))
}

fn plan(total: u64, partitions: &[Partition]) -> Result<Vec<Placement<'_>>>
{
    let mut cursor = ALIGN;
    let mut placements = Vec::new();
    for (index, partition) in partitions.iter().enumerate()
    {
        let size = match partition.size().bytes()?
        {
            Some(bytes) =>
            {
                bytes.div_ceil(SECTOR)
                     .checked_mul(SECTOR)
                     .ok_or_else(|| error("partition size overflows"))?
            }
            None =>
            {
                if index + 1 != partitions.len()
                {
                    return Err(error("only the last partition may consume the remaining space"));
                }
                total.checked_sub(cursor)
                     .ok_or_else(|| error("the fixed partitions consume the whole medium"))?
            }
        };
        if size == 0
        {
            return Err(error(format!("partition '{}' has no space", partition.label())));
        }
        if cursor.checked_add(size).is_none_or(|end| end > total)
        {
            return Err(error(format!("partition '{}' does not fit in the {total} byte medium",
                                     partition.label())));
        }
        placements.push(Placement { partition,
                                    offset: cursor,
                                    size });
        cursor = cursor.checked_add(size).ok_or_else(|| error("partition offset overflows"))?;
        cursor = cursor.div_ceil(ALIGN) * ALIGN;
    }
    Ok(placements)
}

fn write_mbr(file: &mut File, placements: &[Placement<'_>], boot: Option<usize>) -> Result<()>
{
    if let Some(index) = boot
    {
        if index >= placements.len()
        {
            return Err(error(format!("table.boot names partition {index}, but the medium has \
                                      {} partitions",
                                     placements.len())));
        }
    }
    let mut sector = [0u8; 512];
    sector[440..444].copy_from_slice(&0x53525344u32.to_le_bytes());
    for (index, placement) in placements.iter().enumerate()
    {
        let lba = u32::try_from(placement.offset / SECTOR).map_err(|_| {
                                                              error(format!("partition '{}' \
                                                                             starts beyond the \
                                                                             32-bit LBA range",
                                                                            placement.partition
                                                                                     .label()))
                                                          })?;
        let sectors =
            u32::try_from(placement.size / SECTOR).map_err(|_| {
                                                      error(format!("partition '{}' is larger \
                                                                     than the 32-bit sector count",
                                                                    placement.partition.label()))
                                                  })?;
        let entry = 446 + index * 16;
        sector[entry] = if boot == Some(index) { 0x80 } else { 0x00 };
        sector[entry + 1..entry + 4].copy_from_slice(&[0xfe, 0xff, 0xff]);
        sector[entry + 4] = match placement.partition
        {
            Partition::Fat32 { .. } => 0x0c,
            Partition::System7Filesystem { .. } => 0xaf,
            Partition::Raw { .. } => 0x83
        };
        sector[entry + 5..entry + 8].copy_from_slice(&[0xfe, 0xff, 0xff]);
        sector[entry + 8..entry + 12].copy_from_slice(&lba.to_le_bytes());
        sector[entry + 12..entry + 16].copy_from_slice(&sectors.to_le_bytes());
    }
    sector[510] = 0x55;
    sector[511] = 0xaa;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&sector)?;
    Ok(())
}

struct FatFile
{
    path:  String,
    bytes: Vec<u8>
}

fn resolve_files(workspace: &Workspace, files: &[Entry]) -> Result<Vec<FatFile>>
{
    let mut resolved = Vec::new();
    for entry in files
    {
        resolved.push(FatFile { path:  entry.path.clone(),
                                bytes: resolve_source(workspace, &entry.source)? });
    }
    Ok(resolved)
}

fn resolve_source(workspace: &Workspace, source: &Source) -> Result<Vec<u8>>
{
    match source
    {
        Source::Artifact { artifact } =>
        {
            let path = workspace.dist().join(format!("{artifact}.bin"));
            fs::read(&path).map_err(|why| {
                               error(format!("cannot read artifact {}: {why}", path.display()))
                           })
        }
        Source::File { file } =>
        {
            let path = workspace.root().join(file);
            fs::read(&path).map_err(|why| {
                               error(format!("cannot read file {}: {why}", path.display()))
                           })
        }
        Source::BoardConfig { lines } =>
        {
            let mut text = String::new();
            for line in lines
            {
                text.push_str(&line.key);
                text.push('=');
                text.push_str(&line.value);
                text.push('\n');
            }
            Ok(text.into_bytes())
        }
    }
}

fn copy_into(file: &mut File, source: &Path, offset: u64) -> Result<()>
{
    let mut input = File::open(source)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut buffer = vec![0u8; 1024 * 1024];
    loop
    {
        let count = input.read(&mut buffer)?;
        if count == 0
        {
            break;
        }
        file.write_all(&buffer[..count])?;
    }
    Ok(())
}

struct Dir
{
    cluster: u32,
    entries: Vec<DirEntry>
}

struct DirEntry
{
    name:          String,
    node:          Node,
    first_cluster: u32
}

enum Node
{
    File(Vec<u8>),
    Dir(Dir)
}

struct WrittenFile
{
    path:          String,
    bytes:         Vec<u8>,
    first_cluster: u32
}

struct Geometry
{
    total_sectors: u32,
    fat_size:      u32,
    first_data:    u64,
    clusters:      u64,
    cluster_bytes: u64
}

fn geometry(size: u64) -> Result<Geometry>
{
    let total_sectors = u32::try_from(size / SECTOR).map_err(|_| {
                                                        error("FAT32 partition exceeds the 32-bit \
                                                               sector count")
                                                    })?;
    let total = u64::from(total_sectors);
    let mut fat_size = 1u64;
    loop
    {
        let data = total.checked_sub(RESERVED_SECTORS + FAT_COUNT * fat_size)
                        .ok_or_else(|| error("FAT32 partition is too small"))?;
        let clusters = data / CLUSTER_SECTORS;
        let needed = ((clusters + 2) * 4).div_ceil(SECTOR);
        if needed <= fat_size
        {
            break;
        }
        fat_size = needed;
    }
    let data_sectors = total - RESERVED_SECTORS - FAT_COUNT * fat_size;
    let clusters = data_sectors / CLUSTER_SECTORS;
    if clusters < 65_525
    {
        return Err(error(format!("FAT32 partition has {clusters} clusters; FAT32 needs at \
                                  least 65525")));
    }
    Ok(Geometry { total_sectors,
                  fat_size: u32::try_from(fat_size).map_err(|_| {
                                                       error("FAT32 table exceeds the 32-bit \
                                                              sector count")
                                                   })?,
                  first_data: RESERVED_SECTORS + FAT_COUNT * fat_size,
                  clusters,
                  cluster_bytes: CLUSTER_SECTORS * SECTOR })
}

fn write_at(file: &mut File, base: u64, relative: u64, bytes: &[u8]) -> Result<()>
{
    file.seek(SeekFrom::Start(base.checked_add(relative)
                                  .ok_or_else(|| error("FAT32 write offset overflows"))?))?;
    file.write_all(bytes)?;
    Ok(())
}

fn write_file(file: &mut File, base: u64, first_data: u64, written: &WrittenFile, fat: &[u32])
              -> Result<()>
{
    let mut cluster = written.first_cluster;
    let mut done = 0usize;
    while done < written.bytes.len()
    {
        let offset = (first_data + (u64::from(cluster) - 2) * CLUSTER_SECTORS) * SECTOR;
        let take = (written.bytes.len() - done).min((CLUSTER_SECTORS * SECTOR) as usize);
        write_at(file, base, offset, &written.bytes[done..done + take])?;
        done += take;
        if done < written.bytes.len()
        {
            let next = fat[cluster as usize] & 0x0fff_ffff;
            if next >= 0x0fff_fff8
            {
                return Err(error(format!("FAT32 cluster chain for '{}' ends early",
                                         written.path)));
            }
            cluster = next;
        }
    }
    Ok(())
}

fn write_fat32(file: &mut File, base: u64, size: u64, hidden: u64, files: &[FatFile])
               -> Result<Value>
{
    let geo = geometry(size)?;
    let hidden = u32::try_from(hidden).map_err(|_| {
                                          error("FAT32 partition start exceeds the 32-bit \
                                                 hidden-sector field")
                                      })?;
    let mut fat = vec![0u32; (geo.clusters + 2) as usize];
    fat[0] = 0x0fff_fff8;
    fat[1] = 0x0fff_ffff;
    fat[2] = 0x0fff_ffff; // root directory
    let mut next = 3u32;
    let mut root = Dir { cluster: 2,
                         entries: Vec::new() };
    for file in files
    {
        insert(&mut root, &file.path, file.bytes.clone())?;
    }
    let mut written = Vec::new();
    allocate_dir(&mut root, "", &mut fat, &mut next, geo.clusters, geo.cluster_bytes,
                 &mut written)?;
    let boot = boot_sector(geo.fat_size, geo.total_sectors, hidden);
    write_at(file, base, 0, &boot)?;
    write_at(file, base, 6 * 512, &boot)?;
    let fsinfo = fsinfo_sector(geo.clusters - (u64::from(next) - 2), next);
    write_at(file, base, 512, &fsinfo)?;
    write_at(file, base, 7 * 512, &fsinfo)?;
    let mut fat_bytes = Vec::with_capacity(geo.fat_size as usize * 512);
    for entry in &fat
    {
        fat_bytes.extend_from_slice(&entry.to_le_bytes());
    }
    fat_bytes.resize(geo.fat_size as usize * 512, 0);
    for index in 0..FAT_COUNT
    {
        write_at(file,
                 base,
                 (RESERVED_SECTORS + index * u64::from(geo.fat_size)) * SECTOR,
                 &fat_bytes)?;
    }
    write_dir(file, base, &root, geo.first_data, geo.cluster_bytes, true, 0)?;
    for entry in &written
    {
        write_file(file, base, geo.first_data, entry, &fat)?;
    }
    Ok(json!(written.iter()
                    .map(|entry| {
                        json!({
                            "path": entry.path,
                            "bytes": entry.bytes.len(),
                            "first_cluster": entry.first_cluster,
                            "sha256": package::sha256(&entry.bytes)
                        })
                    })
                    .collect::<Vec<_>>()))
}
fn insert(dir: &mut Dir, path: &str, data: Vec<u8>) -> Result<()>
{
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    if parts.is_empty()
    {
        return Err(error("FAT32 entry has an empty path"));
    }
    let name = parts[0];
    if parts.len() == 1
    {
        if dir.entries.iter().any(|entry| entry.name == name)
        {
            return Err(error(format!("FAT32 path '{path}' is listed twice")));
        }
        dir.entries.push(DirEntry { name:          name.to_string(),
                                    node:          Node::File(data),
                                    first_cluster: 0 });
        return Ok(());
    }
    let index = match dir.entries.iter().position(|entry| entry.name == name)
    {
        Some(index) =>
        {
            if !matches!(dir.entries[index].node, Node::Dir(_))
            {
                return Err(error(format!("FAT32 path '{path}' names a file as a directory")));
            }
            index
        }
        None =>
        {
            dir.entries.push(DirEntry { name:          name.to_string(),
                                        node:          Node::Dir(Dir { cluster: 0,
                                                                       entries: Vec::new() }),
                                        first_cluster: 0 });
            dir.entries.len() - 1
        }
    };
    let rest = parts[1..].join("/");
    match &mut dir.entries[index].node
    {
        Node::Dir(child) => insert(child, &rest, data),
        Node::File(_) => Err(error(format!("FAT32 path '{path}' names a file as a directory")))
    }
}

fn allocate(fat: &mut [u32], next: &mut u32, clusters: u64, count: u64) -> Result<u32>
{
    if u64::from(*next) + count > clusters + 2
    {
        return Err(error("FAT32 partition is full"));
    }
    let first = *next;
    for index in 0..count
    {
        let cluster = *next;
        *next += 1;
        fat[cluster as usize] = if index + 1 == count { 0x0fff_ffff } else { cluster + 1 };
    }
    Ok(first)
}

fn allocate_dir(dir: &mut Dir, prefix: &str, fat: &mut [u32], next: &mut u32, clusters: u64,
                cluster_bytes: u64, written: &mut Vec<WrittenFile>)
                -> Result<()>
{
    if dir.cluster == 0
    {
        dir.cluster = allocate(fat, next, clusters, 1)?;
    }
    for entry in &mut dir.entries
    {
        let path =
            if prefix.is_empty() { entry.name.clone() } else { format!("{prefix}/{}", entry.name) };
        match &mut entry.node
        {
            Node::File(bytes) =>
            {
                let count = (bytes.len() as u64).div_ceil(cluster_bytes).max(1);
                let first = allocate(fat, next, clusters, count)?;
                entry.first_cluster = first;
                written.push(WrittenFile { path,
                                           bytes: bytes.clone(),
                                           first_cluster: first });
            }
            Node::Dir(child) =>
            {
                let cluster = allocate(fat, next, clusters, 1)?;
                child.cluster = cluster;
                entry.first_cluster = cluster;
                allocate_dir(child, &path, fat, next, clusters, cluster_bytes, written)?;
            }
        }
    }
    Ok(())
}

fn write_dir(file: &mut File, base: u64, dir: &Dir, first_data: u64, cluster_bytes: u64,
             root: bool, parent: u32)
             -> Result<()>
{
    let offset = (first_data + (u64::from(dir.cluster) - 2) * CLUSTER_SECTORS) * SECTOR;
    // One 512-byte cluster holds 16 directory entries. A larger directory is
    // a named error rather than a silent overwrite of the following data.
    let mut buffer = vec![0u8; cluster_bytes as usize];
    let mut cursor = 0usize;
    if !root
    {
        write_entry(&mut buffer[cursor..cursor + 32], *b".          ", 0x10, dir.cluster, 0);
        cursor += 32;
        write_entry(&mut buffer[cursor..cursor + 32], *b"..         ", 0x10, parent, 0);
        cursor += 32;
    }
    for entry in &dir.entries
    {
        if cursor + 32 > buffer.len()
        {
            return Err(error(format!("FAT32 directory cluster {} exceeds its 16 entries; \
                                      directory-chain support is required",
                                     dir.cluster)));
        }
        let (attr, size) = match &entry.node
        {
            Node::File(bytes) => (0x20, bytes.len() as u32),
            Node::Dir(_) => (0x10, 0)
        };
        write_entry(&mut buffer[cursor..cursor + 32],
                    name83(&entry.name)?,
                    attr,
                    entry.first_cluster,
                    size);
        cursor += 32;
    }
    write_at(file, base, offset, &buffer)?;
    for entry in &dir.entries
    {
        if let Node::Dir(child) = &entry.node
        {
            write_dir(file, base, child, first_data, cluster_bytes, false, dir.cluster)?;
        }
    }
    Ok(())
}
fn write_entry(slot: &mut [u8], name: [u8; 11], attr: u8, first_cluster: u32, size: u32)
{
    slot[..11].copy_from_slice(&name);
    slot[11] = attr;
    let date: u16 = ((2026 - 1980) << 9) | (11 << 5) | 4;
    slot[14..16].copy_from_slice(&0u16.to_le_bytes());
    slot[16..18].copy_from_slice(&date.to_le_bytes());
    slot[18..20].copy_from_slice(&date.to_le_bytes());
    slot[20..22].copy_from_slice(&((first_cluster >> 16) as u16).to_le_bytes());
    slot[22..24].copy_from_slice(&0u16.to_le_bytes());
    slot[24..26].copy_from_slice(&date.to_le_bytes());
    slot[26..28].copy_from_slice(&(first_cluster as u16).to_le_bytes());
    slot[28..32].copy_from_slice(&size.to_le_bytes());
}

fn name83(name: &str) -> Result<[u8; 11]>
{
    let upper = name.to_ascii_uppercase();
    let (base, ext) = match upper.rsplit_once('.')
    {
        Some((base, ext)) => (base, ext),
        None => (upper.as_str(), "")
    };
    if base.is_empty() || base.len() > 8 || ext.len() > 3
    {
        return Err(error(format!("FAT32 name '{name}' does not fit 8.3")));
    }
    for character in base.chars().chain(ext.chars())
    {
        if !character.is_ascii_alphanumeric() && !"-_.$~".contains(character)
        {
            return Err(error(format!("FAT32 name '{name}' contains unsupported character \
                                      '{character}'")));
        }
    }
    let mut out = [b' '; 11];
    out[..base.len()].copy_from_slice(base.as_bytes());
    out[8..8 + ext.len()].copy_from_slice(ext.as_bytes());
    Ok(out)
}

fn boot_sector(fat_size: u32, total_sectors: u32, hidden: u32) -> [u8; 512]
{
    let mut sector = [0u8; 512];
    sector[0..3].copy_from_slice(&[0xeb, 0x58, 0x90]);
    sector[3..11].copy_from_slice(b"MSWIN4.1");
    sector[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes());
    sector[13] = CLUSTER_SECTORS as u8;
    sector[14..16].copy_from_slice(&(RESERVED_SECTORS as u16).to_le_bytes());
    sector[16] = FAT_COUNT as u8;
    sector[17..19].copy_from_slice(&0u16.to_le_bytes());
    sector[19..21].copy_from_slice(&0u16.to_le_bytes());
    sector[21] = 0xf8;
    sector[22..24].copy_from_slice(&0u16.to_le_bytes());
    sector[24..26].copy_from_slice(&63u16.to_le_bytes());
    sector[26..28].copy_from_slice(&255u16.to_le_bytes());
    sector[28..32].copy_from_slice(&hidden.to_le_bytes());
    sector[32..36].copy_from_slice(&total_sectors.to_le_bytes());
    sector[36..40].copy_from_slice(&fat_size.to_le_bytes());
    sector[40..42].copy_from_slice(&0u16.to_le_bytes());
    sector[42..44].copy_from_slice(&0u16.to_le_bytes());
    sector[44..48].copy_from_slice(&2u32.to_le_bytes());
    sector[48..50].copy_from_slice(&1u16.to_le_bytes());
    sector[50..52].copy_from_slice(&6u16.to_le_bytes());
    sector[64] = 0x80;
    sector[66] = 0x29;
    sector[67..71].copy_from_slice(&0x53525344u32.to_le_bytes());
    sector[71..82].copy_from_slice(b"BOOT       ");
    sector[82..90].copy_from_slice(b"FAT32   ");
    sector[510] = 0x55;
    sector[511] = 0xaa;
    sector
}

fn fsinfo_sector(free: u64, next: u32) -> [u8; 512]
{
    let mut sector = [0u8; 512];
    sector[0..4].copy_from_slice(&0x41615252u32.to_le_bytes());
    sector[484..488].copy_from_slice(&0x61417272u32.to_le_bytes());
    sector[488..492].copy_from_slice(&(free as u32).to_le_bytes());
    sector[492..496].copy_from_slice(&next.to_le_bytes());
    sector[508..512].copy_from_slice(&0xaa550000u32.to_le_bytes());
    sector
}

#[cfg(test)]
mod tests
{
    use std::{io::Read, path::PathBuf};

    use mac_build::platform::storage::SizeRule;

    use super::*;

    struct Cleanup(PathBuf);
    impl Drop for Cleanup
    {
        fn drop(&mut self)
        {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir() -> (PathBuf, Cleanup)
    {
        let path = std::env::temp_dir().join(format!("sr-partition-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        (path.clone(), Cleanup(path))
    }

    fn read_u16(bytes: &[u8], offset: usize) -> u16
    {
        u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
    }

    fn read_u32(bytes: &[u8], offset: usize) -> u32
    {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    fn entries(bytes: &[u8], offset: usize) -> Vec<(String, u8, u32, u32)>
    {
        let mut found = Vec::new();
        for index in 0..16
        {
            let at = offset + index * 32;
            let entry = &bytes[at..at + 32];
            if entry[0] == 0
            {
                break;
            }
            if entry[0] == 0xe5
            {
                continue;
            }
            let base = String::from_utf8_lossy(&entry[0..8]).trim_end().to_string();
            let ext = String::from_utf8_lossy(&entry[8..11]).trim_end().to_string();
            let name = if ext.is_empty() { base } else { format!("{base}.{ext}") };
            let first = (u32::from(read_u16(entry, 20)) << 16) | u32::from(read_u16(entry, 26));
            let size = read_u32(entry, 28);
            found.push((name, entry[11], first, size));
        }
        found
    }

    fn find(bytes: &[u8], dir: &[(String, u8, u32, u32)], path: &str, geo: &Geometry)
            -> Option<(String, u8, u32, u32)>
    {
        let (first, rest) = path.split_once('/').unwrap_or((path, ""));
        let wanted = first.to_ascii_uppercase();
        let entry = dir.iter().find(|(name, _, _, _)| *name == wanted)?.clone();
        if rest.is_empty()
        {
            return Some(entry);
        }
        let offset = (geo.first_data + (u64::from(entry.2) - 2) * CLUSTER_SECTORS) * SECTOR;
        let child = entries(bytes, offset as usize);
        find(bytes, &child, rest, geo)
    }

    fn read_chain(bytes: &[u8], fat_offset: usize, first_data: usize, first: u32, size: u32,
                  cluster_bytes: usize)
                  -> Vec<u8>
    {
        let mut out = Vec::new();
        let mut cluster = first;
        while out.len() < size as usize
        {
            let offset = first_data + (cluster as usize - 2) * cluster_bytes;
            let take = (size as usize - out.len()).min(cluster_bytes);
            out.extend_from_slice(&bytes[offset..offset + take]);
            if out.len() >= size as usize
            {
                break;
            }
            let next = read_u32(bytes, fat_offset + cluster as usize * 4) & 0x0fff_ffff;
            assert!(next < 0x0fff_fff8, "FAT chain ended before the file size");
            cluster = next;
        }
        out
    }

    #[test]
    fn fat32_roundtrip_and_mbr_partition_table()
    {
        let (dir, _cleanup) = temp_dir();
        let path = dir.join("fat.img");
        let size = 64 * 1024 * 1024u64;
        let inputs = vec![FatFile { path:  "config.txt".into(),
                                    bytes: b"arm_64bit=1\nkernel=kernel8.img\n".to_vec() },
                          FatFile { path:  "kernel8.img".into(),
                                    bytes: (0..5000).map(|index| (index % 251) as u8).collect() },
                          FatFile { path:  "sub/note.bin".into(),
                                    bytes: b"nested payload".to_vec() }];
        let mut file = File::create(&path).unwrap();
        file.set_len(size).unwrap();
        let report = write_fat32(&mut file, 0, size, 0, &inputs).unwrap();
        drop(file);
        let bytes = fs::read(&path).unwrap();
        assert_eq!(&bytes[510..512], &[0x55, 0xaa]);
        let geo = geometry(size).unwrap();
        assert_eq!(u64::from(read_u32(&bytes, 32)), size / SECTOR);
        assert_eq!(read_u32(&bytes, 36), geo.fat_size);
        assert_eq!(read_u16(&bytes, 14), RESERVED_SECTORS as u16);
        assert_eq!(bytes[16], FAT_COUNT as u8);
        let fat1 = RESERVED_SECTORS as usize * 512;
        let fat2 = fat1 + geo.fat_size as usize * 512;
        assert_eq!(&bytes[fat1..fat1 + geo.fat_size as usize * 512],
                   &bytes[fat2..fat2 + geo.fat_size as usize * 512],
                   "the two FAT copies must be identical");
        let root = entries(&bytes, geo.first_data as usize * 512);
        let names: Vec<&str> = root.iter().map(|(name, _, _, _)| name.as_str()).collect();
        assert!(names.contains(&"CONFIG.TXT"));
        assert!(names.contains(&"KERNEL8.IMG"));
        assert!(names.contains(&"SUB"));
        let report: Vec<Value> = serde_json::from_value(report).unwrap();
        for input in &inputs
        {
            let item = report.iter()
                             .find(|item| item["path"].as_str() == Some(input.path.as_str()))
                             .unwrap();
            let first = item["first_cluster"].as_u64().unwrap() as u32;
            assert_eq!(item["sha256"].as_str().unwrap(), package::sha256(&input.bytes));
            let entry = find(&bytes, &root, &input.path, &geo).unwrap();
            assert_eq!(entry.2, first, "directory first cluster for {}", input.path);
            let data = read_chain(&bytes,
                                  fat1,
                                  (geo.first_data * SECTOR) as usize,
                                  first,
                                  input.bytes.len() as u32,
                                  geo.cluster_bytes as usize);
            assert_eq!(data, input.bytes, "file bytes for {}", input.path);
        }
        let mbr_path = dir.join("mbr.img");
        let mut mbr = fs::OpenOptions::new().read(true)
                                            .write(true)
                                            .create(true)
                                            .truncate(true)
                                            .open(&mbr_path)
                                            .unwrap();
        mbr.set_len(4096).unwrap();
        let fat = Partition::Fat32 { label: "BOOT".into(),
                                     size:  SizeRule::Bytes { bytes: "0x100000".into() },
                                     files: Vec::new() };
        let system = Partition::System7Filesystem { label: "SYSTEM".into(),
                                                    size:  SizeRule::Remaining };
        let placements = vec![Placement { partition: &fat,
                                          offset:    2048 * SECTOR,
                                          size:      100 * SECTOR },
                              Placement { partition: &system,
                                          offset:    4096 * SECTOR,
                                          size:      200 * SECTOR }];
        write_mbr(&mut mbr, &placements, Some(0)).unwrap();
        mbr.seek(SeekFrom::Start(0)).unwrap();
        let mut sector = [0u8; 512];
        mbr.read_exact(&mut sector).unwrap();
        assert_eq!(&sector[510..512], &[0x55, 0xaa]);
        assert_eq!(sector[446], 0x80);
        assert_eq!(sector[446 + 4], 0x0c);
        assert_eq!(read_u32(&sector, 446 + 8), 2048);
        assert_eq!(read_u32(&sector, 446 + 12), 100);
        assert_eq!(sector[446 + 16], 0x00);
        assert_eq!(sector[446 + 16 + 4], 0xaf);
        assert_eq!(read_u32(&sector, 446 + 16 + 8), 4096);
        assert_eq!(read_u32(&sector, 446 + 16 + 12), 200);
        assert!(write_mbr(&mut mbr, &placements, Some(9)).is_err());
        let beyond = vec![Placement { partition: &fat,
                                      offset:    (u64::from(u32::MAX) + 1) * SECTOR,
                                      size:      SECTOR }];
        assert!(write_mbr(&mut mbr, &beyond, Some(0)).is_err());
    }
}

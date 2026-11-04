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
                let scratch = path.with_extension(format!("{label}.fat32"));
                let written =
                    write_fat32(&scratch, placement.size, placement.offset / SECTOR, &files)?;
                copy_into(&mut file, &scratch, placement.offset)?;
                fs::remove_file(&scratch)?;
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
    let mut sector = [0u8; 512];
    sector[440..444].copy_from_slice(&0x53525344u32.to_le_bytes());
    for (index, placement) in placements.iter().enumerate()
    {
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
        sector[entry + 8..entry + 12]
            .copy_from_slice(&((placement.offset / SECTOR) as u32).to_le_bytes());
        sector[entry + 12..entry + 16]
            .copy_from_slice(&((placement.size / SECTOR) as u32).to_le_bytes());
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

fn write_fat32(path: &Path, size: u64, hidden: u64, files: &[FatFile]) -> Result<Value>
{
    let total_sectors = size / SECTOR;
    let mut fat_size = 1u64;
    loop
    {
        let data = total_sectors.checked_sub(RESERVED_SECTORS + FAT_COUNT * fat_size)
                                .ok_or_else(|| error("FAT32 partition is too small"))?;
        let clusters = data / CLUSTER_SECTORS;
        let needed = ((clusters + 2) * 4).div_ceil(SECTOR);
        if needed <= fat_size
        {
            break;
        }
        fat_size = needed;
    }
    let data_sectors = total_sectors - RESERVED_SECTORS - FAT_COUNT * fat_size;
    let clusters = data_sectors / CLUSTER_SECTORS;
    if clusters < 65_525
    {
        return Err(error(format!("FAT32 partition has {clusters} clusters; FAT32 needs at \
                                  least 65525")));
    }
    let first_data = RESERVED_SECTORS + FAT_COUNT * fat_size;
    let cluster_bytes = CLUSTER_SECTORS * SECTOR;
    let mut fat = vec![0u32; (clusters + 2) as usize];
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
    allocate_dir(&mut root, "", &mut fat, &mut next, clusters, cluster_bytes, &mut written)?;
    let mut image = vec![0u8; size as usize];
    let boot = boot_sector(fat_size as u32, total_sectors as u32, hidden as u32);
    image[0..512].copy_from_slice(&boot);
    image[6 * 512..7 * 512].copy_from_slice(&boot);
    let fsinfo = fsinfo_sector(clusters - next as u64, next);
    image[512..1024].copy_from_slice(&fsinfo);
    image[7 * 512..8 * 512].copy_from_slice(&fsinfo);
    let mut fat_bytes = Vec::with_capacity(fat_size as usize * 512);
    for entry in &fat
    {
        fat_bytes.extend_from_slice(&entry.to_le_bytes());
    }
    fat_bytes.resize(fat_size as usize * 512, 0);
    for index in 0..FAT_COUNT
    {
        let offset = ((RESERVED_SECTORS + index * fat_size) * SECTOR) as usize;
        image[offset..offset + fat_bytes.len()].copy_from_slice(&fat_bytes);
    }
    write_dir(&root, &mut image, first_data, cluster_bytes, true, 0)?;
    for file in &written
    {
        let offset = (first_data + (file.first_cluster as u64 - 2) * CLUSTER_SECTORS) * SECTOR;
        image[offset as usize..offset as usize + file.bytes.len()].copy_from_slice(&file.bytes);
    }
    fs::write(path, &image)?;
    Ok(json!(written.iter()
                    .map(|file| {
                        json!({
                            "path": file.path,
                            "bytes": file.bytes.len(),
                            "first_cluster": file.first_cluster,
                            "sha256": package::sha256(&file.bytes)
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

fn write_dir(dir: &Dir, image: &mut [u8], first_data: u64, cluster_bytes: u64, root: bool,
             parent: u32)
             -> Result<()>
{
    let offset = ((first_data + (u64::from(dir.cluster) - 2) * CLUSTER_SECTORS) * SECTOR) as usize;
    let mut cursor = offset;
    if !root
    {
        write_entry(&mut image[cursor..cursor + 32], *b".          ", 0x10, dir.cluster, 0);
        cursor += 32;
        write_entry(&mut image[cursor..cursor + 32], *b"..         ", 0x10, parent, 0);
        cursor += 32;
    }
    for entry in &dir.entries
    {
        let (attr, size) = match &entry.node
        {
            Node::File(bytes) => (0x20, bytes.len() as u32),
            Node::Dir(_) => (0x10, 0)
        };
        write_entry(&mut image[cursor..cursor + 32],
                    name83(&entry.name)?,
                    attr,
                    entry.first_cluster,
                    size);
        cursor += 32;
        if cursor > offset + cluster_bytes as usize
        {
            return Err(error(format!("FAT32 directory '{}' exceeds one cluster", entry.name)));
        }
    }
    for entry in &dir.entries
    {
        if let Node::Dir(child) = &entry.node
        {
            write_dir(child, image, first_data, cluster_bytes, false, dir.cluster)?;
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

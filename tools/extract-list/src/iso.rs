//! Spec A1 — read a classic Source file's resources from the ISO.
//!
//! The disc is a System 7 era CD: an Apple partition map (or a bare volume)
//! wrapping a classic HFS volume. `mac-hfs` is that parser; the host block
//! device is `hfs-b`'s file-backed image. Nothing here writes to the disc.
//!
//! TEMPORARY scaffolding: this moves to the Phase B extractor when the list is
//! built, since that tool reads the same disc through the same code.

use std::path::{Path, PathBuf};

use hfs_b::Image;
use mac_formats::resource::Resource;
use mac_hfs::{Partition, Volume};

/// Where discs live: one directory, one rule, for every package and every disc.
pub const DISCO_DIR: &str = "iso";

/// The path of a disc named by an extract list. The list carries the filename
/// only; the repository fixes the directory.
pub fn disc_path(name: &str) -> PathBuf
{
    Path::new(DISCO_DIR).join(name)
}

/// Resolve a disc the extract list names, or say exactly what to supply.
///
/// The responsible party is the missing disc, the cause is that it is not in
/// the disc directory, and the next action names the disc by what it is, the
/// exact filename, and where to put it. No checksum or identity check: a disc
/// that has the resources is the disc the list meant.
pub fn require(name: &str, description: &str) -> Result<PathBuf, Box<dyn std::error::Error>>
{
    let path = disc_path(name);
    if path.is_file()
    {
        return Ok(path);
    }
    Err(mac_build::error(format!("this package takes its resources from the {description}, and \
                                  that disc is not present: expected {name} in the repository's \
                                  {DISCO_DIR} directory ({}). Download the {description}, save \
                                  it under exactly that filename there, and run the build \
                                  again; it continues once the disc is in place",
                                 path.display())))
}

/// One disc file's decoded resources, tagged with its path on the disc.
pub struct SourceFile
{
    pub path:      String,
    pub resources: Vec<Resource>
}

/// Read every source named by a package's list. A source is a file, or a
/// folder whose immediate files are all sources — font suitcases live in one
/// folder beside the System file, and a theme may span several files, so the
/// list names the files it draws from rather than assuming one.
pub fn sources(iso: &Path, paths: &[String])
               -> Result<Vec<SourceFile>, Box<dyn std::error::Error>>
{
    let mut image = Image::open(iso, false)?;
    let partition = partition(&mut image, iso)?;
    let mut volume = Volume::mount(&mut image, partition)?;
    let mut sources = Vec::new();
    for path in paths
    {
        let entry = resolve(&mut volume, path, iso)?;
        if entry.directory
        {
            let mut index = 0;
            while let Some(child) = volume.child(entry.id, index)?
            {
                index += 1;
                if child.directory
                {
                    return Err(mac_build::error(format!("{path} in {} contains the folder \
                                                         '{}'; a source folder must hold \
                                                         files, because the list names the \
                                                         files it draws from rather than \
                                                         guessing at a tree",
                                                        iso.display(),
                                                        mac_formats::text::decode(&child.name))));
                }
                let file = format!("{path}/{}", mac_formats::text::decode(&child.name));
                sources.push(SourceFile { path:      file.clone(),
                                          resources: entries(&mut volume, &file, iso)? });
            }
            continue;
        }
        sources.push(SourceFile { path:      path.clone(),
                                  resources: entries(&mut volume, path, iso)? });
    }
    Ok(sources)
}

fn entries(volume: &mut Volume<'_, Image>, source: &str, iso: &Path)
           -> Result<Vec<Resource>, Box<dyn std::error::Error>>
{
    let Some(mut fork) = resolve(volume, source, iso)?.resources
    else
    {
        return Err(mac_build::error(format!("{} has no resource fork in {}: the file is there \
                                             but carries no resources, so it cannot supply \
                                             them",
                                            source,
                                            iso.display())));
    };
    let mut bytes = Vec::new();
    let mut offset = 0u64;
    loop
    {
        let mut chunk = vec![0u8; 1 << 20];
        let read = volume.read(&mut fork, offset, &mut chunk)?;
        if read == 0
        {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        offset += read as u64;
    }
    mac_formats::resource::decode(&bytes).map_err(|cause| {
                                             mac_build::error(format!("the resource fork of {} \
                                                                       in {} is not a classic \
                                                                       resource fork: {cause}",
                                                                      source,
                                                                      iso.display()))
                                         })
}

/// Read one file's resource fork from the disc and decode its resources.
///
/// `source` is the path on the disc, because the extract list names the file it
/// takes from — not every package takes resources from the startup System file.
pub fn read_resources(iso: &Path, source: &str)
                      -> Result<Vec<Resource>, Box<dyn std::error::Error>>
{
    let mut image = Image::open(iso, false)?;
    let partition = partition(&mut image, iso)?;
    let mut volume = Volume::mount(&mut image, partition)?;
    entries(&mut volume, source, iso)
}

/// One directory entry, for finding the disc's layout before listing resources.
pub struct EntryInfo
{
    pub name:      String,
    pub directory: bool,
    pub data:      u64,
    pub resources: u64
}

/// List a folder on the disc (the volume root when `path` is empty), or the
/// forks of a file. Used to establish where a disc keeps its System file.
pub fn listing(iso: &Path, path: &str) -> Result<Vec<EntryInfo>, Box<dyn std::error::Error>>
{
    let mut image = Image::open(iso, false)?;
    let partition = partition(&mut image, iso)?;
    let mut volume = Volume::mount(&mut image, partition)?;
    let id = if path.is_empty() { 2 } else { resolve(&mut volume, path, iso)?.id };
    let Some(entry) = volume.entry(id)?
    else
    {
        return Err(mac_build::error(format!("{path} is missing from {}", iso.display())));
    };
    if !entry.directory
    {
        return Ok(vec![EntryInfo { name:      name(&entry.name),
                                   directory: false,
                                   data:      entry.data.map_or(0, |fork| fork.length),
                                   resources: entry.resources.map_or(0, |fork| fork.length) }]);
    }
    let mut entries = Vec::new();
    let mut index = 0;
    while let Some(child) = volume.child(id, index)?
    {
        entries.push(EntryInfo { name:      name(&child.name),
                                 directory: child.directory,
                                 data:      child.data.map_or(0, |fork| fork.length),
                                 resources: child.resources.map_or(0, |fork| fork.length) });
        index += 1;
    }
    Ok(entries)
}

/// Walk the whole volume and report every file whose resource fork carries a
/// resource of one of `kinds`, with the IDs found. A feasibility question —
/// "does this disc carry theme resources at all?" — is answered by the disc,
/// not by a guess about its vintage.
pub fn scan(iso: &Path, kinds: &[String])
            -> Result<(Vec<(String, Vec<(String, i16)>)>, usize), Box<dyn std::error::Error>>
{
    let wanted: Vec<[u8; 4]> = kinds.iter()
                                    .map(|kind| {
                                        let bytes = mac_formats::text::encode(kind)?;
                                        bytes.try_into().map_err(|_| {
                                            mac_build::error(format!("'{kind}' is not four Mac                                                                       Roman bytes"))
                                        })
                                    })
                                    .collect::<Result<Vec<_>, _>>()?;
    let mut image = Image::open(iso, false)?;
    let partition = partition(&mut image, iso)?;
    let mut volume = Volume::mount(&mut image, partition)?;
    let mut found = Vec::new();
    let mut examined = 0;
    walk_volume(&mut volume, iso, 2, "", &wanted, &mut found, &mut examined, 0)?;
    Ok((found, examined))
}

fn walk_volume(volume: &mut Volume<'_, Image>, iso: &Path, id: u32, path: &str,
               wanted: &[[u8; 4]], found: &mut Vec<(String, Vec<(String, i16)>)>,
               examined: &mut usize, depth: usize)
               -> Result<(), Box<dyn std::error::Error>>
{
    if depth > 24
    {
        return Ok(());
    }
    let mut index = 0;
    while let Some(child) = volume.child(id, index)?
    {
        index += 1;
        let name = name(&child.name);
        let child_path = if path.is_empty() { name.clone() } else { format!("{path}/{name}") };
        if child.directory
        {
            walk_volume(volume, iso, child.id, &child_path, wanted, found, examined, depth + 1)?;
            continue;
        }
        *examined += 1;
        if child.resources.as_ref().is_none_or(|fork| fork.length == 0)
        {
            continue;
        }
        // A malformed fork on one file must not stop the scan.
        let Ok(resources) = entries(volume, &child_path, iso)
        else
        {
            continue;
        };
        let hits: Vec<(String, i16)> =
            resources.iter()
                     .filter(|resource| wanted.contains(&resource.kind))
                     .map(|resource| (name_or_empty(&resource.kind), resource.id))
                     .collect();
        if !hits.is_empty()
        {
            found.push((child_path, hits));
        }
    }
    Ok(())
}

fn name_or_empty(kind: &[u8; 4]) -> String
{
    kind.iter().map(|byte| if byte.is_ascii_graphic() { *byte as char } else { '?' }).collect()
}

fn name(bytes: &[u8]) -> String
{
    mac_formats::text::decode(bytes)
}

/// One resource as the disc carries it, for the A1 acceptance listing.
pub struct ResourceInfo
{
    pub kind:       [u8; 4],
    pub id:         i16,
    pub name:       String,
    pub attributes: u8,
    pub length:     usize
}

pub fn resource_listing(iso: &Path, source: &str)
                        -> Result<Vec<ResourceInfo>, Box<dyn std::error::Error>>
{
    Ok(read_resources(iso, source)?.into_iter()
                                   .map(|resource| {
                                       ResourceInfo { kind:       resource.kind,
                                                      id:         resource.id,
                                                      name:
                                                          resource.name
                                                                  .as_deref()
                                                                  .map_or_else(String::new, name),
                                                      attributes: resource.attributes,
                                                      length:     resource.data.len() }
                                   })
                                   .collect())
}

fn partition(image: &mut Image, iso: &Path) -> Result<Partition, Box<dyn std::error::Error>>
{
    let partitions = mac_hfs::partitions(image)?;
    partitions.into_iter().next().ok_or_else(|| {
                                     mac_build::error(format!("{} has no classic HFS volume, so \
                                                               it holds no Macintosh System \
                                                               resources; supply a System 7.5.1 \
                                                               disc",
                                                              iso.display()))
                                 })
}

/// Walk a disc path from the volume root, naming the segment that is missing.
fn resolve(volume: &mut Volume<'_, Image>, source: &str, iso: &Path)
           -> Result<mac_hfs::CatalogEntry, Box<dyn std::error::Error>>
{
    let segments: Vec<&str> = source.split('/').filter(|segment| !segment.is_empty()).collect();
    if segments.is_empty()
    {
        return Err(mac_build::error(format!("'{source}' names no file in {}", iso.display())));
    }
    let mut parent = 2; // the HFS root directory's fixed CNID
    let mut entry = None;
    for (index, segment) in segments.iter().enumerate()
    {
        let name = mac_formats::text::encode(segment)?;
        let found = volume.lookup(parent, &name)?.ok_or_else(|| {
            mac_build::error(format!("{} has no '{segment}' in {}; the disc does not carry the \
                                      file this package takes resources from",
                                     iso.display(),
                                     segments[..=index].join("/")))
        })?;
        if index + 1 < segments.len() && !found.directory
        {
            return Err(mac_build::error(format!("'{}' in {} is not a folder, so '{}' cannot \
                                                 be inside it",
                                                segments[..=index].join("/"),
                                                iso.display(),
                                                source)));
        }
        parent = found.id;
        entry = Some(found);
    }
    Ok(entry.expect("a non-empty path resolves at least one segment"))
}

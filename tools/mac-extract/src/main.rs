//! Phase B: turn an extract list into a package's generated resources.
//!
//! Inputs are the package's `resources.json` and its `extract-list.json`; the
//! disc comes from the list, by filename, from the repository's `iso`
//! directory. Outputs are `extracted_resources.json` and `extracted_assets/`
//! beside the list. Both are gitignored build artifacts: the list is the
//! reviewed source of truth, the pair is what this tool derives from the disc.
//!
//! Nothing here is System- or Finder-specific: a package is a manifest, a list
//! and an output directory, so a third package with another disc is the same
//! command.
//!
//! Gates, in order:
//!  * every listed resource is present in the source file the list names;
//!  * every emitted payload is byte-identical to the disc's payload;
//!  * every entry that replaces a resource of ours still has the classic bytes our manifest
//!    compiles to (parity), so ripping the original out cannot change what legacy software
//!    observes. Entries taken by policy (`taken`) have no counterpart of ours and are exempt, as
//!    are the repeated IDs they create.

use std::{collections::BTreeMap,
          fs,
          path::{Path, PathBuf},
          process::ExitCode};

use hfs_b::Image;
use mac_formats::resource::Resource;
use mac_hfs::{Partition, Volume};
use serde::{Deserialize, Serialize};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn error(message: impl Into<String>) -> Box<dyn std::error::Error>
{
    std::io::Error::other(message.into()).into()
}

const DISC_DIRECTORY: &str = "iso";

#[derive(Deserialize)]
struct ExtractList
{
    description: String,
    iso:         String,
    sources:     Vec<ExtractSource>
}

#[derive(Deserialize)]
struct ExtractSource
{
    path:      String,
    resources: Vec<ExtractEntry>
}

#[derive(Deserialize, Serialize, Clone)]
struct ExtractEntry
{
    #[serde(rename = "type")]
    kind:       String,
    /// The ID the resource carries in the package.
    id:         i16,
    /// The ID the disc's file uses, when the importer renumbered it.
    #[serde(default)]
    source_id:  Option<i16>,
    symbol:     String,
    name:       Option<String>,
    attributes: u8,
    form:       String,
    #[serde(default)]
    taken:      bool
}

#[derive(Serialize)]
struct Generated
{
    resources: Vec<GeneratedResource>
}

#[derive(Serialize)]
struct GeneratedResource
{
    #[serde(rename = "type")]
    kind:       String,
    id:         i16,
    symbol:     String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name:       Option<String>,
    attributes: u8,
    payload:    Payload
}

#[derive(Serialize)]
#[serde(tag = "format", rename_all = "snake_case")]
enum Payload
{
    File
    {
        path: String
    }
}

fn main() -> ExitCode
{
    match run()
    {
        Ok(code) => code,
        Err(cause) =>
        {
            eprintln!("mac-extract: {cause}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> String
{
    "mac-extract — build a package's generated resources from its extract list\n\nmac-extract \
     extract --package PATH --list PATH [--check]\n\n\x20 --package  the package's \
     resources.json\n\x20 --list     its extract-list.json; the disc it names is read from \
     iso/\n\x20 --check    compare what would be generated with what is on disk\n\nOutputs go \
     beside the list: extracted_resources.json and extracted_assets/."
                                                                      .to_string()
}

fn run() -> Result<ExitCode>
{
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first()
    else
    {
        print!("{}", usage());
        return Ok(ExitCode::FAILURE);
    };
    match command.as_str()
    {
        "extract" => extract(&args[1..]),
        "help" | "--help" | "-h" =>
        {
            print!("{}", usage());
            Ok(ExitCode::SUCCESS)
        }
        other => Err(error(format!("unknown command '{other}'; run mac-extract help")))
    }
}

fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str>
{
    args.iter()
        .position(|argument| argument == name)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

fn required<'a>(args: &'a [String], name: &str) -> Result<&'a str>
{
    option(args, name).ok_or_else(|| error(format!("{name} is required")))
}

fn extract(args: &[String]) -> Result<ExitCode>
{
    let package_path = PathBuf::from(required(args, "--package")?);
    let list_path = PathBuf::from(required(args, "--list")?);
    let check = args.iter().any(|argument| argument == "--check");
    let list: ExtractList = serde_json::from_slice(&fs::read(&list_path).map_err(|cause| {
                                                        error(format!("cannot read extract list \
                                                                       {}: {cause}",
                                                                      list_path.display()))
                                                    })?).map_err(|cause| {
                                                            error(format!("{} is not an extract \
                                                                           list: {cause}",
                                                                          list_path.display()))
                                                        })?;
    let disc = Path::new(DISC_DIRECTORY).join(&list.iso);
    if !disc.is_file()
    {
        return Err(error(format!("this package takes its resources from the {}, and that disc \
                                  is not present: expected {} in the repository's \
                                  {DISC_DIRECTORY} directory ({}). Download the {}, save it \
                                  under exactly that filename there, and run the build again; \
                                  it continues once the disc is in place",
                                 list.description,
                                 list.iso,
                                 disc.display(),
                                 list.description)));
    }
    let output = list_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let manifest = mac_build::Manifest::read(&package_path)?;
    let ours = ours(&manifest, &package_path);
    let mut image = Image::open(&disc, false)?;
    let partition = partition(&mut image, &disc)?;
    let mut volume = Volume::mount(&mut image, partition)?;
    let mut generated = Vec::new();
    let mut payloads: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut replaced = 0;
    let mut taken = 0;
    let mut parity_failures = Vec::new();
    let mut by_family: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for source in &list.sources
    {
        let disc_resources = read_source(&mut volume, &source.path, &disc)?;
        for entry in &source.resources
        {
            let kind = fourcc(&entry.kind)?;
            let disc_id = entry.source_id.unwrap_or(entry.id);
            let Some(found) =
                disc_resources.iter()
                              .find(|resource| resource.kind == kind && resource.id == disc_id)
            else
            {
                return Err(error(format!("{} lacks {} {} ({}), which {} names for this \
                                          package: the disc does not carry that resource in \
                                          {}. Supply a disc whose {} contains it, or remove \
                                          the entry from the list and its resource from the \
                                          package",
                                         disc.display(),
                                         entry.kind,
                                         disc_id,
                                         entry.symbol,
                                         list_path.display(),
                                         source.path,
                                         source.path)));
            };
            let file = asset_name(entry);
            payloads.insert(file.clone(), found.data.clone());
            match (entry.taken, ours.get(&(entry.kind.clone(), entry.id)))
            {
                (true, _) => taken += 1,
                (false, Some(original)) =>
                {
                    replaced += 1;
                    if original != &found.data
                    {
                        parity_failures.push(format!("{} {} ({}): the disc's bytes are not the \
                                                      classic payload our manifest compiles",
                                                     entry.kind, entry.id, entry.symbol));
                    }
                }
                (false, None) =>
                {
                    replaced += 1;
                    parity_failures.push(format!("{} {} ({}): the list takes a resource the \
                                                  package no longer declares",
                                                 entry.kind, entry.id, entry.symbol));
                }
            }
            let family = source.path.rsplit('/').next().unwrap_or_default().to_string();
            let counts = by_family.entry(family).or_default();
            counts.0 += 1;
            counts.1 += found.data.len();
            generated.push(GeneratedResource { kind:       entry.kind.clone(),
                                               id:         entry.id,
                                               symbol:     entry.symbol.clone(),
                                               name:       entry.name.clone(),
                                               attributes: entry.attributes,
                                               payload:
                                                   Payload::File { path:
                                                                       format!("extracted_assets/\
                                                                                {file}") } });
        }
    }
    generated.sort_by(|a, b| (&a.kind, a.id, &a.symbol).cmp(&(&b.kind, b.id, &b.symbol)));
    let duplicates = duplicate_keys(&generated);
    let text = format!("{}\n", serde_json::to_string_pretty(&Generated { resources: generated })?);
    println!("{}", list.description);
    println!("disc:  {}", disc.display());
    for (family, (count, bytes)) in &by_family
    {
        println!("  {family:<38} {count:>4} resources, {bytes:>9} bytes");
    }
    println!("total {} resources — {replaced} replace a resource of ours, {taken} new to the \
              package, {} bytes",
             by_family.values().map(|(count, _)| count).sum::<usize>(),
             by_family.values().map(|(_, bytes)| bytes).sum::<usize>());
    for failure in &parity_failures
    {
        eprintln!("parity: {failure}");
    }
    if !duplicates.is_empty()
    {
        println!("\n{} repeated (type, id) keys: {}", duplicates.len(), duplicates.join(", "));
        println!("A resource file cannot hold two resources with one key. Fonts are imported by \
                  enumeration, so the disc's own IDs repeat across families; Phase B must number \
                  them in the package and regenerate FOND against those numbers before this file \
                  can be included in resources.json.");
    }
    if check
    {
        return check_output(&output, &text, &payloads);
    }
    fs::create_dir_all(output.join("extracted_assets"))?;
    for (file, bytes) in &payloads
    {
        write_if_changed(&output.join("extracted_assets").join(file), bytes)?;
    }
    write_if_changed(&output.join("extracted_resources.json"), text.as_bytes())?;
    println!("\nwrote {} and extracted_assets/ ({} assets)",
             output.join("extracted_resources.json").display(),
             payloads.len());
    Ok(if parity_failures.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

/// The package's own resources as classic payload bytes, by `(type, id)`.
/// `native_code` entries have no classic payload and are absent here.
fn ours(manifest: &mac_build::Manifest, path: &Path) -> BTreeMap<(String, i16), Vec<u8>>
{
    let mut map = BTreeMap::new();
    let Ok(payloads) = manifest.classic_payloads(path, None)
    else
    {
        return map;
    };
    for (definition, payload) in manifest.resources.iter().zip(payloads)
    {
        if let Ok(payload) = payload
        {
            map.insert((definition.kind.clone(), definition.id), payload.data);
        }
    }
    map
}

fn asset_name(entry: &ExtractEntry) -> String
{
    let kind: String =
        entry.kind.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    format!("{kind}-{}-{}.bin", entry.id, entry.symbol)
}

fn duplicate_keys(generated: &[GeneratedResource]) -> Vec<String>
{
    let mut seen: BTreeMap<(String, i16), usize> = BTreeMap::new();
    for resource in generated
    {
        *seen.entry((resource.kind.clone(), resource.id)).or_default() += 1;
    }
    seen.into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|((kind, id), count)| format!("{kind} {id} ({count}×)"))
        .collect()
}

fn check_output(output: &Path, text: &str, payloads: &BTreeMap<String, Vec<u8>>)
                -> Result<ExitCode>
{
    let mut differences = Vec::new();
    let manifest = output.join("extracted_resources.json");
    match fs::read(&manifest)
    {
        Ok(existing) if existing == text.as_bytes() =>
        {}
        Ok(_) => differences.push(format!("{} differs", manifest.display())),
        Err(_) => differences.push(format!("{} is missing", manifest.display()))
    }
    for (file, bytes) in payloads
    {
        let path = output.join("extracted_assets").join(file);
        match fs::read(&path)
        {
            Ok(existing) if existing == *bytes =>
            {}
            Ok(_) => differences.push(format!("{} differs", path.display())),
            Err(_) => differences.push(format!("{} is missing", path.display()))
        }
    }
    if differences.is_empty()
    {
        println!("\ncheck: the generated resources still match the disc and the list");
        return Ok(ExitCode::SUCCESS);
    }
    for difference in &differences
    {
        eprintln!("check: {difference}");
    }
    Err(error(format!("{} generated files disagree with the disc and the list; rebuild them",
                      differences.len())))
}

fn write_if_changed(path: &Path, contents: &[u8]) -> Result<()>
{
    if fs::read(path).is_ok_and(|existing| existing == contents)
    {
        return Ok(());
    }
    fs::write(path, contents)?;
    Ok(())
}

fn fourcc(kind: &str) -> Result<[u8; 4]>
{
    mac_formats::text::encode(kind)?.try_into()
                                    .map_err(|_| {
                                        error(format!("'{kind}' is not four Mac Roman bytes"))
                                    })
}

fn partition(image: &mut Image, disc: &Path) -> Result<Partition>
{
    let partitions = mac_hfs::partitions(image)?;
    partitions.into_iter().next().ok_or_else(|| {
                                     error(format!("{} has no classic HFS volume, so it holds no \
                                                    Macintosh resources; supply a System 7.5.1 \
                                                    disc",
                                                   disc.display()))
                                 })
}

/// Read one file's resource fork from the disc. Both the filesystem and the
/// resource codec are the ones the rest of the toolchain uses; there is no
/// second parser here.
fn read_source(volume: &mut Volume<'_, Image>, source: &str, disc: &Path) -> Result<Vec<Resource>>
{
    let mut parent = 2;
    let mut entry = None;
    for segment in source.split('/').filter(|segment| !segment.is_empty())
    {
        let name = mac_formats::text::encode(segment)?;
        let found = volume.lookup(parent, &name)?.ok_or_else(|| {
                                                      error(format!("{} has no '{segment}' while \
                                                                     resolving '{source}'; the \
                                                                     disc does not carry the \
                                                                     file this list names",
                                                                    disc.display()))
                                                  })?;
        parent = found.id;
        entry = Some(found);
    }
    let Some(mut fork) = entry.and_then(|entry| entry.resources)
    else
    {
        return Err(error(format!("{source} in {} has no resource fork", disc.display())));
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
                                             error(format!("the resource fork of {source} in {} \
                                                            is not a classic resource fork: \
                                                            {cause}",
                                                           disc.display()))
                                         })
}

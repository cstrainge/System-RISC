//! Author the FOND family records for the faces this project imports.
//!
//! A font manager finds a family through its FOND: the family ID, the
//! association table that maps size and style to a face resource, and the family
//! name. The imported faces arrive without one, so this tool writes the record
//! we own — identity and associations only, never a copy of the disc's own
//! family data.
//!
//! Families are read from the extract list, not from a table in this file: each
//! imported face carries the size, style and family name the disc's own FOND
//! declares for it, and the generator groups faces by that name. An outline
//! face is associated at size 0 (scalable); a strike at the size it was cut at.
//! Nothing here repeats that data, so a corrected association or a renumbered
//! face flows through without editing this tool.
//!
//! `verify` then proves, against the packaged System fork, that every generated
//! family is admitted by the same rules the resident catalog applies.

use std::{collections::{BTreeMap, BTreeSet},
          fs,
          path::PathBuf,
          process::ExitCode};

use serde::{Deserialize, Serialize};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn error(message: impl Into<String>) -> Box<dyn std::error::Error>
{
    std::io::Error::other(message.into()).into()
}

#[derive(Deserialize)]
struct ExtractList
{
    sources: Vec<ExtractSource>
}

#[derive(Deserialize)]
struct ExtractSource
{
    path:      String,
    resources: Vec<ExtractEntry>
}

#[derive(Deserialize)]
struct ExtractEntry
{
    #[serde(rename = "type")]
    kind:   String,
    id:     i16,
    name:   Option<String>,
    #[serde(default)]
    taken:  bool,
    #[serde(default)]
    size:   Option<i16>,
    #[serde(default)]
    style:  Option<u16>,
    #[serde(default)]
    family: Option<String>
}

/// Family records live in their own block, above the faces, so the two
/// allocations cannot push each other around.
const FIRST_ASSIGNED_FOND_ID: i16 = 13400;

#[derive(Deserialize, Serialize)]
struct ManifestEntry
{
    #[serde(rename = "type")]
    kind:       String,
    id:         i16,
    symbol:     String,
    name:       String,
    attributes: u8,
    payload:    Payload
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "format", rename_all = "snake_case")]
enum Payload
{
    File
    {
        path: String
    }
}

#[derive(Deserialize, Serialize)]
struct Entries
{
    resources: Vec<ManifestEntry>
}

fn main() -> ExitCode
{
    match run()
    {
        Ok(code) => code,
        Err(cause) =>
        {
            eprintln!("mac-fonts: {cause}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> String
{
    "mac-fonts — author the FOND records for imported faces\n\nmac-fonts generate --list PATH \
     --assets DIR --entries PATH\n\x20   Read a package's extract list, write FOND-<id>.bin for \
     each imported
     \x20   family, and write the manifest entries that reference them.\n\nmac-fonts verify --fork \
     PATH --families PATH\n\x20   Check, against a packaged resource fork, that every generated \
     family
     \x20   is admitted by the same rules the resident font catalog applies."
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
        "generate" => generate(&args[1..]),
        "verify" => verify(&args[1..]),
        "help" | "--help" | "-h" =>
        {
            print!("{}", usage());
            Ok(ExitCode::SUCCESS)
        }
        other => Err(error(format!("unknown command '{other}'; run mac-fonts help")))
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

/// Style bits for a face, from the name the suitcase gives it.
fn style(name: &str) -> u16
{
    let name = name.to_ascii_lowercase();
    let mut bits = 0;
    if name.contains("bold")
    {
        bits |= 1;
    }
    if name.contains("italic") || name.contains("oblique")
    {
        bits |= 2;
    }
    if name.contains("condensed") || name.contains("cn ")
    {
        bits |= 32;
    }
    bits
}

/// A one-face family is selected at plain style: its name already says
/// "Condensed" or "Bold", so marking the only face as a styled variant would
/// leave the family empty at the style an application actually asks for.
fn face_style(name: Option<&str>, single: bool) -> u16
{
    if single { 0 } else { name.map(style).unwrap_or(0) }
}

/// A FOND record: the family's identity and its association table, then the
/// family name. Metrics live in each face, so the numeric fields are zero.
fn record(id: i16, name: &str, faces: &[(i16, u16, i16)]) -> Result<Vec<u8>>
{
    let mut data = Vec::with_capacity(54 + faces.len() * 6 + name.len() + 1);
    let short = |data: &mut Vec<u8>, value: i16| data.extend_from_slice(&value.to_be_bytes());
    short(&mut data, 0); // ffFlags
    short(&mut data, id); // ffFamID
    short(&mut data, 0); // ffFirstChar
    short(&mut data, 255); // ffLastChar
    for _ in 0..4
    {
        short(&mut data, 0); // ascent, descent, leading, maximum width
    }
    data.extend_from_slice(&0u32.to_be_bytes()); // ffWTabOff
    data.extend_from_slice(&0u32.to_be_bytes()); // ffKernOff
    data.extend_from_slice(&0u32.to_be_bytes()); // ffStylOff
    for _ in 0..9
    {
        short(&mut data, 0); // ffProperty
    }
    for _ in 0..2
    {
        short(&mut data, 0); // ffIntl
    }
    let version: u16 = 1;
    data.extend_from_slice(&version.to_be_bytes());
    let count = i16::try_from(faces.len()).map_err(|_| error("too many faces in one family"))?;
    short(&mut data, count - 1);
    for (size, style, face) in faces
    {
        short(&mut data, *size);
        data.extend_from_slice(&style.to_be_bytes());
        short(&mut data, *face);
    }
    let encoded = mac_formats::text::encode(name)?;
    if encoded.len() > 255
    {
        return Err(error(format!("family name '{name}' is longer than 255 bytes")));
    }
    data.push(encoded.len() as u8);
    data.extend_from_slice(&encoded);
    Ok(data)
}

/// One imported face as the extract list describes it.
struct Face
{
    outline: bool,
    name:    Option<String>,
    id:      i16,
    size:    Option<i16>,
    style:   Option<u16>
}

fn generate(args: &[String]) -> Result<ExitCode>
{
    let list_path = PathBuf::from(required(args, "--list")?);
    let assets = PathBuf::from(required(args, "--assets")?);
    let entries_path = PathBuf::from(required(args, "--entries")?);
    let list: ExtractList = serde_json::from_slice(&fs::read(&list_path)?)?;
    // Group the imported faces by the family the disc's own FOND gives them.
    // Everything here comes from the list, so a renumbered face or a corrected
    // association flows through without editing this tool.
    let mut grouped: BTreeMap<(String, String), Vec<Face>> = BTreeMap::new();
    for source in &list.sources
    {
        let suit = source.path.rsplit('/').next().unwrap_or_default().to_string();
        for entry in source.resources.iter().filter(|entry| entry.taken)
        {
            let family = entry.family.clone().unwrap_or_else(|| suit.clone());
            grouped.entry((suit.clone(), family)).or_default().push(Face { outline: entry.kind
                                                                                    == "sfnt",
                                                                           name:    entry.name
                                                                                         .clone(),
                                                                           id:      entry.id,
                                                                           size:    entry.size,
                                                                           style:   entry.style });
        }
    }
    let resident: BTreeSet<i16> = report_ids(&list);
    let mut families = Vec::new();
    let mut next = FIRST_ASSIGNED_FOND_ID;
    for ((_, family), faces) in grouped
    {
        while resident.contains(&next)
        {
            next += 1;
        }
        let id = next;
        next += 1;
        let single = faces.len() == 1;
        let mut associations: Vec<(i16, u16, i16)> =
            faces.iter()
                 .map(|face| {
                     if face.outline
                     {
                         // An outline renders at any size, so it is
                         // associated as scalable. A suitcase of outlines
                         // has no strike table, so its style comes from the
                         // face's own name.
                         (0, face_style(face.name.as_deref(), single), face.id)
                     }
                     else
                     {
                         // A strike is associated at the size and style the
                         // disc's own FOND gave it, which the list carries.
                         (face.size.unwrap_or(0), face.style.unwrap_or(0), face.id)
                     }
                 })
                 .collect();
        associations.sort_by_key(|(size, style, face)| (*size, *style, *face));
        families.push((id, family, associations));
    }
    families.sort_by_key(|(id, _, _)| *id);
    let mut resources = Vec::new();
    for (id, name, faces) in &families
    {
        let bytes = record(*id, name, faces)?;
        let file = format!("FOND-{id}.bin");
        fs::write(assets.join(&file), &bytes)?;
        resources.push(ManifestEntry { kind:       "FOND".to_string(),
                                       id:         *id,
                                       symbol:     symbol(name, *id),
                                       name:       name.to_string(),
                                       attributes: 0,
                                       payload:    Payload::File { path: format!("assets/fonts/\
                                                                                  {file}") } });
        println!("{id:>6}  {name:<26} {} face(s): {}",
                 faces.len(),
                 faces.iter()
                      .map(|(size, style, face)| {
                          if *size == 0
                          {
                              format!("{face}@scale/{style}")
                          }
                          else
                          {
                              format!("{face}@{size}/{style}")
                          }
                      })
                      .collect::<Vec<_>>()
                      .join(" "));
    }
    fs::write(&entries_path,
              format!("{}\n", serde_json::to_string_pretty(&Entries { resources })?))?;
    println!("\nwrote {} FOND records and {}", families.len(), entries_path.display());
    Ok(ExitCode::SUCCESS)
}

/// Family IDs the package already installs. They keep their own numbers; a new
/// family is never assigned one of them.
fn report_ids(list: &ExtractList) -> BTreeSet<i16>
{
    let mut ids = BTreeSet::from([0, 2, 3, 4, 16, 20, 21, 22, 23]);
    for source in &list.sources
    {
        for entry in source.resources.iter().filter(|entry| entry.taken)
        {
            ids.insert(entry.id);
        }
    }
    ids
}


fn symbol(name: &str, id: i16) -> String
{
    let mut symbol = String::from("FOND_");
    for character in name.chars()
    {
        if character.is_ascii_alphanumeric()
        {
            symbol.push(character.to_ascii_uppercase());
        }
        else if !symbol.ends_with('_')
        {
            symbol.push('_');
        }
    }
    while symbol.ends_with('_')
    {
        symbol.pop();
    }
    format!("{symbol}_{}", id.unsigned_abs())
}

/// The resident catalog's admission rules, applied to a packaged fork: the
/// family's ID comes from its payload, the count from offset 52, the
/// associations from 54, and each association names a face by size and kind.
fn verify(args: &[String]) -> Result<ExitCode>
{
    let fork = PathBuf::from(required(args, "--fork")?);
    let package_path = PathBuf::from(required(args, "--package")?);
    let package: Package = serde_json::from_slice(&fs::read(&package_path)?)?;
    let expected: Vec<(&str, i16)> = package.resources
                                            .iter()
                                            .filter(|entry| entry.kind == "FOND")
                                            .map(|entry| (entry.name.as_str(), entry.id))
                                            .collect();
    let resources = mac_formats::resource::decode(&fs::read(&fork).map_err(|cause| {
                                                                      error(format!("cannot read \
                                                                                     {}: {cause}",
                                                                                    fork.display()))
                                                                  })?).map_err(|cause| {
                        error(format!("{} is not a resource fork: {cause}", fork.display()))
                    })?;
    let find = |kind: &[u8; 4], id: i16| {
        resources.iter().find(|resource| resource.kind == *kind && resource.id == id)
    };
    let mut failures = Vec::new();
    let mut seen: BTreeSet<i16> = BTreeSet::new();
    for (name, id) in &expected
    {
        if !seen.insert(*id)
        {
            failures.push(format!("FOND {id} is declared more than once"));
        }
        let Some(fond) = find(b"FOND", *id)
        else
        {
            failures.push(format!("FOND {id} ({name}) is missing from the fork"));
            continue;
        };
        let data = &fond.data;
        let family = data.get(2..4).map(|bytes| i16::from_be_bytes([bytes[0], bytes[1]]));
        if family != Some(*id)
        {
            failures.push(format!("FOND {id} names family {family:?}"));
            continue;
        }
        let Some(count) =
            data.get(52..54).map(|bytes| i16::from_be_bytes([bytes[0], bytes[1]]) as i32 + 1)
        else
        {
            failures.push(format!("FOND {id} is too short"));
            continue;
        };
        if count <= 0 || 54 + count as usize * 6 > data.len()
        {
            failures.push(format!("FOND {id} association table overruns its record"));
            continue;
        }
        let mut faces = 0;
        for index in 0..count as usize
        {
            let at = 54 + index * 6;
            let size = i16::from_be_bytes([data[at], data[at + 1]]);
            let style = u16::from_be_bytes([data[at + 2], data[at + 3]]);
            let face = i16::from_be_bytes([data[at + 4], data[at + 5]]);
            let target = if size == 0
            {
                find(b"sfnt", face)
            }
            else
            {
                find(b"NFNT", face).or_else(|| find(b"FONT", face))
            };
            match target
            {
                Some(target) if !target.data.is_empty() =>
                {
                    faces += 1;
                }
                _ =>
                {
                    failures.push(format!("FOND {id} ({name}) face size {size} style {style} ->                                            {face} has no {} to render",
                                          if size == 0 { "sfnt" } else { "strike" }));
                }
            }
        }
        if faces == 0
        {
            failures.push(format!("FOND {id} ({name}) has no usable face and would be omitted"));
        }
        // A strike's own header states a line height, which has to track the
        // size the family claims it for: a strike cut at 9pt does not have a
        // 12pt line height. Associations are not sorted by size (the bundled
        // families are not), so each is checked on its own, against a band
        // measured across the strikes this disc carries. It catches a gross
        // mispairing only — a swap between two nearby sizes can fall inside the
        // band — which is why the association is derived from the disc's own
        // FOND rather than retyped.
        for index in 0..count as usize
        {
            let at = 54 + index * 6;
            let size = i16::from_be_bytes([data[at], data[at + 1]]);
            let face = i16::from_be_bytes([data[at + 4], data[at + 5]]);
            if size == 0
            {
                continue;
            }
            let Some(strike) = find(b"NFNT", face).or_else(|| find(b"FONT", face))
            else
            {
                continue;
            };
            let Some(height) =
                strike.data.get(14..16).map(|bytes| i16::from_be_bytes([bytes[0], bytes[1]]))
            else
            {
                continue;
            };
            if !(-3..=8).contains(&(height - size))
            {
                failures.push(format!("FOND {id} ({name}): the {size}pt association points at a \
                                       strike whose line height is {height}, which does not \
                                       belong to that size"));
            }
        }
        let named = fond.name.as_deref().map(mac_formats::text::decode).unwrap_or_default();
        if named != *name
        {
            failures.push(format!("FOND {id} is named {named:?}, expected {name:?}"));
        }
        println!("{name:<26} FOND {id:>6}  {faces}/{count} faces resolve, named {named:?}");
    }
    if failures.is_empty()
    {
        println!("\nverify: {} families are admitted by the catalog's own rules", expected.len());
        return Ok(ExitCode::SUCCESS);
    }
    for failure in &failures
    {
        eprintln!("verify: {failure}");
    }
    Err(error(format!("{} families would not be admitted; fix them before the fork ships",
                      failures.len())))
}

#[derive(Deserialize)]
struct Package
{
    resources: Vec<PackageResource>
}

#[derive(Deserialize)]
struct PackageResource
{
    /// Absent on the include entry the manifest starts with.
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    id:   i16,
    #[serde(default)]
    name: String
}

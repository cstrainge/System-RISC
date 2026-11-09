//! TEMPORARY list-builder scaffolding — spec Phase A1, A2 and A3.
//!
//! A1 reads a classic System file from the disc named by an extract list, A2
//! resolves every resource in a package's `resources.json` down to the classic
//! payload bytes both sides can be compared at, and A3 classifies those
//! resources against the disc's (spec §7.2).
//!
//! This is not a shipped tool. It exists to produce `extract-list.json` once;
//! the permanent pieces are the compiler's `include` support (mac-build) and
//! the Phase B extractor that ninja runs per build. Removing this crate is
//! meant to be one delete: remove `tools/extract-list/` and its line in the
//! workspace `members` list. Nothing else references it.

use std::{collections::{BTreeMap, BTreeSet},
          fmt, fs,
          path::{Path, PathBuf}};

use mac_build::{Manifest, Result, error, manifest::Payload};
use mac_formats::resource::Resource;
use serde::Serialize;

use crate::usage::Usage;

pub mod iso;
pub mod usage;

/// One extract-list entry: what the extractor takes, and the classic form it
/// must land in. Metadata only — no payload bytes, so the list is safe to
/// check in and is the reviewed statement of what we take from Apple.
#[derive(Serialize)]
pub struct ExtractEntry
{
    #[serde(rename = "type")]
    pub kind:       String,
    /// The ID the resource carries in the package.
    pub id:         i16,
    /// The ID the disc's file uses, when the two differ. Fonts are imported by
    /// enumeration and the disc reuses IDs per suitcase, so the importer
    /// assigns its own from a range it owns; the disc's ID stays as provenance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_id:  Option<i16>,
    /// The package symbol the extracted resource carries, so the generated file
    /// can replace a hand-authored entry without breaking code that names it.
    pub symbol:     String,
    pub name:       Option<String>,
    pub attributes: u8,
    pub form:       String,
    /// True when the disc's copy is taken by policy rather than by matching our
    /// key. Fonts are imported this way, so their IDs are the disc's, not ours.
    #[serde(skip_serializing_if = "is_false")]
    pub taken:      bool,
    /// For a bitmap strike, the size and style the disc's own FOND associates it
    /// with. A strike does not carry its point size, so the disc is the only
    /// source for it; recording it here keeps the pairing derived rather than
    /// retyped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size:       Option<i16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style:      Option<u16>,
    /// The family name the disc's own FOND gives these faces. A strike is
    /// unnamed; the FOND is what names the family it belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family:     Option<String>
}

fn is_false(value: &bool) -> bool
{
    !*value
}

/// The extract list: a complete description of its own source, so a package
/// that names it needs nothing else and a third package can name another disc.
#[derive(Serialize)]
pub struct ExtractList
{
    /// What the disc actually is, for the person who has to find it.
    pub description: String,
    /// The disc's filename inside the repository's `iso` directory.
    pub iso:         String,
    /// One section per disc file the package takes from, in the order the
    /// sources were read. A package may draw from several files (fonts live in
    /// suitcases beside the System file), and their `(type, id)` numbers are
    /// only unique within one file.
    pub sources:     Vec<ExtractSource>
}

#[derive(Serialize)]
pub struct ExtractSource
{
    /// The file's path on the disc.
    pub path:      String,
    pub resources: Vec<ExtractEntry>
}

impl ExtractList
{
    /// Deterministic text: sorted entries, fixed field order, no timestamps.
    pub fn to_json(&self) -> Result<String>
    {
        Ok(serde_json::to_string_pretty(self)?)
    }
}


/// A payload form, named exactly as `resources.json` spells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Form
{
    File,
    Hex,
    Pstring,
    Strings,
    Menu,
    OpenTypes,
    NativeCode
}

impl Form
{
    pub fn as_str(self) -> &'static str
    {
        match self
        {
            Self::File => "file",
            Self::Hex => "hex",
            Self::Pstring => "pstring",
            Self::Strings => "strings",
            Self::Menu => "menu",
            Self::OpenTypes => "open_types",
            Self::NativeCode => "native_code"
        }
    }
}

fn form(payload: &Payload) -> Form
{
    match payload
    {
        Payload::File { .. } => Form::File,
        Payload::Hex { .. } => Form::Hex,
        Payload::Pstring { .. } => Form::Pstring,
        Payload::Strings { .. } => Form::Strings,
        Payload::Menu { .. } => Form::Menu,
        Payload::OpenTypes { .. } => Form::OpenTypes,
        Payload::NativeCode { .. } => Form::NativeCode
    }
}

/// One of our resources, resolved to the classic bytes an ISO payload would be
/// compared with. `classic` is `None` only for `native_code`, which is ours by
/// definition and never compared or extracted (spec §9.5).
pub struct OursResource
{
    pub source:     String,
    pub kind:       String,
    pub id:         i16,
    pub symbol:     String,
    pub name:       Option<Vec<u8>>,
    pub attributes: u8,
    pub form:       Form,
    pub classic:    Option<Vec<u8>>,
    /// True for a resource that came in through the generated include rather
    /// than from the manifest's own entries.
    pub generated:  bool
}

/// A resource of ours that does not resolve to classic bytes, with the reason.
pub struct Unresolvable
{
    pub source: String,
    pub kind:   String,
    pub id:     i16,
    pub symbol: String,
    pub reason: String
}

impl fmt::Display for Unresolvable
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        write!(f, "{} {} {} ({}): {}", self.source, self.kind, self.id, self.symbol, self.reason)
    }
}

/// One package's resources after A2.
pub struct Package
{
    pub source:       String,
    pub path:         PathBuf,
    pub resources:    Vec<OursResource>,
    pub unresolvable: Vec<Unresolvable>
}

/// A2: read a package manifest and resolve every payload to classic bytes.
///
/// Resolution goes through the same encoder the package build uses — the
/// manifests are parsed once, by `mac-build`, and no form is re-implemented
/// here. `native_code` is classified without resolving: its payload is this
/// project's own native code, not classic bytes.
pub fn read_package(source: &str, path: &Path) -> Result<Package>
{
    let manifest = Manifest::read(path)?;
    let payloads = manifest.classic_payloads(path, None)?;
    let mut resources = Vec::with_capacity(manifest.resources.len());
    let mut unresolvable = Vec::new();
    for (definition, payload) in manifest.resources.iter().zip(payloads)
    {
        let form = form(&definition.payload);
        let name = definition.name.as_deref().map(mac_formats::text::encode).transpose()?;
        let classic = if form == Form::NativeCode
        {
            None
        }
        else
        {
            match payload
            {
                Ok(payload) => Some(payload.data),
                Err(cause) =>
                {
                    unresolvable.push(Unresolvable { source: source.to_string(),
                                                     kind:   definition.kind.clone(),
                                                     id:     definition.id,
                                                     symbol: definition.symbol.clone(),
                                                     reason: cause.to_string() });
                    continue;
                }
            }
        };
        // Extraction writes its assets under `extracted_assets/`, so a payload
        // there is a generated face rather than one this package authors.
        let generated = matches!(&definition.payload,
        Payload::File { path }
            if path.components()
                   .next()
                   .is_some_and(|part| {
                       part.as_os_str() == "extracted_assets"
                   }));
        resources.push(OursResource { source: source.to_string(),
                                      kind: definition.kind.clone(),
                                      id: definition.id,
                                      symbol: definition.symbol.clone(),
                                      name,
                                      attributes: definition.attributes,
                                      form,
                                      classic,
                                      generated });
    }
    Ok(Package { source: source.to_string(),
                 path: path.to_path_buf(),
                 resources,
                 unresolvable })
}

/// One classic resource on the ISO side, tagged with the source file it came
/// from. A package may draw from more than one disc file: fonts are suitcases
/// beside the System file, and their `(type, id)` pairs collide across them, so
/// the source file is part of a resource's identity.
pub struct IsoResource
{
    pub source:   String,
    /// Read for comparison evidence only: an unapproved source can show that a
    /// resource exists on the disc, but nothing is extracted from it.
    pub held:     bool,
    pub resource: Resource
}

/// What spec §7.2 decides for one of our resources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict
{
    /// Byte-identical payload, name and attributes: pristine Apple bytes.
    Extract,
    /// At least one of payload, name or attributes differs from the ISO's.
    Customised
    {
        payload: bool, name: bool, attributes: bool
    },
    /// The ISO has no counterpart: invented here.
    OursOnly,
    /// Several disc files carry this `(type, id)` and none is byte-identical.
    /// Keeping ours is the only safe answer; the report names the candidates.
    Ambiguous,
    /// Matches a source that is not approved for extraction: identical bytes,
    /// but taken from a file this phase does not accept.
    Held,
    /// Our own native code. Never compared, never extracted.
    NativeCode
}

impl Verdict
{
    pub fn reason(self) -> &'static str
    {
        match self
        {
            Self::Extract => "identical",
            Self::Customised { .. } => "customised",
            Self::OursOnly => "ours-only",
            Self::Ambiguous => "ambiguous",
            Self::Held => "held",
            Self::NativeCode => "native-code"
        }
    }

    /// What differs, for the report's customised entries.
    pub fn detail(self) -> &'static str
    {
        match self
        {
            Self::Customised { payload: true,
                               name: true,
                               attributes: true } => "payload, name and attributes differ",
            Self::Customised { payload: true,
                               name: true,
                               attributes: false } => "payload and name differ",
            Self::Customised { payload: true,
                               name: false,
                               attributes: true } => "payload and attributes differ",
            Self::Customised { payload: false,
                               name: true,
                               attributes: true } => "name and attributes differ",
            Self::Customised { payload: true,
                               name: false,
                               attributes: false } => "payload differs",
            Self::Customised { payload: false,
                               name: true,
                               attributes: false } => "name differs",
            Self::Customised { payload: false,
                               name: false,
                               attributes: true } => "attributes differ",
            _ => ""
        }
    }
}

pub struct Classification
{
    pub source:     String,
    pub kind:       String,
    pub id:         i16,
    /// The disc's own ID when it differs from the package ID (fonts).
    pub source_id:  Option<i16>,
    pub symbol:     String,
    pub name:       Option<Vec<u8>>,
    pub attributes: u8,
    pub form:       Form,
    /// A bitmap strike's size and style, as the disc's own FOND declares them,
    /// and the family name that FOND carries.
    pub size:       Option<i16>,
    pub style:      Option<u16>,
    pub family:     Option<String>,
    /// The disc file this resource was matched in, when one matched.
    pub iso_source: Option<String>,
    /// True when the disc's copy is taken by policy rather than by matching
    /// our key: fonts are imported by enumeration, not by ID lookup.
    pub taken:      bool,
    /// True when the entry came in through the generated include rather than
    /// from the manifest's own entries.
    pub generated:  bool,
    pub verdict:    Verdict
}

pub struct Report
{
    /// Sorted by `(type, id)`: the same inputs give the same report.
    pub classifications: Vec<Classification>,
    /// Resources read from the disc, across every source file.
    pub disc:            usize,
    /// ISO-side resources we do not use. Spec §7.2: ignored, never taken.
    pub ignored:         usize,
    /// Faces the disc offers that the package already carries byte for byte,
    /// described as they were found. They are not taken: the package has them.
    pub font_duplicates: Vec<String>
}

/// A3: compare every resolved resource with its disc counterpart at classic
/// payload byte level, with names and attributes part of the comparison.
///
/// The comparison is deliberately on classic bytes. A converted entry
/// (`strings`, `pstring`, `menu`) has already been encoded back to classic
/// bytes by A2; comparing its JSON text with an ISO payload would put every
/// converted resource in "keep ours".
///
/// A package can draw from several disc files, and font suitcases reuse the
/// same `(type, id)` numbers in each file. So the lookup spans every source and
/// prefers a byte-identical match; when several files carry the id and none is
/// identical, the answer is `ambiguous` rather than a guess.
fn identical(ours: &OursResource, candidate: &IsoResource) -> bool
{
    candidate.resource.data.as_slice() == ours.classic.as_deref().unwrap_or_default()
    && candidate.resource.name.as_deref() == ours.name.as_deref()
    && candidate.resource.attributes == ours.attributes
}

pub fn classify(packages: &[Package], iso: &[IsoResource]) -> Report
{
    let mut lookup: BTreeMap<(&str, i16), Vec<&IsoResource>> = BTreeMap::new();
    for entry in iso
    {
        lookup.entry((kind_str(&entry.resource.kind), entry.resource.id)).or_default().push(entry);
    }
    let mut used: BTreeSet<(&str, i16)> = BTreeSet::new();
    let mut classifications = Vec::new();
    for package in packages
    {
        for ours in &package.resources
        {
            let key = (ours.kind.as_str(), ours.id);
            let candidates = lookup.get(&key);
            if candidates.is_some()
            {
                used.insert(key);
            }
            let mut iso_source = None;
            let verdict = if ours.form == Form::NativeCode
            {
                Verdict::NativeCode
            }
            else
            {
                let all = candidates.map(Vec::as_slice).unwrap_or_default();
                let approved: Vec<&IsoResource> =
                    all.iter().copied().filter(|candidate| !candidate.held).collect();

                if approved.is_empty()
                {
                    if let Some(held) =
                        all.iter().copied().find(|candidate| identical(ours, candidate))
                    {
                        iso_source = Some(held.source.clone());
                        Verdict::Held
                    }
                    else
                    {
                        Verdict::OursOnly
                    }
                }
                else if let Some(identical) =
                    approved.iter().copied().find(|candidate| identical(ours, candidate))
                {
                    iso_source = Some(identical.source.clone());
                    Verdict::Extract
                }
                else if approved.len() == 1
                {
                    let iso = &approved[0].resource;
                    iso_source = Some(approved[0].source.clone());
                    let payload = ours.classic.as_deref() != Some(iso.data.as_slice());
                    let name = ours.name.as_deref() != iso.name.as_deref();
                    let attributes = ours.attributes != iso.attributes;
                    Verdict::Customised { payload,
                                          name,
                                          attributes }
                }
                else
                {
                    Verdict::Ambiguous
                }
            };
            classifications.push(Classification { source: ours.source.clone(),
                                                  kind: ours.kind.clone(),
                                                  id: ours.id,
                                                  source_id: None,
                                                  symbol: ours.symbol.clone(),
                                                  name: ours.name.clone(),
                                                  attributes: ours.attributes,
                                                  form: ours.form,
                                                  size: None,
                                                  style: None,
                                                  family: None,
                                                  iso_source,
                                                  taken: false,
                                                  generated: ours.generated,
                                                  verdict });
        }
    }
    classifications.sort_by(|a, b| (&a.kind, a.id).cmp(&(&b.kind, b.id)));
    let ignored =
        iso.iter()
           .filter(|entry| !used.contains(&(kind_str(&entry.resource.kind), entry.resource.id)))
           .count();
    Report { classifications,
             disc: iso.len(),
             ignored,
             font_duplicates: Vec::new() }
}

/// Import every instance of `types` the approved sources carry, regardless of
/// whether our manifest uses that key.
///
/// Fonts are the case this exists for: the disc's TrueType faces are what we
/// want, not the exact `sfnt` IDs our current tables happen to hold, and one ID
/// (`sfnt 9217`) is reused by every family that has a single face. The entry
/// keeps the disc's own ID and name; the package's numbering is a Phase B
/// decision, made when the tables are regenerated.
/// IDs the importer hands out. The 12000 block is already ours, so the
/// allocation starts at 13000; a later disc takes the next free numbers and
/// nothing can silently collide with this range.
pub const FIRST_ASSIGNED_FONT_ID: i16 = 13000;
/// The face block ends where the family-record block begins.
pub const FONT_FACE_RANGE_END: i16 = 13400;

/// Import every instance of `types` the approved sources carry, regardless of
/// whether our manifest uses that key.
///
/// Fonts are the case this exists for: the disc's TrueType faces are what we
/// want, not the exact `sfnt` IDs our current tables happen to hold, and one ID
/// (`sfnt 9217`) is reused by every family that has a single face. Because the
/// disc's numbering is per file and cannot be imported as-is, each new face gets
/// a unique ID from the range above; the disc's own ID is kept as `source_id`.
/// A face that already exists in the package with identical bytes keeps its ID.
pub fn take_all(report: &mut Report, iso: &[IsoResource], package: &Package, types: &[String],
                form: Form, suppressed_by: Option<&str>)
{
    let types: BTreeSet<&str> = types.iter().map(String::as_str).collect();
    // Outline-first: a source that carries an outline contributes its outline
    // only, so a bitmap strike that TTF supersedes is never taken. A source with
    // no outline at all contributes its strikes, because there is nothing else.
    let suppressed: BTreeSet<&str> = match suppressed_by
    {
        None => BTreeSet::new(),
        Some(kind) =>
        {
            iso.iter()
               .filter(|entry| !entry.held)
               .filter(|entry| kind_str(&entry.resource.kind) == kind)
               .map(|entry| entry.source.as_str())
               .collect()
        }
    };
    let mut symbols: BTreeSet<String> =
        report.classifications.iter().map(|entry| entry.symbol.clone()).collect();
    let mut present: BTreeSet<(String, String, i16)> =
        report.classifications
              .iter()
              .filter(|entry| entry.verdict == Verdict::Extract)
              .filter_map(|entry| {
                  entry.iso_source
                       .as_ref()
                       .map(|source| (source.clone(), entry.kind.clone(), entry.id))
              })
              .collect();
    // A face the package already carries byte for byte is not taken. Taking it
    // would add a second family answering to one name and a second copy of the
    // same face, so the resolution for a duplicate is "the package has it".
    // Comparing bytes rather than names keeps the decision about the payload,
    // not about how a family happens to be spelled.
    // Only what the package authors counts as carried. The generated include
    // holds what an earlier run of this step took, so treating it as carried
    // would make every run conclude that everything is already present and take
    // nothing at all — the list would oscillate instead of settling.
    let carried: BTreeSet<Vec<u8>> =
        package.resources
               .iter()
               .filter(|resource| !resource.generated)
               .filter(|resource| matches!(resource.kind.as_str(), "sfnt" | "NFNT" | "FONT"))
               .filter_map(|resource| resource.classic.clone())
               .collect();
    let mut candidates: Vec<&IsoResource> =
        iso.iter()
           .filter(|entry| !entry.held)
           .filter(|entry| !suppressed.contains(entry.source.as_str()))
           .filter(|entry| types.contains(kind_str(&entry.resource.kind)))
           .filter(|entry| !carried.contains(&entry.resource.data))
           .filter(|entry| {
               present.insert((entry.source.clone(),
                               kind_str(&entry.resource.kind).to_string(),
                               entry.resource.id))
           })
           .collect();
    candidates.sort_by(|a, b| (&a.source, a.resource.id).cmp(&(&b.source, b.resource.id)));
    let associations = fond_associations(iso);
    // Faces own 13000-13399 and family records own 13400 upward, so neither
    // allocation can push the other around. Only ids outside the face block are
    // reserved from the package, which is what makes the numbering a function of
    // the disc alone: a face keeps its number whatever the generated include
    // currently holds. Ids handed out by an earlier call are reserved too, so
    // outlines and bitmaps cannot collide.
    let mut used: BTreeSet<i16> =
        package.resources
               .iter()
               .map(|resource| resource.id)
               .filter(|id| !(*id >= FIRST_ASSIGNED_FONT_ID && *id < FONT_FACE_RANGE_END))
               .chain(report.classifications
                            .iter()
                            .filter(|entry| entry.taken)
                            .map(|entry| entry.id))
               .collect();
    for entry in candidates
    {
        let kind = kind_str(&entry.resource.kind).to_string();
        let association = associations.get(&(entry.source.clone(), entry.resource.id)).cloned();
        let (size, style) = association.as_ref().map(|(size, style, _)| (*size, *style)).unzip();
        let family = association.and_then(|(_, _, family)| family);
        let base =
            format!("EXTRACTED_{}_{}",
                    kind.chars()
                        .map(|c| {
                            if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' }
                        })
                        .collect::<String>(),
                    entry.resource.id.unsigned_abs());
        let mut symbol = base.clone();
        let mut suffix = 2;
        while !symbols.insert(symbol.clone())
        {
            symbol = format!("{base}_{suffix}");
            suffix += 1;
        }
        let mut id = FIRST_ASSIGNED_FONT_ID;
        while used.contains(&id)
        {
            id += 1;
        }
        used.insert(id);
        report.classifications.push(Classification { source: package.source.clone(),
                                                     kind,
                                                     id,
                                                     source_id: Some(entry.resource.id),
                                                     symbol,
                                                     name: entry.resource.name.clone(),
                                                     attributes: entry.resource.attributes,
                                                     form,
                                                     size,
                                                     style,
                                                     family,
                                                     iso_source: Some(entry.source.clone()),
                                                     taken: true,
                                                     generated: false,
                                                     verdict: Verdict::Extract });
    }
    report.classifications.sort_by(|a, b| (&a.kind, a.id).cmp(&(&b.kind, b.id)));
    // Record what was declined for being a duplicate, so the report can say why
    // a family the disc carries is absent from the list.
    for entry in iso.iter()
                    .filter(|entry| !entry.held)
                    .filter(|entry| !suppressed.contains(entry.source.as_str()))
                    .filter(|entry| types.contains(kind_str(&entry.resource.kind)))
                    .filter(|entry| carried.contains(&entry.resource.data))
    {
        let suit = entry.source.rsplit('/').next().unwrap_or_default();
        let family = associations.get(&(entry.source.clone(), entry.resource.id))
                                 .and_then(|(_, _, family)| family.clone());
        report.font_duplicates.push(format!("{} {} from {} ({})",
                                            kind_str(&entry.resource.kind),
                                            entry.resource.id,
                                            suit,
                                            family.as_deref().unwrap_or("unnamed")));
    }
    report.font_duplicates.sort();
    report.font_duplicates.dedup();
}

/// What the disc's own FOND declares for each face ID, per source file: the
/// size and style it associates the face with, and the family name it carries.
/// A bitmap strike stores no point size and no name, so the FOND is the only
/// statement of either; reading it here keeps the pairing derived from the
/// source rather than retyped by hand, where a swap would go unnoticed.
fn fond_associations(iso: &[IsoResource]) -> BTreeMap<(String, i16), (i16, u16, Option<String>)>
{
    let mut map = BTreeMap::new();
    for entry in iso.iter().filter(|entry| !entry.held && entry.resource.kind == *b"FOND")
    {
        let data = &entry.resource.data;
        let Some(count) =
            data.get(52..54).map(|bytes| i16::from_be_bytes([bytes[0], bytes[1]]) as i32 + 1)
        else
        {
            continue;
        };
        if count <= 0 || 54 + count as usize * 6 > data.len()
        {
            continue;
        }
        let family = entry.resource.name.as_deref().map(mac_formats::text::decode);
        for index in 0..count as usize
        {
            let at = 54 + index * 6;
            let size = i16::from_be_bytes([data[at], data[at + 1]]);
            let style = u16::from_be_bytes([data[at + 2], data[at + 3]]);
            let face = i16::from_be_bytes([data[at + 4], data[at + 5]]);
            map.entry((entry.source.clone(), face)).or_insert((size, style, family.clone()));
        }
    }
    map
}

/// Turn the extract verdicts into the package's checked-in extract list.
///
/// Each entry names the classic form the extractor must land the resource in.
/// Because the comparison is at classic byte level, that form is the one our
/// manifest already uses: an identical payload means our current form is a
/// faithful encoding of the disc's bytes.
pub fn extract_list(description: &str, iso: &str, report: &Report, source_order: &[String],
                    exclude: &BTreeSet<(String, i16)>)
                    -> ExtractList
{
    let mut sources: Vec<ExtractSource> = source_order.iter()
                                                      .map(|path| {
                                                          ExtractSource { path:      path.clone(),
                                                                          resources: Vec::new() }
                                                      })
                                                      .collect();
    for entry in report.classifications
                       .iter()
                       .filter(|entry| entry.verdict == Verdict::Extract)
                       .filter(|entry| !exclude.contains(&(entry.kind.clone(), entry.id)))
    {
        let path = entry.iso_source.as_deref().unwrap_or_default();
        let Some(section) = sources.iter_mut().find(|section| section.path == path)
        else
        {
            continue;
        };
        section.resources.push(ExtractEntry { kind:       entry.kind.clone(),
                                              id:         entry.id,
                                              source_id:  entry.source_id,
                                              symbol:     entry.symbol.clone(),
                                              name:       entry.name
                                                               .as_deref()
                                                               .map(mac_formats::text::decode),
                                              attributes: entry.attributes,
                                              form:       entry.form.as_str().to_string(),
                                              taken:      entry.taken,
                                              size:       entry.size,
                                              style:      entry.style,
                                              family:     entry.family.clone() });
    }
    sources.retain(|section| !section.resources.is_empty());
    ExtractList { description: description.to_string(),
                  iso: iso.to_string(),
                  sources }
}

/// The resource type as the string `resources.json` uses, for report keys.
fn kind_str(kind: &[u8; 4]) -> &str
{
    // Every type this tool reads is four ASCII bytes; a malformed one would
    // have been refused when the fork was decoded.
    core::str::from_utf8(kind).unwrap_or("????")
}

/// The human report beside the extract list: what we take, what we keep and
/// why, counted overall and per type. Sorted, so two runs read the same.
pub fn human_report(description: &str, iso: &str, sources: &[String], package: &Package,
                    report: &Report, usage: &Usage)
                    -> String
{
    use std::fmt::Write as _;
    let mut text = String::new();
    let _ = writeln!(text, "{description}");
    let _ = writeln!(text, "disc:    {iso}");
    for source in sources
    {
        let _ = writeln!(text, "source:  {source}");
    }
    let _ = writeln!(text, "package: {} ({})", package.source, package.path.display());
    let extract = count(report, |verdict| verdict == Verdict::Extract);
    let customised = count(report, |verdict| matches!(verdict, Verdict::Customised { .. }));
    let ours_only = count(report, |verdict| verdict == Verdict::OursOnly);
    let ambiguous = count(report, |verdict| verdict == Verdict::Ambiguous);
    let held = count(report, |verdict| verdict == Verdict::Held);
    let native = count(report, |verdict| verdict == Verdict::NativeCode);
    let _ = writeln!(text,
                     "ours: {} resources — extract {extract}, keep ours {customised} customised, \
                      {ours_only} ours-only, {ambiguous} ambiguous, {held} held, {native} \
                      native-code",
                     package.resources.len() + package.unresolvable.len());
    let _ = writeln!(text,
                     "disc: {} resources, {} whose (type, id) we do not name",
                     report.disc, report.ignored);
    let _ = writeln!(text, "\nby type");
    let _ = writeln!(text,
                     "  {:<5} {:>5} {:>8} {:>11} {:>10} {:>10} {:>6} {:>7}",
                     "type",
                     "ours",
                     "extract",
                     "customised",
                     "ours-only",
                     "ambiguous",
                     "held",
                     "native");
    for (kind, row) in types(report)
    {
        let _ = writeln!(text,
                         "  {:<5} {:>5} {:>8} {:>11} {:>10} {:>10} {:>6} {:>7}",
                         kind,
                         row.ours,
                         row.extract,
                         row.customised,
                         row.ours_only,
                         row.ambiguous,
                         row.held,
                         row.native);
    }
    let _ = writeln!(text, "\nkeep ours");
    for entry in &report.classifications
    {
        if matches!(entry.verdict, Verdict::Extract | Verdict::NativeCode | Verdict::Held)
        {
            continue;
        }
        let detail = entry.verdict.detail();
        let _ = writeln!(text,
                         "  {:<11} {:<5} {:>7}  {:<28} {}  ({detail})",
                         entry.verdict.reason(),
                         entry.kind,
                         entry.id,
                         entry.name.as_deref().map_or_else(String::new, mac_formats::text::decode),
                         entry.symbol);
    }
    let _ = writeln!(text, "\nheld out of the list");
    let mut any_held = false;
    for entry in &report.classifications
    {
        if entry.verdict != Verdict::Held
        {
            continue;
        }
        any_held = true;
        let _ = writeln!(text,
                         "  {:<5} {:>7}  {:<28} {}  (identical to {})",
                         entry.kind,
                         entry.id,
                         entry.name.as_deref().map_or_else(String::new, mac_formats::text::decode),
                         entry.symbol,
                         entry.iso_source.as_deref().unwrap_or("an unapproved source"));
    }
    if !any_held
    {
        let _ = writeln!(text, "  none");
    }
    let _ = writeln!(text, "\nambiguous — kept as ours, not extracted");
    let mut any_ambiguous = false;
    for entry in &report.classifications
    {
        if entry.verdict != Verdict::Ambiguous
        {
            continue;
        }
        any_ambiguous = true;
        let _ = writeln!(text,
                         "  {:<5} {:>7}  {:<28} {}",
                         entry.kind,
                         entry.id,
                         entry.name.as_deref().map_or_else(String::new, mac_formats::text::decode),
                         entry.symbol);
    }
    if !any_ambiguous
    {
        let _ = writeln!(text, "  none");
    }
    else
    {
        let _ = writeln!(text,
                         "  reason: more than one disc file carries this (type, id) and none is \
                          byte-identical, so the tool cannot say which file the resource came \
                          from. They stay ours until a ruling; the usage section below says \
                          whether anything names them.");
    }
    write_fonts(&mut text, report);
    write_usage(&mut text, report, usage, package);
    text
}

/// Resources that another resource of ours points at: an ALRT names its DITL,
/// and our standard-file dialogs read the DITL with the DLOG's own id. Neither
/// is a code reference, but deleting one would break the other.
fn reachable(package: &Package, report: &Report) -> BTreeMap<(String, i16), String>
{
    let payload = |kind: &str, id: i16| {
        package.resources
               .iter()
               .find(|resource| resource.kind == kind && resource.id == id)
               .and_then(|resource| resource.classic.as_deref())
    };
    let mut reachable = BTreeMap::new();
    for entry in report.classifications.iter().filter(|entry| entry.verdict == Verdict::Extract)
    {
        match entry.kind.as_str()
        {
            "ALRT" =>
            {
                if let Some(data) = payload("ALRT", entry.id)
                   && data.len() >= 10
                {
                    let items = i16::from_be_bytes([data[8], data[9]]);
                    reachable.entry(("DITL".to_string(), items))
                             .or_insert_with(|| format!("named by ALRT {}", entry.id));
                }
            }
            "DLOG" =>
            {
                reachable.entry(("DITL".to_string(), entry.id))
                         .or_insert_with(|| format!("read with DLOG {}", entry.id));
            }
            _ =>
            {}
        }
    }
    reachable
}

/// The four dispositions of an extracted resource, for the report and the
/// console summary alike.
pub struct UsageCounts
{
    pub extracted:     usize,
    pub named:         usize,
    pub reachable:     usize,
    pub unproven:      usize,
    pub unmatched:     usize,
    pub tests:         usize,
    pub reachable_why: BTreeMap<(String, i16), String>
}

pub fn usage_counts(report: &Report, usage: &Usage, package: &Package) -> UsageCounts
{
    let reachable = reachable(package, report);
    let extracted = usage_extracted(report);
    let named = |entry: &Classification| !usage.of(&entry.kind, entry.id).is_empty();
    let implied = |entry: &Classification| reachable.contains_key(&(entry.kind.clone(), entry.id));
    let unproven = |entry: &Classification| {
        !named(entry) && !implied(entry) && usage.dynamic_types.contains(&entry.kind)
    };
    let named_count = extracted.iter().filter(|entry| named(entry)).count();
    let implied_count = extracted.iter().filter(|entry| !named(entry) && implied(entry)).count();
    let unproven_count = extracted.iter().filter(|entry| unproven(entry)).count();
    let tests = extracted.iter()
                         .filter(|entry| {
                             let hits = usage.of(&entry.kind, entry.id);
                             !hits.is_empty() && hits.iter().all(|hit| hit.test)
                         })
                         .count();
    UsageCounts { extracted: extracted.len(),
                  named: named_count,
                  reachable: implied_count,
                  unproven: unproven_count,
                  unmatched: extracted.len() - named_count - implied_count - unproven_count,
                  tests,
                  reachable_why: reachable }
}

fn usage_extracted(report: &Report) -> Vec<&Classification>
{
    report.classifications.iter().filter(|entry| entry.verdict == Verdict::Extract).collect()
}

/// Extracted resources the static scan could not name at all, and that no
/// extracted resource reaches: the entries the ruling excludes from the list.
pub fn unreferenced(report: &Report, usage: &Usage, package: &Package) -> BTreeSet<(String, i16)>
{
    let reachable = reachable(package, report);
    report.classifications
          .iter()
          .filter(|entry| entry.verdict == Verdict::Extract)
          // A face taken by policy has no code reference by construction: the
          // font catalog reaches it through its FOND association, which this
          // scan cannot see. Excluding it here would drop the very resources
          // the font ruling exists to import.
          .filter(|entry| !entry.taken)
          .filter(|entry| usage.of(&entry.kind, entry.id).is_empty())
          .filter(|entry| !reachable.contains_key(&(entry.kind.clone(), entry.id)))
          .filter(|entry| !usage.dynamic_types.contains(&entry.kind))
          .map(|entry| (entry.kind.clone(), entry.id))
          .collect()
}

/// The font section: what the disc carries as outlines, what it does not carry
/// that we hold, and the numbering problem the ruling leaves to Phase B.
fn write_fonts(text: &mut String, report: &Report)
{
    use std::fmt::Write as _;
    let taken: Vec<&Classification> =
        report.classifications.iter().filter(|entry| entry.taken).collect();
    if taken.is_empty()
    {
        return;
    }
    let mut families: BTreeMap<String, Vec<&Classification>> = BTreeMap::new();
    for entry in &taken
    {
        let family = entry.iso_source
                          .as_deref()
                          .and_then(|source| source.rsplit('/').next())
                          .unwrap_or("-")
                          .to_string();
        families.entry(family).or_default().push(entry);
    }
    let _ = writeln!(text, "\nfonts — outlines imported by enumeration, not by ID lookup");
    let _ = writeln!(text, "  taken: {} faces from {} families", taken.len(), families.len());
    if !report.font_duplicates.is_empty()
    {
        let _ = writeln!(text,
                         "  not taken: {} faces the package already carries byte for byte — a \
                          duplicate would be a second family answering to one name and a second \
                          copy of the same face, so the family stays the one we have",
                         report.font_duplicates.len());
        for duplicate in &report.font_duplicates
        {
            let _ = writeln!(text, "    {duplicate}");
        }
    }
    for (family, faces) in &families
    {
        let ids: Vec<String> = faces.iter().map(|face| face.id.to_string()).collect();
        let names: Vec<String> =
            faces.iter()
                 .map(|face| {
                     face.name.as_deref().map_or_else(String::new, mac_formats::text::decode)
                 })
                 .collect();
        let kinds: Vec<&str> = faces.iter()
                                    .map(|face| face.kind.as_str())
                                    .collect::<BTreeSet<_>>()
                                    .into_iter()
                                    .collect();
        let _ = writeln!(text,
                         "    {:<26} {} face(s)  {} {}  [{}]",
                         family,
                         faces.len(),
                         kinds.join("/"),
                         ids.join(", "),
                         names.join(", "));
    }
    let mut by_id: BTreeMap<i16, Vec<&str>> = BTreeMap::new();
    for (family, faces) in &families
    {
        for face in faces
        {
            by_id.entry(face.source_id.unwrap_or(face.id)).or_default().push(family.as_str());
        }
    }
    let shared: Vec<String> = by_id.iter()
                                   .filter(|(_, users)| users.len() > 1)
                                   .map(|(id, users)| format!("{id} ({})", users.join(", ")))
                                   .collect();
    if shared.is_empty()
    {
        let _ = writeln!(text,
                         "  numbering: every imported face is assigned a unique ID from {} \
                          upward, with the disc's own ID kept beside it as source_id.",
                         FIRST_ASSIGNED_FONT_ID);
    }
    else
    {
        let _ = writeln!(text,
                         "  numbering: the disc reuses IDs across families — {}, so its \
                          numbering is per file and cannot be imported as-is. Every imported \
                          face is assigned a unique ID from {} upward, with the disc's own ID \
                          kept beside it as source_id.",
                         shared.join("; "),
                         FIRST_ASSIGNED_FONT_ID);
    }
    let _ = writeln!(text,
                     "  bitmaps: where a family has an outline on the disc, the outline is taken \
                      and its strikes are not; a family with no outline is taken as strikes.");
    let absent_nfnt =
        report.classifications
              .iter()
              .filter(|entry| {
                  entry.kind == "NFNT" && entry.verdict == Verdict::OursOnly && !entry.generated
              })
              .count();
    let absent_font =
        report.classifications
              .iter()
              .filter(|entry| entry.kind == "FONT" && entry.verdict == Verdict::OursOnly)
              .count();
    let _ = writeln!(text,
                     "  our bitmaps: {absent_nfnt} NFNT and {absent_font} FONT have no disc \
                      counterpart by ID — bitmap absent, outline present where the family is \
                      imported. They stay ours; nothing is dropped from the manifest.");
    let fondonly: Vec<&Classification> =
        report.classifications.iter().filter(|entry| entry.kind == "FOND").collect();
    if !fondonly.is_empty()
    {
        let _ = writeln!(text,
                         "  FOND: {} family records, all ours and none extracted. The nine \
                          bundled ones stay exactly as they are: their associations name the IDs \
                          we already install, and a face imported from the disc is the same \
                          bytes as the one they name. The imported families get a record \
                          authored by tools/mac-fonts from this list.",
                         fondonly.len());
        for entry in fondonly
        {
            let _ =
                writeln!(text,
                         "    FOND {:>3}  {:<12} {}  ({})",
                         entry.id,
                         entry.name.as_deref().map_or_else(String::new, mac_formats::text::decode),
                         entry.symbol,
                         entry.verdict.reason());
        }
    }
}

fn write_usage(text: &mut String, report: &Report, usage: &Usage, package: &Package)
{
    use std::fmt::Write as _;
    let counts = usage_counts(report, usage, package);
    let reachable = counts.reachable_why.clone();
    let extracted = usage_extracted(report);
    let named = |entry: &Classification| !usage.of(&entry.kind, entry.id).is_empty();
    let implied = |entry: &Classification| reachable.contains_key(&(entry.kind.clone(), entry.id));
    let unproven = |entry: &Classification| {
        !named(entry) && !implied(entry) && usage.dynamic_types.contains(&entry.kind)
    };
    let _ = writeln!(text, "\nusage — static scan of {} Rust source files", usage.files);
    let _ = writeln!(text, "  extracted {}", counts.extracted);
    let _ = writeln!(text, "    referenced by name, literal or symbol    {}", counts.named);
    let _ = writeln!(text, "    reachable from another extracted resource {}", counts.reachable);
    let _ = writeln!(text,
                     "    cannot be ruled out (type looked up with a computed id) {}",
                     counts.unproven);
    let _ = writeln!(text, "    no reference found at all                 {}", counts.unmatched);
    let _ = writeln!(text, "  of the referenced, {} are named only by test code", counts.tests);
    let _ = writeln!(text,
                     "  excluded from the extract list on this basis: {} (listed below; they \
                      stay in resources.json and are not extracted at all)",
                     unreferenced(report, usage, package).len());
    let ambiguous = report.classifications
                          .iter()
                          .filter(|entry| entry.verdict == Verdict::Ambiguous)
                          .collect::<Vec<_>>();
    let _ = writeln!(text,
                     "  ambiguous {} — referenced {}, no reference found {}",
                     ambiguous.len(),
                     ambiguous.iter().filter(|entry| named(entry)).count(),
                     ambiguous.iter().filter(|entry| !named(entry)).count());
    let _ = writeln!(text,
                     "  how: a resources.json symbol, a (type, id) literal in the same \
                      expression (or, where the id is computed, an id literal in the same file), \
                      or a quoted resource name, anywhere in the repository's Rust sources. \
                      Tests are included and counted separately.");
    let _ = writeln!(text,
                     "  not searchable this way: ids computed at runtime, ids read from data or \
                      from another resource, names built at runtime, classic software asking for \
                      a resource by number, and anything outside this repository. A type that is \
                      ever looked up with a computed id is therefore reported as 'cannot be \
                      ruled out', never as unused. 'No reference found' is not proof of disuse \
                      and must not be treated as one.");
    let _ = writeln!(text, "\n  sample references");
    let mut shown = 0;
    for entry in extracted.iter().filter(|entry| named(entry))
    {
        if shown == 12
        {
            break;
        }
        shown += 1;
        let hits = usage.of(&entry.kind, entry.id);
        let hit = hits.iter().find(|hit| !hit.test).unwrap_or(&hits[0]);
        let _ = writeln!(text,
                         "    {:<5} {:>7}  {:<28} {}:{}  ({})",
                         entry.kind, entry.id, entry.symbol, hit.path, hit.line, hit.how);
    }
    let _ = writeln!(text, "\n  reachable from another extracted resource");
    for entry in extracted.iter().filter(|entry| !named(entry) && implied(entry)).take(8)
    {
        let why = &reachable[&(entry.kind.clone(), entry.id)];
        let _ =
            writeln!(text, "    {:<5} {:>7}  {:<28} {}", entry.kind, entry.id, entry.symbol, why);
    }
    let _ = writeln!(text, "\n  no reference found at all (review, do not delete on this alone)");
    let mut listed = 0;
    for entry in
        extracted.iter().filter(|entry| !named(entry) && !implied(entry) && !unproven(entry))
    {
        listed += 1;
        let _ = writeln!(text,
                         "    {:<5} {:>7}  {:<28} {}",
                         entry.kind,
                         entry.id,
                         entry.name.as_deref().map_or_else(String::new, mac_formats::text::decode),
                         entry.symbol);
    }
    if listed == 0
    {
        let _ = writeln!(text, "    none");
    }
    let _ = writeln!(text, "\n  cannot be ruled out, by type (count of extracted resources)");
    let mut by_type: BTreeMap<&str, usize> = BTreeMap::new();
    for entry in extracted.iter().filter(|entry| unproven(entry))
    {
        *by_type.entry(entry.kind.as_str()).or_default() += 1;
    }
    for (kind, count) in by_type
    {
        let _ = writeln!(text, "    {:<5} {count}", kind);
    }
}


fn count(report: &Report, predicate: impl Fn(Verdict) -> bool) -> usize
{
    report.classifications.iter().filter(|entry| predicate(entry.verdict)).count()
}

struct TypeCount
{
    ours:       usize,
    extract:    usize,
    customised: usize,
    ours_only:  usize,
    ambiguous:  usize,
    held:       usize,
    native:     usize
}

fn types(report: &Report) -> BTreeMap<String, TypeCount>
{
    let mut rows: BTreeMap<String, TypeCount> = BTreeMap::new();
    for entry in &report.classifications
    {
        let row = rows.entry(entry.kind.clone()).or_insert(TypeCount { ours:       0,
                                                                       extract:    0,
                                                                       customised: 0,
                                                                       ours_only:  0,
                                                                       ambiguous:  0,
                                                                       held:       0,
                                                                       native:     0 });
        row.ours += 1;
        match entry.verdict
        {
            Verdict::Extract => row.extract += 1,
            Verdict::Customised { .. } => row.customised += 1,
            Verdict::OursOnly => row.ours_only += 1,
            Verdict::Ambiguous => row.ambiguous += 1,
            Verdict::Held => row.held += 1,
            Verdict::NativeCode => row.native += 1
        }
    }
    rows
}


/// Read a classic resource fork as a stand-in for a disc source file, for the
/// classifier's tests. Uses the existing resource stream codec: no second
/// parser.
pub fn read_fork(source: &str, path: &Path) -> Result<Vec<IsoResource>>
{
    let bytes = fs::read(path)?;
    let resources = mac_formats::resource::decode(&bytes).map_err(|cause| {
                                                             error(format!("cannot read classic \
                                                                            resource fork {}: \
                                                                            {cause}",
                                                                           path.display()))
                                                         })?;
    Ok(resources.into_iter()
                .map(|resource| {
                    IsoResource { source: source.to_string(),
                                  held: false,
                                  resource }
                })
                .collect())
}


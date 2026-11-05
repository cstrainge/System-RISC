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

pub mod iso;

/// One extract-list entry: what the extractor takes, and the classic form it
/// must land in. Metadata only — no payload bytes, so the list is safe to
/// check in and is the reviewed statement of what we take from Apple.
#[derive(Serialize)]
pub struct ExtractEntry
{
    #[serde(rename = "type")]
    pub kind:       String,
    pub id:         i16,
    pub name:       Option<String>,
    pub attributes: u8,
    pub form:       String
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
    pub classic:    Option<Vec<u8>>
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
        resources.push(OursResource { source: source.to_string(),
                                      kind: definition.kind.clone(),
                                      id: definition.id,
                                      symbol: definition.symbol.clone(),
                                      name,
                                      attributes: definition.attributes,
                                      form,
                                      classic });
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
    pub symbol:     String,
    pub name:       Option<Vec<u8>>,
    pub attributes: u8,
    pub form:       Form,
    /// The disc file this resource was matched in, when one matched.
    pub iso_source: Option<String>,
    pub verdict:    Verdict
}

pub struct Report
{
    /// Sorted by `(type, id)`: the same inputs give the same report.
    pub classifications: Vec<Classification>,
    /// Resources read from the disc, across every source file.
    pub disc:            usize,
    /// ISO-side resources we do not use. Spec §7.2: ignored, never taken.
    pub ignored:         usize
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
                match candidates.map(Vec::as_slice)
                {
                    None | Some([]) => Verdict::OursOnly,
                    Some(candidates) =>
                    {
                        if let Some(identical) =
                            candidates.iter().find(|candidate| {
                                                 candidate.resource.data.as_slice()
                                                 == ours.classic.as_deref().unwrap_or_default()
                                                 && candidate.resource.name.as_deref()
                                                    == ours.name.as_deref()
                                                 && candidate.resource.attributes == ours.attributes
                                             })
                        {
                            iso_source = Some(identical.source.clone());
                            Verdict::Extract
                        }
                        else if candidates.len() == 1
                        {
                            let iso = &candidates[0].resource;
                            iso_source = Some(candidates[0].source.clone());
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
                    }
                }
            };
            classifications.push(Classification { source: ours.source.clone(),
                                                  kind: ours.kind.clone(),
                                                  id: ours.id,
                                                  symbol: ours.symbol.clone(),
                                                  name: ours.name.clone(),
                                                  attributes: ours.attributes,
                                                  form: ours.form,
                                                  iso_source,
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
             ignored }
}

/// Turn the extract verdicts into the package's checked-in extract list.
///
/// Each entry names the classic form the extractor must land the resource in.
/// Because the comparison is at classic byte level, that form is the one our
/// manifest already uses: an identical payload means our current form is a
/// faithful encoding of the disc's bytes.
pub fn extract_list(description: &str, iso: &str, report: &Report, source_order: &[String])
                    -> ExtractList
{
    let mut sources: Vec<ExtractSource> = source_order.iter()
                                                      .map(|path| {
                                                          ExtractSource { path:      path.clone(),
                                                                          resources: Vec::new() }
                                                      })
                                                      .collect();
    for entry in report.classifications.iter().filter(|entry| entry.verdict == Verdict::Extract)
    {
        let path = entry.iso_source.as_deref().unwrap_or_default();
        let Some(section) = sources.iter_mut().find(|section| section.path == path)
        else
        {
            continue;
        };
        section.resources.push(ExtractEntry { kind:       entry.kind.clone(),
                                              id:         entry.id,
                                              name:       entry.name
                                                               .as_deref()
                                                               .map(mac_formats::text::decode),
                                              attributes: entry.attributes,
                                              form:       entry.form.as_str().to_string() });
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
                    report: &Report)
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
    let native = count(report, |verdict| verdict == Verdict::NativeCode);
    let _ = writeln!(text,
                     "ours: {} resources — extract {extract}, keep ours {customised} customised, \
                      {ours_only} ours-only, {ambiguous} ambiguous, {native} native-code",
                     package.resources.len() + package.unresolvable.len());
    let _ = writeln!(text,
                     "disc: {} resources, {} whose (type, id) we do not name",
                     report.disc, report.ignored);
    let _ = writeln!(text, "\nby type");
    let _ = writeln!(text,
                     "  {:<5} {:>5} {:>8} {:>11} {:>10} {:>10} {:>7}",
                     "type", "ours", "extract", "customised", "ours-only", "ambiguous", "native");
    for (kind, row) in types(report)
    {
        let _ = writeln!(text,
                         "  {:<5} {:>5} {:>8} {:>11} {:>10} {:>10} {:>7}",
                         kind,
                         row.ours,
                         row.extract,
                         row.customised,
                         row.ours_only,
                         row.ambiguous,
                         row.native);
    }
    let _ = writeln!(text, "\nkeep ours");
    for entry in &report.classifications
    {
        if entry.verdict == Verdict::Extract || entry.verdict == Verdict::NativeCode
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
    text
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
                                                                       native:     0 });
        row.ours += 1;
        match entry.verdict
        {
            Verdict::Extract => row.extract += 1,
            Verdict::Customised { .. } => row.customised += 1,
            Verdict::OursOnly => row.ours_only += 1,
            Verdict::Ambiguous => row.ambiguous += 1,
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
                                  resource }
                })
                .collect())
}

#[cfg(test)]
mod tests
{
    use std::{fs,
              sync::atomic::{AtomicUsize, Ordering}};

    use mac_formats::resource::Resource;

    use super::*;

    static FIXTURES: AtomicUsize = AtomicUsize::new(0);

    /// A throwaway package, so manifests go through the real reader and the
    /// real payload encoder rather than a test-only path.
    fn package(resources: &str) -> PathBuf
    {
        let root = std::env::temp_dir().join(format!("extract-list-{}-{}",
                                                     std::process::id(),
                                                     FIXTURES.fetch_add(1, Ordering::Relaxed)));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();
        fs::write(
                  root.join("Cargo.toml"),
                  r#"[package]
name = "extract-list-fixture"
version = "1.2.3"
edition = "2024"
"#
        ).unwrap();
        fs::write(root.join("resources.json"),
                  format!("{{\"name\":\"Fixture\",\"file_type\":\"APPL\",\"creator\":\"TEST\",\"\
                           role\":\"application\",\"abi_version\":1,\"description\":\"fixture\",\
                           \"resources\":{resources}}}")).unwrap();
        root
    }

    fn read(resources: &str) -> (PathBuf, Package)
    {
        let root = package(resources);
        let package = read_package("system", &root.join("resources.json")).unwrap();
        (root, package)
    }

    fn iso(source: &str, kind: &[u8; 4], id: i16, name: Option<&[u8]>, attributes: u8,
           data: Vec<u8>)
           -> IsoResource
    {
        IsoResource { source:   source.to_string(),
                      resource: Resource { kind: *kind,
                                           id,
                                           name: name.map(<[u8]>::to_vec),
                                           attributes,
                                           data } }
    }

    #[test]
    fn converted_entries_compare_at_classic_bytes_not_json_text()
    {
        // The one sharp decision in §7.2: these three entries are JSON in our
        // manifest and classic bytes on the ISO side. Comparing the JSON text
        // would call all of them "keep ours".
        let (root, package) = read(
                                   r#"[
            {"type":"STR ","id":128,"symbol":"HELLO","payload":{"format":"pstring","text":"Hello"}},
            {"type":"STR#","id":129,"symbol":"LIST","payload":{"format":"strings","strings":["One","Two"]}},
            {"type":"MENU","id":130,"symbol":"MENU","payload":{"format":"menu","title":"File","items":["Quit"]}}
        ]"#
        );
        assert!(package.unresolvable.is_empty(), "{}", package.unresolvable.len());
        let iso = vec![iso("system", b"STR ", 128, None, 0, b"\x05Hello".to_vec()),
                       iso("system", b"STR#", 129, None, 0, b"\x00\x02\x03One\x03Two".to_vec()),
                       iso("system", b"MENU", 130, None, 0, {
                           let mut bytes = 130i16.to_be_bytes().to_vec();
                           bytes.extend_from_slice(&[0; 6]);
                           bytes.extend_from_slice(&0b11u32.to_be_bytes());
                           bytes.extend_from_slice(b"\x04File");
                           bytes.extend_from_slice(b"\x04Quit\0\0\0\0");
                           bytes.push(0);
                           bytes
                       })];
        let report = classify(&[package], &iso);
        assert_eq!(report.classifications.iter().map(|entry| entry.verdict).collect::<Vec<_>>(),
                   vec![Verdict::Extract; 3],
                   "converted forms must classify as identical");
        assert_eq!(report.ignored, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn payload_name_and_attribute_differences_all_keep_ours()
    {
        let (root, package) = read(
                                   r#"[
            {"type":"ICON","id":128,"symbol":"SAME","payload":{"format":"hex","hex":"0102"}},
            {"type":"ICON","id":129,"symbol":"PAYLOAD","payload":{"format":"hex","hex":"0102"}},
            {"type":"ICON","id":130,"symbol":"NAME","name":"Mine","payload":{"format":"hex","hex":"0102"}},
            {"type":"ICON","id":131,"symbol":"ATTRIBUTES","attributes":32,"payload":{"format":"hex","hex":"0102"}},
            {"type":"ICON","id":132,"symbol":"OURS_ONLY","payload":{"format":"hex","hex":"0102"}}
        ]"#
        );
        let iso = vec![iso("system", b"ICON", 128, None, 0, vec![1, 2]),
                       iso("system", b"ICON", 129, None, 0, vec![3, 4]),
                       iso("system", b"ICON", 130, None, 0, vec![1, 2]),
                       iso("system", b"ICON", 131, None, 0, vec![1, 2]),
                       // A resource we do not use: ignored, never taken.
                       iso("system", b"ICON", 200, None, 0, vec![9])];
        let report = classify(&[package], &iso);
        let verdicts: Vec<Verdict> =
            report.classifications.iter().map(|entry| entry.verdict).collect();
        assert_eq!(verdicts,
                   vec![Verdict::Extract,
                        Verdict::Customised { payload:    true,
                                              name:       false,
                                              attributes: false },
                        Verdict::Customised { payload:    false,
                                              name:       true,
                                              attributes: false },
                        Verdict::Customised { payload:    false,
                                              name:       false,
                                              attributes: true },
                        Verdict::OursOnly]);
        assert_eq!(report.ignored, 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_code_is_never_compared_or_extracted()
    {
        // The ISO has a WDEF 0, but ours is our own native code by definition.
        let (root, package) = read(
                                   r#"[
            {"type":"WDEF","id":0,"symbol":"STANDARD","payload":{"format":"native_code","artifact":"standard"}}
        ]"#
        );
        let iso = vec![iso("system", b"WDEF", 0, None, 0, vec![1, 2, 3])];
        let report = classify(&[package], &iso);
        assert_eq!(report.classifications[0].verdict, Verdict::NativeCode);
        assert_eq!(report.ignored, 0, "the counterpart is not counted as an ignored ISO resource");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_report_is_deterministic_and_ordered()
    {
        let (root, package) = read(
                                   r#"[
            {"type":"STR ","id":130,"symbol":"C","payload":{"format":"hex","hex":"43"}},
            {"type":"ALRT","id":128,"symbol":"B","payload":{"format":"hex","hex":"42"}},
            {"type":"ALRT","id":127,"symbol":"A","payload":{"format":"hex","hex":"41"}}
        ]"#
        );
        let iso = vec![iso("system", b"ALRT", 127, None, 0, vec![0x41])];
        let first = classify(std::slice::from_ref(&package), &iso);
        let second = classify(std::slice::from_ref(&package), &iso);
        let order: Vec<(String, i16)> =
            first.classifications.iter().map(|entry| (entry.kind.clone(), entry.id)).collect();
        assert_eq!(order,
                   vec![("ALRT".to_string(), 127),
                        ("ALRT".to_string(), 128),
                        ("STR ".to_string(), 130)]);
        assert_eq!(first.classifications.iter().map(|e| e.verdict).collect::<Vec<_>>(),
                   second.classifications.iter().map(|e| e.verdict).collect::<Vec<_>>());
        assert_eq!(first.ignored, second.ignored);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn several_disc_files_can_supply_one_package_and_collisions_are_not_guessed()
    {
        let (root, package) = read(
                                   r#"[
            {"type":"ICON","id":128,"symbol":"SHARED","payload":{"format":"hex","hex":"0102"}},
            {"type":"ICON","id":129,"symbol":"ONE_FILE","payload":{"format":"hex","hex":"0909"}},
            {"type":"ICON","id":130,"symbol":"CHANGED","payload":{"format":"hex","hex":"0707"}},
            {"type":"ICON","id":131,"symbol":"COLLISION","payload":{"format":"hex","hex":"0505"}}
        ]"#
        );
        let iso = vec![iso("a", b"ICON", 128, None, 0, vec![1, 2]),
                       iso("a", b"ICON", 129, None, 0, vec![1, 1]),
                       iso("a", b"ICON", 130, None, 0, vec![8, 8]),
                       iso("a", b"ICON", 131, None, 0, vec![1, 1]),
                       iso("b", b"ICON", 128, None, 0, vec![1, 2]),
                       iso("b", b"ICON", 131, None, 0, vec![2, 2]),];
        let report = classify(&[package], &iso);
        let verdicts: Vec<Verdict> =
            report.classifications.iter().map(|entry| entry.verdict).collect();
        assert_eq!(verdicts,
                   vec![Verdict::Extract,
                        Verdict::Customised { payload:    true,
                                              name:       false,
                                              attributes: false },
                        Verdict::Customised { payload:    true,
                                              name:       false,
                                              attributes: false },
                        Verdict::Ambiguous],
                   "a byte-identical match wins, one candidate compares, many candidates without \
                    a match never guess");
        assert_eq!(report.classifications[0].iso_source.as_deref(),
                   Some("a"),
                   "an identical match records the file it was found in");
        assert_eq!(report.classifications[2].iso_source.as_deref(), Some("a"));
        assert_eq!(report.classifications[3].iso_source, None);

        let list = extract_list("fixture disc",
                                "fixture.iso",
                                &report,
                                &["a".to_string(), "b".to_string()]);
        assert_eq!(list.sources.len(), 1, "only files that contribute an entry are listed");
        assert_eq!(list.sources[0].path, "a");
        assert_eq!(list.sources[0].resources.len(), 1);
        assert_eq!(list.sources[0].resources[0].id, 128);
        assert_eq!(list.sources[0].resources[0].form, "hex");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_classic_fork_reads_as_the_iso_side()
    {
        let root = package(r#"[{"type":"ICON","id":128,"symbol":"SAME","payload":{"format":"hex","hex":"0102"}}]"#);
        let fork = mac_formats::resource::encode(&[Resource { kind:       *b"ICON",
                                                              id:         128,
                                                              name:       None,
                                                              attributes: 0,
                                                              data:       vec![1, 2] }]).unwrap();
        fs::write(root.join("fork.rsrc"), fork).unwrap();
        let iso = read_fork("system", &root.join("fork.rsrc")).unwrap();
        let package = read_package("system", &root.join("resources.json")).unwrap();
        let report = classify(&[package], &iso);
        assert_eq!(report.classifications[0].verdict, Verdict::Extract);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_file_payload_resolves_to_the_asset_bytes()
    {
        let root = package(r#"[{"type":"PICT","id":128,"symbol":"PIC","payload":{"format":"file","path":"assets/pic.bin"}}]"#);
        fs::create_dir_all(root.join("assets")).unwrap();
        fs::write(root.join("assets/pic.bin"), [0x12, 0x34]).unwrap();
        let package = read_package("system", &root.join("resources.json")).unwrap();
        assert_eq!(package.resources[0].classic.as_deref(), Some(&[0x12, 0x34][..]));
        assert_eq!(package.resources[0].form, Form::File);
        fs::remove_dir_all(root).unwrap();
    }
}

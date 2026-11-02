use alloc::{string::String, sync::Arc, vec::Vec};

use mac_abi::{BAD_ARGUMENT, CORRUPT_DATA, NO_MEMORY, NOT_SUPPORTED, Status};
use mac_formats::resource::Resource;

use super::{Association, BOLD, FaceValidity, FontFamily, FontManager, ITALIC, catalog_name, word};

pub const MAX_INSTALLED_SOURCES: usize = 128;
pub const MAX_INSTALLED_SOURCE_BYTES: usize = 8 * 1024 * 1024;
const MAX_SOURCES: usize = MAX_INSTALLED_SOURCES;
const MAX_RETAINED_SOURCES: usize = 256;
const MAX_SOURCE_BYTES: usize = MAX_INSTALLED_SOURCE_BYTES;
const MAX_RETAINED_BYTES: usize = 64 * 1024 * 1024;
const MAX_RESOURCES: usize = 1024;
const MAX_FAMILIES: usize = 4096;
const MAX_ASSOCIATIONS: usize = 4096;
const MAX_GLYPH_RECORDS: usize = 262144;
const MAX_NAME_BYTES: usize = 1024;
const MAX_TABLES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FontSourceKey
{
    pub volume: u64,
    pub cnid:   u32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontOrigin
{
    Bundled,
    Installed
    {
        source:   FontSourceKey,
        revision: u64,
        fond_id:  Option<i16>
    }
}

#[derive(Clone, Copy)]
pub enum InstalledFontData<'a>
{
    /// Already decoded, source-local resource fork. Non-font resources are inert.
    Suitcase(&'a [Resource]),
    /// Complete classic or native resource fork, decoded under fixed budgets.
    SuitcaseFork(&'a [u8]),
    /// One static TrueType glyf face, not a collection or arbitrary OpenType font.
    TrueType(&'a [u8])
}

#[derive(Clone, Copy)]
pub struct InstalledFontSource<'a>
{
    pub key:      FontSourceKey,
    /// Caller catalog revision; supplied bytes remain authoritative even when
    /// timestamp granularity permits unrelated payloads at the same revision.
    pub revision: u64,
    pub data:     InstalledFontData<'a>
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontInstallFailure
{
    Allocation,
    Limit,
    SourceLimit,
    DuplicateSource,
    DuplicateResource,
    Malformed,
    Unsupported,
    MissingName,
    MissingAssociation,
    NoFamilies,
    FamilyIdsExhausted,
    RevisionExhausted,
    RejectedSnapshot
}

impl FontInstallFailure
{
    pub fn is_source_rejection(self) -> bool
    {
        matches!(self,
                 Self::SourceLimit
                 | Self::DuplicateResource
                 | Self::Malformed
                 | Self::Unsupported
                 | Self::MissingName
                 | Self::MissingAssociation
                 | Self::NoFamilies)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FontInstallError
{
    pub source:  Option<FontSourceKey>,
    pub failure: FontInstallFailure
}

impl FontInstallError
{
    pub fn status(self) -> Status
    {
        match self.failure
        {
            FontInstallFailure::Allocation
            | FontInstallFailure::Limit
            | FontInstallFailure::SourceLimit
            | FontInstallFailure::FamilyIdsExhausted
            | FontInstallFailure::RevisionExhausted => NO_MEMORY,
            FontInstallFailure::Unsupported => NOT_SUPPORTED,
            FontInstallFailure::Malformed
            | FontInstallFailure::DuplicateResource
            | FontInstallFailure::MissingName
            | FontInstallFailure::MissingAssociation => CORRUPT_DATA,
            _ => BAD_ARGUMENT
        }
    }
}

#[derive(Debug)]
pub struct InspectedFontFamily
{
    pub name:           String,
    pub style_name:     Option<String>,
    pub classic_family: Option<i16>,
    pub fond_id:        Option<i16>
}

struct ParsedFamily
{
    metadata: InspectedFontFamily,
    faces:    Vec<Association>
}

pub(super) struct RetainedSource
{
    key:                FontSourceKey,
    revision:           u64,
    pub(super) storage: usize,
    standalone:         bool,
    families:           Vec<Arc<FontFamily>>
}

pub struct InstalledFontSnapshot
{
    manager:            FontManager,
    seen:               Vec<FontSourceKey>,
    retained_bytes:     usize,
    associations:       usize,
    glyph_records:      usize,
    failure:            Option<FontInstallError>,
    previous_available: Vec<Arc<FontFamily>>,
    previous_revision:  u32
}

impl InstalledFontSnapshot
{
    /// Source rejections omit that file and permit further admission. Other
    /// failures poison the transaction. Every attempted key counts toward the
    /// snapshot limit; a rejected key cannot be retried in this transaction.
    pub fn add_source(&mut self, source: InstalledFontSource<'_>) -> Result<(), FontInstallError>
    {
        if let Some(error) = self.failure
        {
            return Err(error);
        }
        let next_family = self.manager.next_family;
        let retained_bytes = self.retained_bytes;
        let result = self.add(source);
        if let Err(error) = result
        {
            if error.failure.is_source_rejection()
            {
                self.manager.next_family = next_family;
                self.retained_bytes = retained_bytes;
            }
            else
            {
                self.failure = Some(error);
            }
        }
        result
    }

    fn admit_key(&mut self, key: FontSourceKey) -> Result<(), FontInstallError>
    {
        let fail = |failure| {
            FontInstallError { source: Some(key),
                               failure }
        };
        if self.seen.len() >= MAX_SOURCES
        {
            return Err(fail(FontInstallFailure::Limit));
        }
        if self.seen.contains(&key)
        {
            return Err(fail(FontInstallFailure::DuplicateSource));
        }
        push(&mut self.seen, key).map_err(fail)
    }

    /// Reuse an immutable file whose catalog revision the caller has confirmed
    /// unchanged. False requests loading its bytes through add_source instead.
    /// A reused revision must never describe a different CNID incarnation.
    pub fn reuse_source(&mut self, key: FontSourceKey, revision: u64)
                        -> Result<bool, FontInstallError>
    {
        if let Some(error) = self.failure
        {
            return Err(error);
        }
        let Some(old) = self.manager
                            .installed
                            .iter()
                            .find(|old| old.key == key && old.revision == revision)
                            .cloned()
        else
        {
            return Ok(false);
        };
        if self.manager
               .installed
               .iter()
               .filter(|old| old.key == key && old.revision == revision)
               .count()
           != 1
        {
            return Ok(false);
        }
        let result = (|| {
            self.admit_key(key)?;
            for family in &old.families
            {
                push(&mut self.manager.available, family.clone()).map_err(|failure| {
                    FontInstallError { source: Some(key),
                                       failure }
                })?;
            }
            Ok(true)
        })();
        if let Err(error) = result
        {
            self.failure = Some(error);
        }
        result
    }

    /// Abort when filesystem reads, revision checks or discovery fail. The
    /// caller's typed I/O error remains its responsibility to report.
    pub fn reject(&mut self)
    {
        if self.failure.is_none()
        {
            self.failure = Some(FontInstallError { source:  None,
                                                   failure: FontInstallFailure::RejectedSnapshot });
        }
    }

    fn add(&mut self, source: InstalledFontSource<'_>) -> Result<(), FontInstallError>
    {
        let fail = |failure| {
            FontInstallError { source: Some(source.key),
                               failure }
        };
        self.admit_key(source.key)?;
        validate_input(source.data).map_err(fail)?;
        let decoded = match source.data
        {
            InstalledFontData::SuitcaseFork(bytes) => Some(decode_suitcase(bytes).map_err(fail)?),
            _ => None
        };
        let data = decoded.as_deref().map_or(source.data, InstalledFontData::Suitcase);
        if let Some(old) =
            self.manager.installed.iter().find(|old| {
                                             old.key == source.key
                                             && old.revision == source.revision
                                             && same_payload(data, old,
                                                             &self.manager.resources[old.storage])
                                         })
        {
            for family in &old.families
            {
                push(&mut self.manager.available, family.clone()).map_err(fail)?;
            }
            return Ok(());
        }
        let resources = if let Some(resources) = decoded
        {
            resources
        }
        else
        {
            copy_payload(source.data).map_err(fail)?
        };
        let retained_bytes =
            self.retained_bytes
                .checked_add(payload_bytes(&resources).ok_or_else(|| {
                                                          fail(FontInstallFailure::Limit)
                                                      })?)
                .ok_or_else(|| fail(FontInstallFailure::Limit))?;
        let storage = self.manager.resources.len();
        let parsed =
            match source.data
            {
                InstalledFontData::TrueType(_) =>
                {
                    FontManager::standalone_family(&resources[0], storage, &mut self.glyph_records)
                }
                _ =>
                {
                    FontManager::suitcase_families(&resources, storage, &mut self.associations,
                                                   &mut self.glyph_records)
                }
            }.map_err(fail)?;
        if parsed.is_empty()
        {
            return Err(fail(FontInstallFailure::NoFamilies));
        }
        if retained_bytes > MAX_RETAINED_BYTES
           || self.manager.installed.len() >= MAX_RETAINED_SOURCES
        {
            return Err(fail(FontInstallFailure::Limit));
        }
        self.retained_bytes = retained_bytes;
        let digest =
            mac_formats::authority::image_digest(&resources).map_err(format_error).map_err(fail)?;
        let families = self.manager.install_families(&source, parsed, digest).map_err(fail)?;
        for family in &families
        {
            if self.manager.families.len() >= MAX_FAMILIES
            {
                return Err(fail(FontInstallFailure::Limit));
            }
            push(&mut self.manager.families, family.clone()).map_err(fail)?;
            push(&mut self.manager.available, family.clone()).map_err(fail)?;
        }
        push(&mut self.manager.resources, shared(resources).map_err(fail)?).map_err(fail)?;
        push(&mut self.manager.installed,
             shared(RetainedSource { key: source.key,
                                     revision: source.revision,
                                     storage,
                                     standalone: matches!(source.data,
                                                          InstalledFontData::TrueType(_)),
                                     families }).map_err(fail)?).map_err(fail)
    }

    pub fn finish(mut self) -> Result<FontManager, FontInstallError>
    {
        if let Some(error) = self.failure
        {
            return Err(error);
        }
        self.manager.available.sort_unstable_by_key(|family| {
                                  match family.origin
                                  {
                                      FontOrigin::Bundled =>
                                      {
                                          (false,
                                           FontSourceKey { volume: 0,
                                                           cnid:   0 },
                                           family.id)
                                      }
                                      FontOrigin::Installed { source,
                                                              fond_id,
                                                              .. } =>
                                      {
                                          (true, source, fond_id.unwrap_or(0))
                                      }
                                  }
                              });
        let changed = self.previous_available.len() != self.manager.available.len()
                      || self.previous_available
                             .iter()
                             .zip(&self.manager.available)
                             .any(|(old, current)| !Arc::ptr_eq(old, current));
        self.manager.revision = if changed
        {
            self.previous_revision
                .checked_add(1)
                .ok_or(FontInstallError { source:  None,
                                          failure: FontInstallFailure::RevisionExhausted })?
        }
        else
        {
            self.previous_revision
        };
        Ok(self.manager)
    }
}

fn font_resource(resource: &Resource) -> bool
{
    matches!(&resource.kind, b"FOND" | b"FONT" | b"NFNT" | b"sfnt")
}

fn copy_bytes(bytes: &[u8]) -> Result<Vec<u8>, FontInstallFailure>
{
    let mut result = Vec::new();
    result.try_reserve_exact(bytes.len()).map_err(|_| FontInstallFailure::Allocation)?;
    result.extend_from_slice(bytes);
    Ok(result)
}

fn shared<T>(value: T) -> Result<Arc<T>, FontInstallFailure>
{
    Arc::try_new(value).map_err(|_| FontInstallFailure::Allocation)
}

fn push<T>(values: &mut Vec<T>, value: T) -> Result<(), FontInstallFailure>
{
    values.try_reserve(1).map_err(|_| FontInstallFailure::Allocation)?;
    values.push(value);
    Ok(())
}

fn format_error(error: mac_formats::Error) -> FontInstallFailure
{
    match error
    {
        mac_formats::Error::AllocationFailed(_) => FontInstallFailure::Allocation,
        mac_formats::Error::BudgetExceeded(_) => FontInstallFailure::SourceLimit,
        mac_formats::Error::TranslationRequired(_) => FontInstallFailure::Unsupported,
        _ => FontInstallFailure::Malformed
    }
}

fn installed_key(digest: [u8; 32], kind: &str, id: i16) -> Result<String, FontInstallFailure>
{
    use core::fmt::Write;
    let mut key = String::new();
    key.try_reserve_exact(96).map_err(|_| FontInstallFailure::Allocation)?;
    write!(&mut key, "installed:{kind}:").map_err(|_| FontInstallFailure::Malformed)?;
    for byte in digest
    {
        write!(&mut key, "{byte:02x}").map_err(|_| FontInstallFailure::Malformed)?;
    }
    write!(&mut key, ":{id}").map_err(|_| FontInstallFailure::Malformed)?;
    Ok(key)
}

fn payload_bytes(resources: &[Resource]) -> Option<usize>
{
    resources.iter().try_fold(0usize, |total, resource| {
                        total.checked_add(resource.data.len())?
                             .checked_add(resource.name.as_ref().map_or(0, Vec::len))
                    })
}

impl FontManager
{
    /// Inspect owned metadata only: no installed IDs, resource-chain entries,
    /// manager mutation or shared lifecycle objects are created.
    pub fn inspect_source(data: InstalledFontData<'_>)
                          -> Result<Vec<InspectedFontFamily>, FontInstallError>
    {
        let fail = |failure| {
            FontInstallError { source: None,
                               failure }
        };
        validate_input(data).map_err(fail)?;
        let resources = copy_payload(data).map_err(fail)?;
        let mut work = 0;
        let mut glyph_records = 0;
        let parsed = match data
                     {
                         InstalledFontData::TrueType(_) =>
                         {
                             Self::standalone_family(&resources[0], 0, &mut glyph_records)
                         }
                         _ => Self::suitcase_families(&resources, 0, &mut work, &mut glyph_records)
                     }.map_err(fail)?;
        if parsed.is_empty()
        {
            return Err(fail(FontInstallFailure::NoFamilies));
        }
        let mut metadata = Vec::new();
        metadata.try_reserve_exact(parsed.len())
                .map_err(|_| fail(FontInstallFailure::Allocation))?;
        metadata.extend(parsed.into_iter().map(|family| family.metadata));
        Ok(metadata)
    }

    /// Build a complete replacement off the drawing/publication path. Any error
    /// leaves this manager unchanged. The caller publishes only against this
    /// same baseline; parallel updates must not overwrite one another.
    ///
    /// Source keys are sorted for deterministic admission. Bundled families win
    /// name lookup; installed duplicates follow volume/CNID and FOND order.
    /// Removed/revised numeric selections retain their original backing until
    /// the manager lineage is dropped. Retention has a hard capacity: exhaustion
    /// refuses refresh instead of recycling IDs or silently discarding fonts.
    pub fn with_installed_snapshot(&self, sources: &[InstalledFontSource<'_>])
                                   -> Result<Self, FontInstallError>
    {
        let error = |source, failure| {
            FontInstallError { source,
                               failure }
        };
        if sources.len() > MAX_SOURCES
        {
            return Err(error(None, FontInstallFailure::Limit));
        }
        let mut order = Vec::new();
        order.try_reserve_exact(sources.len())
             .map_err(|_| error(None, FontInstallFailure::Allocation))?;
        order.extend(0..sources.len());
        order.sort_unstable_by_key(|&index| sources[index].key);
        for pair in order.windows(2)
        {
            if sources[pair[0]].key == sources[pair[1]].key
            {
                return Err(error(Some(sources[pair[0]].key), FontInstallFailure::DuplicateSource));
            }
        }
        let mut staged = self.begin_installed_snapshot()?;
        for index in order
        {
            staged.add_source(sources[index])?;
        }
        staged.finish()
    }

    /// Start a complete current-source transaction. Omission hides an installed
    /// source; it never removes payloads needed by an earlier numeric selection.
    pub fn begin_installed_snapshot(&self) -> Result<InstalledFontSnapshot, FontInstallError>
    {
        let error = |source, failure| {
            FontInstallError { source,
                               failure }
        };
        let mut result = Self::default();
        result.next_family = self.next_family.max(256);
        result.resources
              .try_reserve_exact(self.resources.len())
              .map_err(|_| error(None, FontInstallFailure::Allocation))?;
        result.resources.extend(self.resources.iter().cloned());
        result.families
              .try_reserve_exact(self.families.len())
              .map_err(|_| error(None, FontInstallFailure::Allocation))?;
        result.families.extend(self.families.iter().cloned());
        result.installed
              .try_reserve_exact(self.installed.len())
              .map_err(|_| error(None, FontInstallFailure::Allocation))?;
        result.installed.extend(self.installed.iter().cloned());
        for family in self.available.iter().filter(|family| family.origin == FontOrigin::Bundled)
        {
            push(&mut result.available, family.clone()).map_err(|failure| error(None, failure))?;
        }
        let retained_bytes =
            self.resources
                .iter()
                .try_fold(0usize, |total, resources| total.checked_add(payload_bytes(resources)?))
                .ok_or_else(|| error(None, FontInstallFailure::Limit))?;
        let mut previous_available = Vec::new();
        previous_available.try_reserve_exact(self.available.len())
                          .map_err(|_| error(None, FontInstallFailure::Allocation))?;
        previous_available.extend(self.available.iter().cloned());
        Ok(InstalledFontSnapshot { manager: result,
                                   seen: Vec::new(),
                                   retained_bytes,
                                   associations: 0,
                                   glyph_records: 0,
                                   failure: None,
                                   previous_available,
                                   previous_revision: self.revision })
    }

    /// Current origin-aware resource view. Removed payloads remain accessible
    /// only through retained rendering, not the available resource namespace.
    pub fn available_resources(&self) -> impl Iterator<Item = (Option<FontSourceKey>, &Resource)>
    {
        self.resources
            .iter()
            .enumerate()
            .filter_map(|(storage, resources)| {
                if storage == 0 && !self.installed.iter().any(|source| source.storage == storage)
                {
                    return Some((None, resources));
                }
                let source = self.installed.iter().find(|source| source.storage == storage)?;
                self.available
                    .iter()
                    .any(|family| source.families.iter().any(|old| Arc::ptr_eq(old, family)))
                    .then_some((Some(source.key), resources))
            })
            .flat_map(|(source, resources)| {
                resources.iter().map(move |resource| (source, resource))
            })
    }

    fn installed_id(&mut self) -> Result<i16, FontInstallFailure>
    {
        while self.next_family <= i16::MAX as u32
        {
            let id = self.next_family as i16;
            self.next_family += 1;
            if !self.families.iter().any(|family| family.id == id)
            {
                return Ok(id);
            }
        }
        Err(FontInstallFailure::FamilyIdsExhausted)
    }

    fn install_families(&mut self, source: &InstalledFontSource<'_>, parsed: Vec<ParsedFamily>,
                        digest: [u8; 32])
                        -> Result<Vec<Arc<FontFamily>>, FontInstallFailure>
    {
        let mut families = Vec::new();
        families.try_reserve_exact(parsed.len()).map_err(|_| FontInstallFailure::Allocation)?;
        for parsed in parsed
        {
            let id = self.installed_id()?;
            let metadata = parsed.metadata;
            let key = installed_key(digest,
                                    if metadata.fond_id.is_some() { "fond" } else { "sfnt" },
                                    metadata.fond_id.unwrap_or(0))?;
            families.push(shared(FontFamily {
                id, name: metadata.name, key, faces: parsed.faces,
                style_name: metadata.style_name, classic_family: metadata.classic_family,
                origin: FontOrigin::Installed { source: source.key, revision: source.revision,
                                                fond_id: metadata.fond_id }
            })?);
        }
        Ok(families)
    }

    fn suitcase_families(resources: &[Resource], storage: usize, work: &mut usize,
                         glyph_records: &mut usize)
                         -> Result<Vec<ParsedFamily>, FontInstallFailure>
    {
        let mut keys = Vec::new();
        keys.try_reserve_exact(resources.len()).map_err(|_| FontInstallFailure::Allocation)?;
        keys.extend(0..resources.len());
        keys.sort_unstable_by_key(|&index| (resources[index].kind, resources[index].id));
        for pair in keys.windows(2)
        {
            if (resources[pair[0]].kind, resources[pair[0]].id)
               == (resources[pair[1]].kind, resources[pair[1]].id)
            {
                return Err(FontInstallFailure::DuplicateResource);
            }
        }
        let mut validated = Vec::new();
        validated.try_reserve_exact(resources.len()).map_err(|_| FontInstallFailure::Allocation)?;
        validated.resize(resources.len(), false);
        let mut families = Vec::new();
        for &index in keys.iter().filter(|&&index| resources[index].kind == *b"FOND")
        {
            let fond = &resources[index];
            let data = &fond.data;
            let classic_id = word(data, 2).ok_or(FontInstallFailure::Malformed)? as i16;
            if families.iter()
                       .any(|family: &ParsedFamily| family.metadata.classic_family == Some(classic_id))
            {
                return Err(FontInstallFailure::DuplicateResource);
            }
            let count = usize::from(word(data, 52).ok_or(FontInstallFailure::Malformed)?) + 1;
            let end = 54 + count * 6;
            let records = data.get(54..end).ok_or(FontInstallFailure::Malformed)?;
            if count > MAX_ASSOCIATIONS
            {
                return Err(FontInstallFailure::SourceLimit);
            }
            *work = work.checked_add(count).ok_or(FontInstallFailure::Limit)?;
            if *work > MAX_ASSOCIATIONS
            {
                return Err(FontInstallFailure::Limit);
            }
            let mut faces = Vec::new();
            faces.try_reserve_exact(count).map_err(|_| FontInstallFailure::Allocation)?;
            for record in records.chunks_exact(6)
            {
                let size = i16::from_be_bytes([record[0], record[1]]);
                let face = u16::from_be_bytes([record[2], record[3]]);
                let id = i16::from_be_bytes([record[4], record[5]]);
                if size < 0 || size > super::MAX_POINT_SIZE || face & !0x7f != 0
                {
                    return Err(FontInstallFailure::Malformed);
                }
                let find = |kind| {
                    keys.binary_search_by_key(&(kind, id), |&index| {
                            (resources[index].kind, resources[index].id)
                        })
                        .ok()
                        .map(|position| keys[position])
                };
                let resource = if size == 0
                               {
                                   find(*b"sfnt")
                               }
                               else
                               {
                                   find(*b"NFNT").or_else(|| find(*b"FONT"))
                               }.ok_or(FontInstallFailure::MissingAssociation)?;
                if !validated[resource]
                {
                    if size == 0
                    {
                        validate_truetype(&resources[resource].data, glyph_records)?;
                    }
                    else
                    {
                        match FaceValidity::inspect(&resources[resource], size)
                        {
                            FaceValidity::Valid =>
                            {}
                            FaceValidity::Unsupported =>
                            {
                                return Err(FontInstallFailure::Unsupported);
                            }
                            FaceValidity::Malformed => return Err(FontInstallFailure::Malformed)
                        }
                    }
                    validated[resource] = true;
                }
                if faces.iter().any(|old: &Association| old.size == size && old.face == face)
                {
                    return Err(FontInstallFailure::DuplicateResource);
                }
                faces.push(Association { size,
                                         face,
                                         resource,
                                         storage,
                                         validity: FaceValidity::Valid });
            }
            let name = fond.name
                           .as_deref()
                           .filter(|name| !name.is_empty())
                           .ok_or(FontInstallFailure::MissingName)?;
            let name = catalog_name(name).map_err(|_| FontInstallFailure::Allocation)?;
            if name.chars().any(char::is_control) || name.trim().is_empty()
            {
                return Err(FontInstallFailure::Malformed);
            }
            push(&mut families,
                 ParsedFamily { metadata: InspectedFontFamily { name,
                                                                style_name: None,
                                                                classic_family:
                                                                    Some(classic_id),
                                                                fond_id: Some(fond.id) },
                                faces })?;
        }
        Ok(families)
    }

    fn standalone_family(resource: &Resource, storage: usize, glyph_records: &mut usize)
                         -> Result<Vec<ParsedFamily>, FontInstallFailure>
    {
        validate_truetype(&resource.data, glyph_records)?;
        let font =
            ttf_parser::Face::parse(&resource.data, 0).map_err(|_| FontInstallFailure::Malformed)?;
        let name = font_name(&font, 16, 1)?;
        let style_name = font_name(&font, 17, 2)?;
        let face = if font.is_bold() { BOLD } else { 0 }
                   | if font.is_italic() || font.is_oblique() { ITALIC } else { 0 };
        let mut faces = Vec::new();
        push(&mut faces,
             Association { size: 0,
                           face,
                           resource: 0,
                           storage,
                           validity: FaceValidity::Valid })?;
        let mut families = Vec::new();
        push(&mut families,
             ParsedFamily { metadata: InspectedFontFamily { name,
                                                            style_name: Some(style_name),
                                                            classic_family: None,
                                                            fond_id: None },
                            faces })?;
        Ok(families)
    }
}

fn validate_input(data: InstalledFontData<'_>) -> Result<(), FontInstallFailure>
{
    let length = match data
    {
        InstalledFontData::TrueType(bytes) | InstalledFontData::SuitcaseFork(bytes) => bytes.len(),
        InstalledFontData::Suitcase(resources) =>
        {
            if resources.len() > MAX_RESOURCES
            {
                return Err(FontInstallFailure::SourceLimit);
            }
            for resource in resources.iter().filter(|resource| font_resource(resource))
            {
                if resource.name.as_ref().is_some_and(|name| name.len() > 255)
                {
                    return Err(FontInstallFailure::SourceLimit);
                }
            }
            resources.iter()
                     .filter(|resource| font_resource(resource))
                     .try_fold(0usize, |total, resource| {
                         total.checked_add(resource.data.len())?
                              .checked_add(resource.name.as_ref().map_or(0, Vec::len))
                     })
                     .ok_or(FontInstallFailure::SourceLimit)?
        }
    };
    if length > MAX_SOURCE_BYTES { Err(FontInstallFailure::SourceLimit) } else { Ok(()) }
}

fn same_payload(data: InstalledFontData<'_>, old: &RetainedSource, resources: &[Resource]) -> bool
{
    match data
    {
        InstalledFontData::TrueType(bytes) =>
        {
            old.standalone && resources.len() == 1 && resources[0].data == bytes
        }
        InstalledFontData::Suitcase(input) =>
        {
            !old.standalone && input.iter().filter(|resource| font_resource(resource)).eq(resources)
        }
        InstalledFontData::SuitcaseFork(_) => false
    }
}

fn copy_payload(data: InstalledFontData<'_>) -> Result<Vec<Resource>, FontInstallFailure>
{
    let mut result = Vec::new();
    match data
    {
        InstalledFontData::TrueType(bytes) =>
        {
            push(&mut result,
                 Resource { kind:       *b"sfnt",
                            id:         0,
                            name:       None,
                            attributes: 0,
                            data:       copy_bytes(bytes)? })?;
        }
        InstalledFontData::Suitcase(resources) =>
        {
            result.try_reserve_exact(resources.iter().filter(|r| font_resource(r)).count())
                  .map_err(|_| FontInstallFailure::Allocation)?;
            for resource in resources.iter().filter(|resource| font_resource(resource))
            {
                result.push(Resource { kind:       resource.kind,
                                       id:         resource.id,
                                       attributes: resource.attributes,
                                       name:       resource.name
                                                           .as_deref()
                                                           .map(copy_bytes)
                                                           .transpose()?,
                                       data:       copy_bytes(&resource.data)? });
            }
        }
        InstalledFontData::SuitcaseFork(bytes) => return decode_suitcase(bytes)
    }
    Ok(result)
}

fn decode_suitcase(bytes: &[u8]) -> Result<Vec<Resource>, FontInstallFailure>
{
    use mac_formats::resource::stream::{Budget, read_selected};
    read_selected(bytes.len() as u64,
                  Budget { map_bytes:     256 * 1024,
                           payload_bytes: MAX_SOURCE_BYTES,
                           resources:     MAX_RESOURCES },
                  |kind, _| matches!(&kind, b"FOND" | b"FONT" | b"NFNT" | b"sfnt"),
                  |offset, output| {
                      let start = usize::try_from(offset).map_err(|_| {
                                      mac_formats::Error::Malformed("font resource offset overflow")
                                  })?;
                      let end =
                          start.checked_add(output.len())
                               .ok_or(mac_formats::Error::Malformed("font resource range \
                                                                     overflow"))?;
                      let input =
                          bytes.get(start..end)
                               .ok_or(mac_formats::Error::Malformed("truncated font resource \
                                                                     fork"))?;
                      output.copy_from_slice(input);
                      Ok(())
                  }).map_err(|error| {
                        match error
                        {
                            mac_formats::Error::AllocationFailed(_) =>
                            {
                                FontInstallFailure::Allocation
                            }
                            mac_formats::Error::BudgetExceeded(_) =>
                            {
                                FontInstallFailure::SourceLimit
                            }
                            mac_formats::Error::TranslationRequired(_) =>
                            {
                                FontInstallFailure::Unsupported
                            }
                            _ => FontInstallFailure::Malformed
                        }
                    })
}

fn validate_truetype(data: &[u8], glyph_records: &mut usize) -> Result<(), FontInstallFailure>
{
    let signature = data.get(..4).ok_or(FontInstallFailure::Malformed)?;
    if signature != [0, 1, 0, 0] && signature != b"true"
    {
        return Err(FontInstallFailure::Unsupported);
    }
    let count = usize::from(word(data, 4).ok_or(FontInstallFailure::Malformed)?);
    if count == 0 || count > MAX_TABLES
    {
        return Err(FontInstallFailure::SourceLimit);
    }
    let records = data.get(12..12 + count * 16).ok_or(FontInstallFailure::Malformed)?;
    for (index, record) in records.chunks_exact(16).enumerate()
    {
        let tag = &record[..4];
        if matches!(tag,
                    b"CFF "
                    | b"CFF2"
                    | b"VARC"
                    | b"fvar"
                    | b"gvar"
                    | b"HVAR"
                    | b"VVAR"
                    | b"MVAR"
                    | b"COLR"
                    | b"CPAL"
                    | b"CBDT"
                    | b"CBLC"
                    | b"sbix"
                    | b"SVG ")
        {
            return Err(FontInstallFailure::Unsupported);
        }
        if records[..index * 16].chunks_exact(16).any(|old| &old[..4] == tag)
        {
            return Err(FontInstallFailure::Malformed);
        }
        let offset = u32::from_be_bytes(record[8..12].try_into()
                                                     .map_err(|_| FontInstallFailure::Malformed)?)
                     as usize;
        let length = u32::from_be_bytes(record[12..16].try_into()
                                                      .map_err(|_| FontInstallFailure::Malformed)?)
                     as usize;
        let end = offset.checked_add(length).ok_or(FontInstallFailure::Malformed)?;
        if end > data.len() || length != 0 && offset < 12 + count * 16
        {
            return Err(FontInstallFailure::Malformed);
        }
        for old in records[..index * 16].chunks_exact(16)
        {
            let old_offset = u32::from_be_bytes([old[8], old[9], old[10], old[11]]) as usize;
            let old_length = u32::from_be_bytes([old[12], old[13], old[14], old[15]]) as usize;
            if length != 0
               && old_length != 0
               && offset < old_offset + old_length
               && old_offset < end
            {
                return Err(FontInstallFailure::Malformed);
            }
        }
    }
    let face = ttf_parser::Face::parse(data, 0).map_err(|_| FontInstallFailure::Malformed)?;
    for tag in [b"glyf", b"loca", b"head", b"hhea", b"hmtx", b"maxp", b"cmap"]
    {
        if face.raw_face().table(ttf_parser::Tag::from_bytes(tag)).is_none()
        {
            return Err(FontInstallFailure::Malformed);
        }
    }
    let table = |tag| {
        face.raw_face().table(ttf_parser::Tag::from_bytes(tag)).ok_or(FontInstallFailure::Malformed)
    };
    let maxp = table(b"maxp")?;
    if maxp.get(..4) != Some(&[0, 1, 0, 0])
    {
        return Err(FontInstallFailure::Unsupported);
    }
    let glyphs = usize::from(word(maxp, 4).ok_or(FontInstallFailure::Malformed)?);
    if glyphs == 0 || glyphs > 16384
    {
        return Err(FontInstallFailure::SourceLimit);
    }
    for (offset, limit) in [(6, 8192),
                            (8, 1024),
                            (10, 8192),
                            (12, 1024),
                            (14, 2),
                            (16, 16384),
                            (18, 65535),
                            (20, 4096),
                            (22, 256),
                            (24, 16384),
                            (26, 65535),
                            (28, 64),
                            (30, 32)]
    {
        if word(maxp, offset).ok_or(FontInstallFailure::Malformed)? > limit
        {
            return Err(FontInstallFailure::SourceLimit);
        }
    }
    let loca = table(b"loca")?;
    let glyf = table(b"glyf")?;
    let metric_count = usize::from(word(table(b"hhea")?, 34).ok_or(FontInstallFailure::Malformed)?);
    if metric_count == 0
       || metric_count > glyphs
       || table(b"hmtx")?.len() < metric_count * 4 + (glyphs - metric_count) * 2
    {
        return Err(FontInstallFailure::Malformed);
    }
    let entry_size = match word(table(b"head")?, 50)
    {
        Some(0) => 2,
        Some(1) => 4,
        _ => return Err(FontInstallFailure::Malformed)
    };
    if loca.len() < (glyphs + 1) * entry_size
    {
        return Err(FontInstallFailure::Malformed);
    }
    *glyph_records = glyph_records.checked_add(glyphs).ok_or(FontInstallFailure::Limit)?;
    if *glyph_records > MAX_GLYPH_RECORDS
    {
        return Err(FontInstallFailure::Limit);
    }
    let mut previous = 0usize;
    for (index, record) in loca[..(glyphs + 1) * entry_size].chunks_exact(entry_size).enumerate()
    {
        let offset = if entry_size == 2
        {
            usize::from(u16::from_be_bytes([record[0], record[1]])) * 2
        }
        else
        {
            u32::from_be_bytes([record[0], record[1], record[2], record[3]]) as usize
        };
        if offset < previous || offset > glyf.len()
        {
            return Err(FontInstallFailure::Malformed);
        }
        if index != 0 && offset != previous
        {
            let header = glyf.get(previous..previous + 10)
                             .filter(|_| offset >= previous + 10)
                             .ok_or(FontInstallFailure::Malformed)?;
            let contours = i16::from_be_bytes([header[0], header[1]]);
            if contours < -1 || contours > 1024
            {
                return Err(FontInstallFailure::SourceLimit);
            }
        }
        previous = offset;
    }
    if face.tables().cmap.is_none()
    {
        return Err(FontInstallFailure::Malformed);
    }
    use skrifa::{MetadataProvider, outline::OutlineGlyphFormat};
    let raw = skrifa::FontRef::new(data).map_err(|_| FontInstallFailure::Malformed)?;
    if raw.outline_glyphs().format() != Some(OutlineGlyphFormat::Glyf)
    {
        return Err(FontInstallFailure::Unsupported);
    }
    Ok(())
}

fn font_name(font: &ttf_parser::Face<'_>, preferred: u16, fallback: u16)
             -> Result<String, FontInstallFailure>
{
    let names = font.names();
    if names.len() > 256
    {
        return Err(FontInstallFailure::SourceLimit);
    }
    let mut best = None;
    for name in names
    {
        if name.name_id != preferred && name.name_id != fallback
        {
            continue;
        }
        let encoding = match (name.platform_id, name.encoding_id)
        {
            (ttf_parser::PlatformId::Unicode, _) => 0,
            (ttf_parser::PlatformId::Windows, 0 | 1 | 10) => 0,
            (ttf_parser::PlatformId::Macintosh, 0) => 1,
            _ => continue
        };
        let score = (name.name_id != preferred,
                     name.language_id != 0x409 && name.language_id != 0,
                     encoding);
        if best.as_ref().is_none_or(|(old, _, _)| score < *old)
        {
            best = Some((score, encoding, name.name));
        }
    }
    let (_, encoding, bytes) = best.ok_or(FontInstallFailure::MissingName)?;
    if bytes.is_empty() || bytes.len() > MAX_NAME_BYTES
    {
        return Err(FontInstallFailure::SourceLimit);
    }
    let result = if encoding == 1
    {
        catalog_name(bytes).map_err(|_| FontInstallFailure::Allocation)?
    }
    else
    {
        if bytes.len() % 2 != 0
        {
            return Err(FontInstallFailure::Malformed);
        }
        let mut result = String::new();
        result.try_reserve_exact(bytes.len() * 2).map_err(|_| FontInstallFailure::Allocation)?;
        for character in
            char::decode_utf16(bytes.chunks_exact(2)
                                    .map(|pair| u16::from_be_bytes([pair[0], pair[1]])))
        {
            result.push(character.map_err(|_| FontInstallFailure::Malformed)?);
        }
        result
    };
    if result.chars().any(char::is_control) || result.trim().is_empty()
    {
        return Err(FontInstallFailure::Malformed);
    }
    Ok(result)
}

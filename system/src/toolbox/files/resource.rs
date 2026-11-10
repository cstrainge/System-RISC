//! Bounded, read-only access to one disk resource; executable data stays on disk.
use mac_object_table::shared::InvocationGate as _;

use super::*;
use crate::toolbox::{handles::InvocationGate,
                     quickdraw::{icons, icons_service}};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleReadError
{
    Storage(Status),
    Format(mac_formats::Error)
}
impl core::fmt::Display for ModuleReadError
{
    fn fmt(&self, output: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Storage(NO_MEMORY) =>
            {
                output.write_str("module resource storage read exceeded available memory or its \
                                  admission budget; retain the package, free System memory and \
                                  retry")
            }
            Self::Storage(status) =>
            {
                write!(output,
                       "module resource storage read failed with status {status}; retain the \
                        package, repair or remount its volume and retry")
            }
            Self::Format(cause @ mac_formats::Error::AllocationFailed(_)) =>
            {
                write!(output,
                       "module resource memory admission failed ({cause}); free System memory and \
                        retry")
            }
            Self::Format(cause) => cause.fmt(output)
        }
    }
}
impl ModuleReadError
{
    pub fn status(self) -> Status
    {
        match self
        {
            Self::Storage(status) => status,
            Self::Format(mac_formats::Error::BudgetExceeded(_)
                         | mac_formats::Error::AllocationFailed(_)) => NO_MEMORY,
            Self::Format(mac_formats::Error::Missing(_)) => NOT_FOUND,
            Self::Format(mac_formats::Error::TranslationRequired(_)) => TRANSLATION_REQUIRED,
            Self::Format(mac_formats::Error::Malformed(_)) => BAD_ABI
        }
    }
}

#[cfg(systemrisc)]
fn module_headers(index: mac_formats::resource::stream::ReferenceIndex<'_>)
                  -> impl Iterator<Item = mac_formats::module::ResourceHeader> + Clone
{
    index.entries().map(mac_formats::module::ResourceHeader::from)
}

#[cfg(systemrisc)]
fn module_length(row: mac_formats::resource::index::Entry<'_>,
                 read: &mut dyn FnMut(u64, &mut [u8]) -> mac_formats::Result<()>)
                 -> mac_formats::Result<usize>
{
    let mut word = [0; 4];
    read(row.offset, &mut word)?;
    let length = u32::from_be_bytes(word) as usize;
    if row.offset + 4 + length as u64 > row.data_end
    {
        return Err(mac_formats::Error::Malformed("module resource payload lies outside its \
                                                  indexed data region; rebuild the package"));
    }
    Ok(length)
}

#[cfg(systemrisc)]
fn module_contract(index: mac_formats::resource::stream::ReferenceIndex<'_>,
                   row: mac_formats::resource::index::Entry<'_>, file_type: [u8; 4],
                   read: &mut dyn FnMut(u64, &mut [u8]) -> mac_formats::Result<()>)
                   -> mac_formats::Result<Option<mac_formats::resource::Resource>>
{
    use mac_formats::{Error, resource::Resource};
    let size = module_length(row, read)?;
    mac_formats::module::validate_contract_size(size)?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(size).map_err(|_| {
                                      Error::AllocationFailed("module contract metadata \
                                                               allocation failed; free System \
                                                               memory and retry")
                                  })?;
    bytes.resize(size, 0);
    read(row.offset + 4, &mut bytes)?;
    mac_formats::module::validate_contract(&bytes, row.into(), file_type, module_headers(index))?;
    if row.id != 0
    {
        return Ok(None);
    }
    let name = if let Some(source) = row.name
    {
        let mut name = Vec::new();
        name.try_reserve_exact(source.len()).map_err(|_| {
                                                 Error::AllocationFailed("module metadata name \
                                                                          allocation failed; free \
                                                                          System memory and retry")
                                             })?;
        name.extend_from_slice(source);
        Some(name)
    }
    else
    {
        None
    };
    Ok(Some(Resource { kind: row.kind,
                       id: row.id,
                       name,
                       attributes: row.attributes,
                       data: bytes }))
}

#[cfg(systemrisc)]
fn module_preflight(index: mac_formats::resource::stream::ReferenceIndex<'_>, file_type: [u8; 4],
                    read: &mut dyn FnMut(u64, &mut [u8]) -> mac_formats::Result<()>)
                    -> mac_formats::Result<mac_formats::resource::Resource>
{
    use mac_formats::Error;
    mac_formats::module::validate_index(file_type, module_headers(index))?;
    if file_type == *b"INIT"
    {
        mac_formats::module::require_extension_target(mac_module::ExtensionTarget::System,
                                                      module_headers(index))?;
    }
    for row in index.entries()
    {
        let size = module_length(row, read)?;
        mac_formats::module::validate_code_size(row.kind, size)?;
    }
    let mut selected = None;
    for row in index.entries().filter(|row| [*b"DABI", *b"EABI"].contains(&row.kind))
    {
        if let Some(metadata) = module_contract(index, row, file_type, read)?
        {
            selected = Some(metadata);
        }
    }
    selected.ok_or(Error::Malformed("module has no selected System contract; rebuild the package"))
}

/// Complete framing and both target contracts are validated before selected
/// System code is admitted. Finder body/digest/relocations remain uninspected;
/// its code stays on disk after indexed range and stored-size validation.
#[cfg(systemrisc)]
fn project_module(length: u64, file_type: [u8; 4], metadata_only: bool,
                  mut storage: impl FnMut(u64, &mut [u8]) -> Result<(), Status>)
                  -> Result<Vec<mac_formats::resource::Resource>, ModuleReadError>
{
    use mac_formats::{Error, resource::stream};
    if length == 0
    {
        return Err(ModuleReadError::Format(Error::Missing("module resource fork is missing; \
                                                           rebuild the package")));
    }
    let mut selected_metadata = None;
    let mut io_error = None;
    // Retained System metadata adds at most MAX_BYTES after this projection;
    // selected storage stays within the existing 2 MiB + metadata budget.
    // Finder metadata is temporary preflight only.
    let budget = stream::Budget { payload_bytes: mac_module::MAX_CODE_BYTES,
                                  ..Default::default() };
    let select = |kind, id| {
        !metadata_only
        && ((kind == *b"RV64" && id == 0)
            || kind == *b"DRVR"
            || [*b"FASS", *b"ICN#", *b"icl4", *b"icl8", *b"ics#", *b"ics4", *b"ics8", *b"ICON",
                *b"SICN", *b"cicn"].contains(&kind))
    };
    let result = stream::read_selected_with_index(length,
                                                  budget,
                                                  select,
                                                  |index, read| {
                                                      selected_metadata =
                                                          Some(module_preflight(index,
                                                                                file_type, read)?);
                                                      Ok(())
                                                  },
                                                  |offset, output| {
                                                      storage(offset, output).map_err(|status| {
                                                          io_error = Some(status);
                                                          Error::Malformed("module resource \
                                                                            storage read failed")
                                                      })
                                                  });
    let mut resources = result.map_err(|cause| {
                                  io_error.map(ModuleReadError::Storage)
                                          .unwrap_or(ModuleReadError::Format(cause))
                              })?;
    let metadata =
        selected_metadata.ok_or(ModuleReadError::Format(Error::Malformed("module preflight \
                                                                          did not retain the \
                                                                          selected contract; \
                                                                          rebuild System")))?;
    resources.try_reserve(1).map_err(|_| {
        ModuleReadError::Format(Error::AllocationFailed("module selected metadata admission failed; free System memory and retry"))
    })?;
    resources.push(metadata);
    Ok(resources)
}

pub(super) fn application_format_status(error: mac_formats::Error) -> Status
{
    match error
    {
        mac_formats::Error::BudgetExceeded(_) | mac_formats::Error::AllocationFailed(_) =>
        {
            NO_MEMORY
        }
        mac_formats::Error::Missing(_) => NOT_FOUND,
        mac_formats::Error::TranslationRequired(_) => TRANSLATION_REQUIRED,
        mac_formats::Error::Malformed(_) => CORRUPT_DATA
    }
}

#[cfg(systemrisc)]
pub(crate) struct ProviderInspection
{
    pub token:    u64,
    pub provider: u64,
    pub volume:   u32,
    pub fork:     u32,
    pub offset:   u64,
    pub length:   u64,
    pub index:    Option<mac_formats::resource::index::Index>
}
#[cfg(systemrisc)]
impl Manager
{
    #[cfg(systemrisc)]
    pub(crate) fn startup_preferences(&mut self, name: &str) -> Result<Vec<u8>, Status>
    {
        let id = self.volumes
                     .iter()
                     .find(|(_, v)| v.info().is_some_and(|i| i.startup != 0))
                     .map(|(id, _)| id)
                     .ok_or(NOT_FOUND)?;
        self.with_volume(id, |v| {
                let system = io(v.blessed_folder())?;
                let prefs = required(io(v.lookup(system.id, b"Preferences"))?)?;
                let file = required(io(v.lookup(prefs.id, name.as_bytes()))?)?;
                let mut fork = file.data.ok_or(CORRUPT_DATA)?;
                let length = usize::try_from(fork.length).map_err(|_| CORRUPT_DATA)?;
                if length > mac_file_types::MAX_BYTES
                {
                    return Err(CORRUPT_DATA);
                }
                let mut bytes = Vec::new();
                bytes.try_reserve_exact(length).map_err(|_| NO_MEMORY)?;
                bytes.resize(length, 0);
                if io(v.read(&mut fork, 0, &mut bytes))? != length
                {
                    return Err(IO_ERROR);
                }
                Ok(bytes)
            })
    }

    pub(crate) fn resource_provider(&self, owner: u64, handle: u32) -> Result<Option<u64>, Status>
    {
        self.file_provider(owner, handle)
    }

    pub(crate) fn cached_resource_query(&mut self, owner: u64,
                                        call: &mut mac_abi::resources::FileResourceCall,
                                        name: &str, capacity: usize)
                                        -> Result<bool, Status>
    {
        self.local_ready()?;
        let file = self.slot(owner, call.handle)?;
        let length = match file.description.fork.lock().length()
        {
            Ok(length) => length,
            Err(SERVICE_BUSY) => return Ok(false),
            Err(error) => return Err(error)
        };
        if file.resource_index.lock().is_none() && length == 0
        {
            *file.resource_index.lock() = Some(mac_formats::resource::index::Index::empty());
        }
        let resource_index = file.resource_index.lock();
        let Some(index) = resource_index.as_ref()
        else
        {
            return Ok(false);
        };
        let mut reply = *call;
        match crate::toolbox::resources::inspect::prepare(index, &mut reply, name, capacity)?
        {
            crate::toolbox::resources::inspect::Need::Done =>
            {
                *call = reply;
                Ok(true)
            }
            _ => Ok(false)
        }
    }

    pub(crate) fn begin_resource_inspection(&mut self, owner: u64, handle: u32)
                                            -> Result<ProviderInspection, Status>
    {
        let length = self.slot(owner, handle)?.description.fork.lock().length()?;
        // A composite positioned read holds the normal read lease but leaves
        // the public mark's position untouched. No provider callback runs here.
        let (provider, args) = self.begin_provider_file_read(owner, handle, 0)?;
        let index = self.slot(owner, handle)?.resource_index.lock().take();
        Ok(ProviderInspection { token: args.token,
                                provider,
                                volume: args.volume,
                                fork: args.flags,
                                offset: args.offset,
                                length,
                                index })
    }

    pub(crate) fn end_resource_inspection(&mut self, owner: u64, handle: u32, offset: u64,
                                          index: Option<mac_formats::resource::index::Index>)
                                          -> Result<(), Status>
    {
        let retired =
            self.handles
                .completing(handle)
                .is_some_and(|file| file.retired.load(core::sync::atomic::Ordering::Acquire));
        self.finish_provider_file_read(handle, offset, 0, None)?;
        if retired
        {
            return Ok(());
        }
        let file = self.slot(owner, handle)?;
        if !file.description.fork.lock().orphaned()
        {
            *file.resource_index.lock() = index;
        }
        Ok(())
    }
}

fn read_icon(length: u64, id: i16, resident: crate::toolbox::resources::manager::Manager<'_>,
             read: &mut impl FnMut(u64, &mut [u8]) -> Result<(), Status>)
             -> Result<icons::IconFamily, Status>
{
    let resources = crate::toolbox::resources::file::read_selected(length,
                                                                   |key| {
                                                                       key.id == id
                                                                       && matches!(&key.kind,
                                                                                   b"ICN#"
                                                                                   | b"icl4"
                                                                                   | b"icl8"
                                                                                   | b"ics#"
                                                                                   | b"ics4"
                                                                                   | b"ics8"
                                                                                   | b"ICON"
                                                                                   | b"SICN"
                                                                                   | b"cicn")
                                                                   },
                                                                   64 * 1024,
                                                                   read)?;
    icons::IconFamily::from_lookup(id, |kind, rid| {
        if kind == *b"clut"
        {
            resident.get(mac_abi::resources::SYSTEM,
                         ResourceKey { kind,
                                       id: rid })
                    .ok()
                    .map(|(_, r)| r.data.as_slice())
        }
        else
        {
            resources.iter().find(|r| r.kind == kind && r.id == rid).map(|r| r.data.as_slice())
        }
    }).map_err(icons_service::status)
}


#[cfg_attr(not(systemrisc), allow(dead_code))]
impl Manager
{
    pub(crate) fn take_resource_index(
        &mut self, owner: u64, handle: u32)
        -> Result<Option<mac_formats::resource::index::Index>, Status>
    {
        let file = self.slot(owner, handle)?;
        file.description.fork.lock().length()?;
        Ok(file.resource_index.lock().take())
    }

    pub(crate) fn restore_resource_index(&mut self, owner: u64, handle: u32,
                                         index: mac_formats::resource::index::Index)
                                         -> Result<(), Status>
    {
        let file = self.slot(owner, handle)?;
        file.description.fork.lock().length()?;
        *file.resource_index.lock() = Some(index);
        Ok(())
    }

    /// Startup reads use the same mounted filesystem service as later launches.
    /// ROM supplies block I/O, never an opaque whole-fork copying callback.
    /// Capture original catalog identity at the same read as package resources.
    /// Startup/replacement cwd must not be recovered by a later filename lookup.
    pub(crate) fn startup_application(&mut self, name: &str, payload_bytes: usize)
                                      -> Result<application::Loaded, Status>
    {
        let id = self.volumes
                     .iter()
                     .find(|(_, volume)| volume.info().is_some_and(|info| info.startup != 0))
                     .map(|(id, _)| id)
                     .ok_or(NOT_FOUND)?;
        self.with_volume(id,|volume| {
            let folder=io(volume.blessed_folder())?;
            let mut entry=required(io(volume.lookup(folder.id,name.as_bytes()))?)?;
            if entry.directory {return Err(BAD_ARGUMENT);}
            if entry.data.is_none() {return Err(NOT_FOUND);}
            let catalog=metadata(id,&entry);
            let resources=Self::load_selected_resources(volume,entry.resources.take().ok_or(NOT_FOUND)?,|_,_|true,mac_formats::resource::stream::Budget {payload_bytes,..Default::default()})?;
            Ok(application::Loaded {catalog,resources})
        })
    }

    fn load_selected_resources(volume: &mut Volume<'_, Device>, fork: Fork,
                               select: fn([u8; 4], i16) -> bool,
                               budget: mac_formats::resource::stream::Budget)
                               -> Result<Vec<mac_formats::resource::Resource>, Status>
    {
        use mac_formats::resource::loader::Loader;
        let mut reader = io(volume.fork_reader(&fork))?;
        let mut loader =
            Loader::selected(reader.length(), budget, select).map_err(application_format_status)?;
        while let Some((offset, count)) = loader.request()
        {
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(count).map_err(|_| NO_MEMORY)?;
            bytes.resize(count, 0);
            if io(reader.read(offset, &mut bytes))? != count
            {
                return Err(IO_ERROR);
            }
            loader.received(&bytes).map_err(application_format_status)?;
        }
        loader.finish().map_err(application_format_status)
    }

    #[cfg(systemrisc)]
    pub(crate) fn application_resources_selected(
        &mut self, volume: u32, cnid: u32, selection: super::application::Selection)
        -> Result<(CatalogInfo, Vec<mac_formats::resource::Resource>), Status>
    {
        self.with_volume(volume, |v| {
                let mut e = required(io(v.entry(cnid))?)?;
                let info = metadata(volume, &e);
                if info.kind != FILE || info.file_type != selection.file_type()
                {
                    return Err(BAD_ARGUMENT);
                }
                let fork = e.resources.take().ok_or(NOT_FOUND)?;
                Ok((info,
                    Self::load_selected_resources(v,
                                                  fork,
                                                  selection.includes(),
                                                  selection.budget())?))
            })
    }

    /// Decode a file's custom icon, then its bundle's selected icon family.
    /// A document, which carries no bundle of its own, gets the icon its
    /// creator's application declares for its type. Executable resources are
    /// never loaded by this path.
    pub(crate) fn file_icon(
        &mut self, volume: u32, cnid: u32,
        resident: crate::toolbox::resources::manager::Manager<'_>)
        -> Result<alloc::sync::Arc<crate::toolbox::quickdraw::icons::IconFamily>, Status>
    {
        #[cfg(systemrisc)]
        if matches!(self.volumes.get(volume), Some(Mounted::Provider(_)))
        {
            return self.provider_file_icon(volume, cnid);
        }
        // A cache key never validates an object. Even a hit must reject an
        // unmounted/reused volume or a quarantined device, without storage I/O.
        if self.mounted(volume)?.poisoned
        {
            return Err(ROLLBACK_FAILED);
        }
        if let Some(family) = self.file_icons.get(volume, cnid)
        {
            return Ok(family);
        }
        // Cache misses create shared immutable icon backing at the storage
        // root. Changing authority only when inserting would retain the first
        // requesting application's account in the decoded pixels and masks.
        InvocationGate::with_system_allocation(|| {
            let (parent, own) = self.with_volume(volume, |volume| {
            let entry = required(io(volume.entry(cnid))?)?;
            let catalog = metadata(0, &entry);
            let parent = entry.parent;
            // Files that are programs carry their own bundle. Anything else is
            // a document: only its custom icon, if any, is its own.
            let program = matches!(&catalog.file_type,
                                   b"APPL" | b"INIT" | b"DRVR" | b"FNDR" | b"CLAP");
            let custom = catalog.finder_flags() & 0x0400 != 0;
            let document = Document { creator:   catalog.creator,
                                      file_type: catalog.file_type };
            let Some(mut fork) = entry.resources
            else
            {
                return if program { Err(NOT_FOUND) } else { Ok((parent, Err(document))) };
            };
            let length = fork.length;
            let mut read = |offset, output: &mut [u8]| {
                let count = io(volume.read(&mut fork, offset, output))?;
                if count == output.len() { Ok(()) } else { Err(BAD_ARGUMENT) }
            };
            if custom && let Ok(custom) = read_icon(length, -16455, resident, &mut read)
            {
                return Ok((parent, Ok(custom)));
            }
            if !program
            {
                return Ok((parent, Err(document)));
            }
            let metadata = crate::toolbox::resources::file::read_selected(length,
                                                                          |key| {
                                                                              matches!(&key.kind,
                                                                                       b"BNDL"
                                                                                       | b"FREF")
                                                                          },
                                                                          64 * 1024,
                                                                          &mut read)?;
            let id = icons_service::file_id(&metadata, catalog.creator, catalog.file_type)?;
            Ok((parent, Ok(read_icon(length, id, resident, &mut read)?)))
        })?;
            match own
            {
                Ok(family) =>
                {
                    let family = alloc::sync::Arc::try_new(family).map_err(|_| NO_MEMORY)?;
                    self.file_icons.insert(volume, parent, cnid, family.clone())?;
                    Ok(family)
                }
                Err(document) =>
                {
                    let family = self.document_icon(document, resident)?;
                    self.file_icons.alias(volume, parent, cnid, &family);
                    Ok(family)
                }
            }
        })
    }

    /// The icon the creator's application declares for a document type. One
    /// decoded family per application and type is shared by every such
    /// document. `NOT_FOUND` covers every ordinary reason there is none: no
    /// application owns the creator, or its bundle does not mention the type.
    #[cfg(not(systemrisc))]
    fn document_icon(
        &mut self, _document: Document,
        _resident: crate::toolbox::resources::manager::Manager<'_>)
        -> Result<alloc::sync::Arc<crate::toolbox::quickdraw::icons::IconFamily>, Status>
    {
        Err(NOT_FOUND)
    }

    #[cfg(systemrisc)]
    fn document_icon(
        &mut self, document: Document,
        resident: crate::toolbox::resources::manager::Manager<'_>)
        -> Result<alloc::sync::Arc<crate::toolbox::quickdraw::icons::IconFamily>, Status>
    {
        let owner = match super::applications::owner(document.creator)
        {
            Ok(owner) => owner,
            // Still indexing: say so, so a caller can ask again instead of
            // settling on a generic icon.
            Err(SERVICE_BUSY) => return Err(SERVICE_BUSY),
            Err(_) => return Err(NOT_FOUND)
        };
        let kind = document.file_type;
        if let Some(family) = self.file_icons.get_kind(owner.volume, owner.cnid, kind)
        {
            return Ok(family);
        }
        if self.file_icons.absent(owner.volume, owner.cnid, kind)
        {
            return Err(NOT_FOUND);
        }
        // The owner may be on another volume, and may have gone since the
        // index last looked.
        match self.volumes.get(owner.volume)
        {
            Some(Mounted::Block(mounted)) if !mounted.poisoned => (),
            // A provider answers later. Have it read, and say "not yet".
            Some(Mounted::Provider(_)) =>
            {
                super::provider_icons::want(super::provider_icons::Wanted { volume: owner.volume,
                                                                            file: owner.cnid,
                                                                            kind });
                return Err(SERVICE_BUSY);
            }
            _ => return Err(NOT_FOUND)
        }
        let read = self.with_volume(owner.volume, |volume| {
            let entry = required(io(volume.entry(owner.cnid))?)?;
            let parent = entry.parent;
            let mut fork = entry.resources.ok_or(NOT_FOUND)?;
            let length = fork.length;
            let mut read = |offset, output: &mut [u8]| {
                let count = io(volume.read(&mut fork, offset, output))?;
                if count == output.len() { Ok(()) } else { Err(BAD_ARGUMENT) }
            };
            let metadata = crate::toolbox::resources::file::read_selected(length,
                                                                          |key| {
                                                                              matches!(&key.kind,
                                                                                       b"BNDL"
                                                                                       | b"FREF")
                                                                          },
                                                                          64 * 1024,
                                                                          &mut read)?;
            let id = icons_service::file_id(&metadata, document.creator, kind)?;
            Ok((parent, read_icon(length, id, resident, &mut read)?))
        });
        match read
        {
            Ok((parent, family)) =>
            {
                let family = alloc::sync::Arc::try_new(family).map_err(|_| NO_MEMORY)?;
                self.file_icons.insert_kind(owner.volume, parent, owner.cnid, kind,
                                            family.clone())?;
                Ok(family)
            }
            // The bundle has nothing for this type, or is not one this path
            // can read. Remember it; every document of the type would ask.
            Err(NOT_FOUND | BAD_ARGUMENT | NOT_SUPPORTED) =>
            {
                self.file_icons.note_absent(owner.volume, owner.cnid, kind);
                Err(NOT_FOUND)
            }
            Err(status) => Err(status)
        }
    }

    /// A file's icon on a provider volume: the cached one, a remembered
    /// "none", or "not yet" with the icon now wanted.
    #[cfg(systemrisc)]
    fn provider_file_icon(&mut self, volume: u32, cnid: u32)
                          -> Result<alloc::sync::Arc<icons::IconFamily>, Status>
    {
        // Refuse an unmounted or quarantined volume before any cache answer.
        self.read_provider(volume)?;
        if let Some(family) = self.file_icons.get(volume, cnid)
        {
            return Ok(family);
        }
        if self.file_icons.absent(volume, cnid, super::icon_cache::OWN)
        {
            return Err(NOT_FOUND);
        }
        super::provider_icons::want(super::provider_icons::Wanted { volume,
                                                                    file: cnid,
                                                                    kind:
                                                                        super::icon_cache::OWN });
        Err(SERVICE_BUSY)
    }

    /// Whether an icon that was wanted has since been answered either way.
    #[cfg(systemrisc)]
    pub(super) fn provider_icon_settled(&self, wanted: super::provider_icons::Wanted) -> bool
    {
        self.file_icons.get_kind(wanted.volume, wanted.file, wanted.kind).is_some()
        || self.file_icons.absent(wanted.volume, wanted.file, wanted.kind)
    }

    /// Put what the background read found into the icon cache, and say whose
    /// watchers should look again. A remembered "none" tells nobody: nothing
    /// they show has changed.
    #[cfg(systemrisc)]
    pub(super) fn provider_icon_settle(
        &mut self, wanted: super::provider_icons::Wanted, outcome: super::provider_icons::Outcome,
        resident: crate::toolbox::resources::manager::Manager<'_>)
        -> Result<super::provider_icons::Tell, Status>
    {
        use super::{icon_cache::OWN,
                    provider_icons::{Outcome, Tell}};
        InvocationGate::with_system_allocation(|| {
            let (volume, file, kind) = (wanted.volume, wanted.file, wanted.kind);
            let document = |manager: &mut Self, entry: &CatalogInfo| {
                let document = Document { creator:   entry.creator,
                                          file_type: entry.file_type };
                match manager.document_icon(document, resident)
                {
                    Ok(family) =>
                    {
                        manager.file_icons.alias(volume, entry.parent, file, &family);
                        Ok(Tell::Folder(volume, entry.parent))
                    }
                    // The application's own icon is on its way, or the index
                    // is. Leave this unanswered; the next draw asks again.
                    Err(SERVICE_BUSY) => Ok(Tell::Nobody),
                    Err(_) =>
                    {
                        manager.file_icons.note_absent(volume, file, OWN);
                        Ok(Tell::Nobody)
                    }
                }
            };
            let loaded = match outcome
            {
                Outcome::Failed(_) =>
                {
                    self.file_icons.note_absent(volume, file, kind);
                    return Ok(Tell::Nobody);
                }
                Outcome::Document(entry) => return document(self, &entry),
                Outcome::Loaded(loaded) => loaded
            };
            let entry = loaded.catalog;
            let resources = &loaded.resources;
            let family = |id: i16| {
                icons::IconFamily::from_lookup(id, |kind, rid| {
                    if kind == *b"clut"
                    {
                        resident.get(mac_abi::resources::SYSTEM,
                                     ResourceKey { kind,
                                                   id: rid })
                                .ok()
                                .map(|(_, r)| r.data.as_slice())
                    }
                    else
                    {
                        resources.iter()
                                 .find(|r| r.kind == kind && r.id == rid)
                                 .map(|r| r.data.as_slice())
                    }
                }).map_err(icons_service::status)
            };
            if kind != OWN
            {
                // The icon this application declares for a document type.
                let found = icons_service::file_id(resources, entry.creator, kind).and_then(family);
                return match found
                {
                    Ok(found) =>
                    {
                        let found = alloc::sync::Arc::try_new(found).map_err(|_| NO_MEMORY)?;
                        self.file_icons.insert_kind(volume, entry.parent, file, kind, found)?;
                        // Its documents may be in any folder on any volume.
                        Ok(Tell::Everybody)
                    }
                    Err(_) =>
                    {
                        self.file_icons.note_absent(volume, file, kind);
                        Ok(Tell::Nobody)
                    }
                };
            }
            let program =
                matches!(&entry.file_type, b"APPL" | b"INIT" | b"DRVR" | b"FNDR" | b"CLAP");
            let custom = entry.finder_flags() & 0x0400 != 0;
            let mut own = if custom { family(-16455).ok() } else { None };
            if own.is_none() && program
            {
                own = icons_service::file_id(resources, entry.creator, entry.file_type)
                    .and_then(family)
                    .ok();
            }
            match own
            {
                Some(own) =>
                {
                    let own = alloc::sync::Arc::try_new(own).map_err(|_| NO_MEMORY)?;
                    self.file_icons.insert(volume, entry.parent, file, own)?;
                    Ok(Tell::Folder(volume, entry.parent))
                }
                None if program =>
                {
                    self.file_icons.note_absent(volume, file, OWN);
                    Ok(Tell::Nobody)
                }
                None => document(self, &entry)
            }
        })
    }

    pub(crate) fn resource_value(&mut self, volume: u32, cnid: u32, key: ResourceKey)
                                 -> Result<Vec<u8>, Status>
    {
        self.with_volume(volume, |volume| {
                let entry = required(io(volume.entry(cnid))?)?;
                let mut fork = entry.resources.ok_or(NOT_FOUND)?;
                let length = fork.length;
                crate::toolbox::resources::file::read_value(length, key, |offset, output| {
                    let read = io(volume.read(&mut fork, offset, output))?;
                    if read == output.len() { Ok(()) } else { Err(BAD_ARGUMENT) }
                })
            })
    }
}

/// What a file with no icon of its own is, as far as choosing one goes.
#[derive(Clone, Copy)]
struct Document
{
    creator:   [u8; 4],
    file_type: [u8; 4]
}

#[cfg(systemrisc)]
pub(crate) struct ModuleCandidate
{
    pub directory: mac_module::PackageKind,
    pub volume:    u32,
    pub cnid:      u32,
    pub file_type: [u8; 4],
    pub name:      Vec<u8>
}
#[cfg(systemrisc)]
impl Manager
{
    /// Discover metadata only in the selected blessed installation. This never
    /// enters DRVR or System RV64/0 code, including misplaced packages. Finder
    /// RV64/1 is never selected by this resident discovery path.
    pub(crate) fn module_candidates(&mut self) -> Result<Vec<ModuleCandidate>, Status>
    {
        let id = self.volumes
                     .iter()
                     .find(|(_, v)| v.info().is_some_and(|info| info.startup != 0))
                     .map(|(id, _)| id)
                     .ok_or(NOT_FOUND)?;
        self.with_volume(id, |volume| {
                let system = io(volume.blessed_folder())?;
                let mut candidates = Vec::new();
                for (name, directory) in [(b"Drivers".as_slice(), mac_module::PackageKind::Driver),
                                          (b"Extensions".as_slice(),
                                           mac_module::PackageKind::Extension)]
                {
                    let Some(folder) = io(volume.lookup(system.id, name))?
                    else
                    {
                        continue;
                    };
                    if !folder.directory
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    for index in 0..=256
                    {
                        let Some(entry) = io(volume.child(folder.id, index))?
                        else
                        {
                            break;
                        };
                        if index == 256
                        {
                            serial!("MODULE_DISCOVERY_LIMIT directory={} max_entries=256; reduce \
                                     this installation directory before restarting",
                                    core::str::from_utf8(name).unwrap_or("?"));
                            return Err(NO_MEMORY);
                        }
                        if entry.directory
                        {
                            continue;
                        }
                        let file_type =
                            entry.finder_info[..4].try_into().map_err(|_| BAD_ARGUMENT)?;
                        if ![*b"DRVR", *b"INIT"].contains(&file_type)
                        {
                            continue;
                        }
                        // The bounded directory walk limits discovery work;
                        // candidate storage itself follows real admission. An
                        // unrelated package-count cap must not impersonate
                        // heap exhaustion on an extensible installation.
                        candidates.try_reserve(1).map_err(|_| NO_MEMORY)?;
                        candidates.push(ModuleCandidate { directory,
                                                          volume: id,
                                                          cnid: entry.id,
                                                          file_type,
                                                          name: entry.name });
                    }
                }
                Ok(candidates)
            })
    }
}

#[cfg(systemrisc)]
impl Manager
{
    #[cfg(systemrisc)]
    pub(crate) fn module_metadata(&mut self, volume: u32, cnid: u32) -> Result<Vec<u8>, Status>
    {
        let mut resources = self.module_projected(volume, cnid, true).map_err(|cause| {
            serial!("SYSTEM_MODULE_METADATA_REFUSED: {cause}");
            cause.status()
        })?;
        resources.pop().map(|resource| resource.data).ok_or(BAD_ABI)
    }

    #[cfg(systemrisc)]
    pub(crate) fn module_resources(&mut self, volume: u32, cnid: u32)
                                   -> Result<Vec<mac_formats::resource::Resource>, Status>
    {
        self.module_resources_checked(volume, cnid).map_err(|cause| {
                                                       serial!("SYSTEM_MODULE_PACKAGE_REFUSED: \
                                                                {cause}");
                                                       cause.status()
                                                   })
    }

    pub(crate) fn module_resources_checked(
        &mut self, volume: u32, cnid: u32)
        -> Result<Vec<mac_formats::resource::Resource>, ModuleReadError>
    {
        self.module_projected(volume, cnid, false)
    }

    fn module_projected(&mut self, volume: u32, cnid: u32, metadata_only: bool)
                        -> Result<Vec<mac_formats::resource::Resource>, ModuleReadError>
    {
        self.with_volume(volume, |volume| {
                let entry = required(io(volume.entry(cnid))?)?;
                if entry.directory
                {
                    return Err(BAD_ARGUMENT);
                }
                let file_type = entry.finder_info[..4].try_into().map_err(|_| BAD_ARGUMENT)?;
                let mut fork = entry.resources.ok_or(NOT_FOUND)?;
                Ok(project_module(fork.length, file_type, metadata_only, |offset, output| {
                    if io(volume.read(&mut fork, offset, output))? == output.len()
                    {
                        Ok(())
                    }
                    else
                    {
                        Err(IO_ERROR)
                    }
                }))
            })
            .map_err(ModuleReadError::Storage)?
    }
}


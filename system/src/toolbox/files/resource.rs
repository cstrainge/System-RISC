//! Bounded, read-only access to one disk resource; executable data stays on disk.
use super::*;
use crate::toolbox::quickdraw::{icons, icons_service};

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

#[cfg(target_arch = "riscv64")]
pub(crate) struct ProviderInspection
{
    pub provider: u32,
    pub volume:   u32,
    pub fork:     u32,
    pub offset:   u64,
    pub length:   u64,
    pub index:    Option<mac_formats::resource::index::Index>
}
#[cfg(target_arch = "riscv64")]
impl Manager
{
    #[cfg(target_arch = "riscv64")]
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

    pub(crate) fn resource_provider(&self, owner: u64, handle: u32) -> Result<Option<u32>, Status>
    {
        self.file_provider(owner, handle)
    }

    pub(crate) fn cached_resource_query(&mut self, owner: u64,
                                        call: &mut mac_abi::resources::FileResourceCall,
                                        name: &str, capacity: usize)
                                        -> Result<bool, Status>
    {
        let file = self.slot(owner, call.handle)?;
        let length = match file.fork.length()
        {
            Ok(length) => length,
            Err(SERVICE_BUSY) => return Ok(false),
            Err(error) => return Err(error)
        };
        if file.resource_index.is_none() && length == 0
        {
            file.resource_index = Some(mac_formats::resource::index::Index::empty());
        }
        let Some(index) = &file.resource_index
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
        let length = self.slot(owner, handle)?.fork.length()?;
        // A composite positioned read holds the normal read lease but leaves
        // the public mark's position untouched. No provider callback runs here.
        let (provider, args) = self.begin_provider_file_read(owner, handle, 0)?;
        let index = self.slot(owner, handle)?.resource_index.take();
        Ok(ProviderInspection { provider,
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
        self.finish_provider_file_read(handle, offset, 0, None)?;
        let file = self.slot(owner, handle)?;
        if !file.fork.orphaned()
        {
            file.resource_index = index;
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


#[cfg_attr(not(target_arch = "riscv64"), allow(dead_code))]
impl Manager
{
    pub(crate) fn take_resource_index(
        &mut self, owner: u64, handle: u32)
        -> Result<Option<mac_formats::resource::index::Index>, Status>
    {
        let file = self.slot(owner, handle)?;
        file.fork.length()?;
        Ok(file.resource_index.take())
    }

    pub(crate) fn restore_resource_index(&mut self, owner: u64, handle: u32,
                                         index: mac_formats::resource::index::Index)
                                         -> Result<(), Status>
    {
        let file = self.slot(owner, handle)?;
        file.fork.length()?;
        file.resource_index = Some(index);
        Ok(())
    }

    /// Startup reads use the same mounted filesystem service as later launches.
    /// ROM supplies block I/O, never an opaque whole-fork copying callback.
    pub(crate) fn startup_resources(&mut self, name: &str)
                                    -> Result<Vec<mac_formats::resource::Resource>, Status>
    {
        let id = self.volumes
                     .iter()
                     .find(|(_, v)| v.info().is_some_and(|info| info.startup != 0))
                     .map(|(id, _)| id)
                     .ok_or(NOT_FOUND)?;
        self.with_volume(id, |volume| {
                let folder = io(volume.blessed_folder())?;
                let file = io(volume.find_file(folder.id, name.as_bytes()))?;
                Self::load_resources(volume, file.resources)
            })
    }

    fn load_resources(volume: &mut Volume<'_, Device>, fork: Fork)
                      -> Result<Vec<mac_formats::resource::Resource>, Status>
    {
        Self::load_selected_resources(volume,
                                      fork,
                                      |_, _| true,
                                      mac_formats::resource::stream::Budget::default())
    }

    fn load_selected_resources(volume: &mut Volume<'_, Device>, mut fork: Fork,
                               select: fn([u8; 4], i16) -> bool,
                               budget: mac_formats::resource::stream::Budget)
                               -> Result<Vec<mac_formats::resource::Resource>, Status>
    {
        use mac_formats::resource::loader::Loader;
        let mut loader =
            Loader::selected(fork.length, budget, select).map_err(application_format_status)?;
        while let Some((offset, count)) = loader.request()
        {
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(count).map_err(|_| NO_MEMORY)?;
            bytes.resize(count, 0);
            if io(volume.read(&mut fork, offset, &mut bytes))? != count
            {
                return Err(IO_ERROR);
            }
            loader.received(&bytes).map_err(application_format_status)?;
        }
        loader.finish().map_err(application_format_status)
    }

    pub(crate) fn application_resources(
        &mut self, volume: u32, cnid: u32)
        -> Result<(CatalogInfo, Vec<mac_formats::resource::Resource>), Status>
    {
        self.with_volume(volume, |v| {
                let mut e = required(io(v.entry(cnid))?)?;
                let info = metadata(volume, &e);
                if info.kind != FILE || info.file_type != *b"APPL"
                {
                    return Err(BAD_ARGUMENT);
                }
                let fork = e.resources.take().ok_or(NOT_FOUND)?;
                Ok((info, Self::load_resources(v, fork)?))
            })
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn application_resources_selected(
        &mut self, volume: u32, cnid: u32, selection: super::application::Selection)
        -> Result<(CatalogInfo, Vec<mac_formats::resource::Resource>), Status>
    {
        self.with_volume(volume, |v| {
                let mut e = required(io(v.entry(cnid))?)?;
                let info = metadata(volume, &e);
                if info.kind != FILE || info.file_type != *b"APPL"
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
    /// Executable resources are never loaded by this path.
    pub(crate) fn file_icon(&mut self, volume: u32, cnid: u32,
                            resident: crate::toolbox::resources::manager::Manager<'_>)
                            -> Result<crate::toolbox::quickdraw::icons::IconFamily, Status>
    {
        self.with_volume(volume, |volume| {
            let entry = required(io(volume.entry(cnid))?)?;
            let catalog = metadata(0, &entry);
            let mut fork = entry.resources.ok_or(NOT_FOUND)?;
            let length = fork.length;
            let mut read = |offset, output: &mut [u8]| {
                let count = io(volume.read(&mut fork, offset, output))?;
                if count == output.len() { Ok(()) } else { Err(BAD_ARGUMENT) }
            };
            if catalog.finder_flags() & 0x0400 != 0
               && let Ok(custom) = read_icon(length, -16455, resident, &mut read)
            {
                return Ok(custom);
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
            read_icon(length, id, resident, &mut read)
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

#[cfg(target_arch = "riscv64")]
pub(crate) struct ModuleCandidate
{
    pub directory: mac_module::PackageKind,
    pub volume:    u32,
    pub cnid:      u32,
    pub file_type: [u8; 4],
    pub name:      Vec<u8>
}
#[cfg(target_arch = "riscv64")]
impl Manager
{
    /// Discover metadata only in the selected blessed installation. This never
    /// enters DRVR/INIT code, including packages placed in the wrong directory.
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
                        if candidates.len() >= 32
                        {
                            return Err(NO_MEMORY);
                        }
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

    pub(crate) fn module_metadata(&mut self, volume: u32, cnid: u32) -> Result<Vec<u8>, Status>
    {
        self.with_volume(volume, |volume| {
                let entry = required(io(volume.entry(cnid))?)?;
                let mut fork = entry.resources.ok_or(NOT_FOUND)?;
                let mut metadata =
                    crate::toolbox::resources::file::read_selected(fork.length,
                                                                   |key| {
                                                                       key.kind == *b"DABI"
                                                                       || key.kind == *b"EABI"
                                                                   },
                                                                   mac_module::MAX_BYTES,
                                                                   |offset, output| {
                                                                       if io(volume.read(&mut fork,
                                                                                     offset,
                                                                                     output))?
                                                                      == output.len()
                                                                   {
                                                                       Ok(())
                                                                   }
                                                                   else
                                                                   {
                                                                       Err(IO_ERROR)
                                                                   }
                                                                   })?;
                if metadata.len() != 1 || metadata[0].id != 0 || metadata[0].attributes & 1 != 0
                {
                    return Err(BAD_ABI);
                }
                Ok(metadata.remove(0).data)
            })
    }

    pub(crate) fn module_resources(&mut self, volume: u32, cnid: u32)
                                   -> Result<Vec<mac_formats::resource::Resource>, Status>
    {
        self.with_volume(volume, |volume| {
                let entry = required(io(volume.entry(cnid))?)?;
                if entry.directory
                   || ![*b"DRVR", *b"INIT"].contains(&entry.finder_info[..4].try_into()
                                                                            .map_err(|_| {
                                                                                BAD_ARGUMENT
                                                                            })?)
                {
                    return Err(BAD_ARGUMENT);
                }
                let mut fork = entry.resources.ok_or(NOT_FOUND)?;
                crate::toolbox::resources::file::read_selected(fork.length,
                                                               |key| {
                                                                   [*b"DRVR", *b"INIT", *b"DABI",
                                                                    *b"EABI", *b"NABI", *b"RV64",
                                                                    *b"RLOC", *b"FASS", *b"ICN#", *b"icl4", *b"icl8",
                                                                    *b"ics#", *b"ics4", *b"ics8", *b"ICON", *b"SICN", *b"cicn"].contains(&key.kind)
                                                               },
                                                               2 * 1024 * 1024
                                                               + mac_module::MAX_BYTES,
                                                               |offset, output| {
                                                                   if io(volume.read(&mut fork,
                                                                                     offset,
                                                                                     output))?
                                                                      == output.len()
                                                                   {
                                                                       Ok(())
                                                                   }
                                                                   else
                                                                   {
                                                                       Err(IO_ERROR)
                                                                   }
                                                               })
            })
    }
}

#[cfg(test)]
mod typed_error_tests
{
    use super::*;
    #[test]
    fn status_uses_cause_even_when_the_message_uses_misleading_vocabulary()
    {
        use mac_formats::Error;
        for message in ["limit exceeded", "alloc failed", "absent entry",
                        "allocation budget missing translation"]
        {
            assert_eq!(application_format_status(Error::BudgetExceeded(message)), NO_MEMORY);
            assert_eq!(application_format_status(Error::AllocationFailed(message)), NO_MEMORY);
            assert_eq!(application_format_status(Error::Missing(message)), NOT_FOUND);
            assert_eq!(application_format_status(Error::TranslationRequired(message)),
                       TRANSLATION_REQUIRED);
            assert_eq!(application_format_status(Error::Malformed(message)), CORRUPT_DATA);
        }
    }
}

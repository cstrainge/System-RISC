//! Bounded, read-only access to one disk resource; executable data stays on disk.
use super::*;
use crate::toolbox::quickdraw::{icons, icons_service};

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


impl Manager
{
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

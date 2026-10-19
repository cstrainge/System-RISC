//! Bounded, read-only access to one disk resource; executable data stays on disk.
use super::*;

impl Manager
{
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

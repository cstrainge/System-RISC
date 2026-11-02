//! Read-only inspection snapshots do not install fonts or issue selectable IDs.
use mac_abi::quickdraw::*;
use mac_traps::{Client, Error, Result, types::InlineText};
use toolbox_files::{Cnid, Files, Fork, VolumeId};

pub struct FontSuitcase<'a>
{
    client: Client<'a>,
    id:     u64,
    count:  u32
}
pub struct SuitcaseFamily
{
    name: InlineText<256>
}
impl SuitcaseFamily
{
    pub fn name(&self) -> &str
    {
        self.name.as_str()
    }
}
fn read(preview: &FontSuitcase<'_>, files: Files<'_>, volume: VolumeId, id: Cnid, fork: Fork,
        length: u64)
        -> Result<()>
{
    let length = usize::try_from(length).map_err(|_| Error::NoMemory)?;
    if length == 0
    {
        return Ok(());
    }
    let mut bytes = [0; 32 * 1024];
    let mut file = files.open(volume, id, fork)?;
    if file.len()? != length as u64
    {
        return Err(Error::ScanStale);
    }
    let mut position = 0;
    while position != length
    {
        let wanted = bytes.len().min(length - position);
        let count = file.read(&mut bytes[..wanted])?;
        if count == 0
        {
            return Err(Error::ScanStale);
        }
        let operation = match fork
        {
            Fork::Data => FONT_SUITCASE_APPEND_DATA,
            Fork::Resource => FONT_SUITCASE_APPEND_RESOURCE
        };
        preview.client.font_suitcase(&mut FontSuitcaseCall { operation,
                                                              id: preview.id,
                                                              ..Default::default() },
                                      &bytes[..count],
                                      &[])?;
        position += count;
    }
    if file.read(&mut [0; 1])? != 0
    {
        return Err(Error::ScanStale);
    }
    file.close()?;
    Ok(())
}
impl<'a> super::Fonts<'a>
{
    /// Inspect a suitcase or standalone TrueType file without installing it.
    pub fn inspect_file(self, volume: VolumeId, id: Cnid) -> Result<FontSuitcase<'a>>
    {
        let files = Files::new(self.client);
        let entry = files.catalog(volume, id)?;
        if entry.kind != toolbox_files::EntryKind::File
        {
            return Err(Error::BadArgument);
        }
        let length = entry.data_length.checked_add(entry.resource_length).ok_or(Error::NoMemory)?;
        if length == 0 || length > FONT_SUITCASE_MAX_BYTES as u64
        {
            return Err(if length == 0 { Error::CorruptData } else { Error::NoMemory });
        }
        let mut call = FontSuitcaseCall { expected_data: entry.data_length as u32,
                                          expected_resource: entry.resource_length as u32,
                                          ..Default::default() };
        self.client.font_suitcase(&mut call, &[], &[])?;
        let mut preview = FontSuitcase { client: self.client,
                                         id:     call.id,
                                         count:  0 };
        read(&preview, files, volume, id, Fork::Data, entry.data_length)?;
        read(&preview, files, volume, id, Fork::Resource, entry.resource_length)?;
        let after = files.catalog(volume, id)?;
        if entry.modified != after.modified
           || entry.data_length != after.data_length
           || entry.resource_length != after.resource_length
        {
            return Err(Error::ScanStale);
        }
        let mut call = FontSuitcaseCall { operation: FONT_SUITCASE_FINISH,
                                          id: preview.id,
                                          ..Default::default() };
        self.client.font_suitcase(&mut call, &[], &[])?;
        preview.count = call.count;
        Ok(preview)
    }
}
impl FontSuitcase<'_>
{
    pub fn count(&self) -> u32
    {
        self.count
    }

    pub fn family(&self, index: u32) -> Result<SuitcaseFamily>
    {
        let mut call = FontSuitcaseCall { operation: FONT_SUITCASE_FAMILY,
                                          id: self.id,
                                          index,
                                          ..Default::default() };
        self.client.font_suitcase(&mut call, &[], &[])?;
        Ok(SuitcaseFamily { name: InlineText::from_reply(call.name, call.name_length)? })
    }

    pub fn dispose(mut self) -> Result<()>
    {
        self.close()
    }

    fn close(&mut self) -> Result<()>
    {
        if self.id != 0
        {
            self.client.font_suitcase(&mut FontSuitcaseCall { operation:
                                                                   FONT_SUITCASE_DISPOSE,
                                                               id: self.id,
                                                               ..Default::default() },
                                       &[],
                                       &[])?;
            self.id = 0;
        }
        Ok(())
    }
}
impl Drop for FontSuitcase<'_>
{
    fn drop(&mut self)
    {
        let _ = self.close();
    }
}

//! Canonical public SDK assertions shared by host and actual ECALL disk boots.
//! The disposable fixture has a root file `Write Sample` with both known forks.
use mac_toolbox::{Error, System, Timestamp,
                  files::{Fork, ROOT_ID}};

#[derive(Debug)]
pub enum Failure
{
    Service(i32),
    Mismatch(&'static str)
}
impl From<Error> for Failure
{
    fn from(error: Error) -> Self
    {
        Self::Service(error.status())
    }
}
impl core::fmt::Display for Failure
{
    fn fmt(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Service(status) => write!(out, "service status {status}"),
            Self::Mismatch(field) => write!(out, "mismatch: {field}")
        }
    }
}
fn require(test: bool, what: &'static str) -> Result<(), Failure>
{
    if test { Ok(()) } else { Err(Failure::Mismatch(what)) }
}
const WHEN: Timestamp = Timestamp::from_unix_seconds(1_792_326_896);
const FOLDER: &str = "Write conformance";
const DONE: &str = "Write completed";
const RENAMED_SAMPLE: &str = "Renamed Write Sample";
struct CommentText
{
    bytes: [u8; 768],
    len:   usize
}
impl core::fmt::Write for CommentText
{
    fn write_str(&mut self, text: &str) -> core::fmt::Result
    {
        if text.len() > self.bytes.len() - self.len
        {
            return Err(core::fmt::Error);
        }
        self.bytes[self.len..self.len + text.len()].copy_from_slice(text.as_bytes());
        self.len += text.len();
        Ok(())
    }
}
impl CommentText
{
    fn text(&self) -> &str
    {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap()
    }
}
fn comment(system: &System<'_>, volume: mac_toolbox::VolumeId, id: u32)
           -> Result<CommentText, Failure>
{
    let mut output = CommentText { bytes: [0; 768],
                                   len:   0 };
    system.files().comment(volume, mac_toolbox::Cnid(id), &mut output)?;
    Ok(output)
}

fn payload(system: &System<'_>, volume: mac_toolbox::VolumeId, id: u32) -> Result<(), Failure>
{
    for (fork, size, multiplier) in [(Fork::Data, 1700usize, 17usize), (Fork::Resource, 900, 31)]
    {
        let mut file = system.files().open(volume, mac_toolbox::Cnid(id), fork)?;
        require(file.len()? == size as u64, "fork logical size")?;
        let mut data = [0u8; 2048];
        require(file.read(&mut data)? == size, "fork complete read")?;
        require(data[..size].iter().enumerate().all(|(i, &byte)| byte == (i * multiplier) as u8),
                "fork payload preserved")?;
    }
    Ok(())
}
/// One of three independent boots. All stages use the same generated disk.
pub fn stage(system: &System<'_>) -> Result<u8, Failure>
{
    let files = system.files();
    let volume = files.volume_at(0)?;
    require(!volume.read_only, "startup volume writable")?;
    if let Ok(done) = files.lookup(volume.id, ROOT_ID, DONE)
    {
        require(comment(system, volume.id, (done.id).0)?.text() == "Boot two sentinel",
                "updated comment persisted through third boot")?;
        files.set_comment_at(volume.id, done.id, "", WHEN)?;
        system.log("FILE_COMMENTS_UPDATED_PERSISTED\n")?;
        require(files.lookup(volume.id, ROOT_ID, FOLDER) == Err(Error::NotFound),
                "removed folder absent after second remount")?;
        require(files.lookup(volume.id, ROOT_ID, "Write Sample") == Err(Error::NotFound),
                "deleted file absent after second remount")?;
        require(files.trash_usage(volume.id)?.items == 0, "Trash empty after second remount")?;
        files.move_to_trash_at(volume.id, done.id, WHEN)?;
        require(files.empty_trash_at(volume.id, WHEN)? == 1, "final sentinel deletion")?;
        return Ok(3);
    }
    if let Ok(folder) = files.lookup(volume.id, ROOT_ID, FOLDER)
    {
        let trash = files.trash_directory(volume.id, false)?;
        require(trash.finder_flags().bits() & 0x1000 != 0
                && trash.finder_flags().bits() & 0x4000 == 0,
                "original Trash folder flags")?;
        let child = files.lookup(volume.id, trash.id, "Café")?;
        require(child.put_away_parent() == folder.id, "saved folder parent persisted")?;
        let sample = files.lookup(volume.id, trash.id, RENAMED_SAMPLE)?;
        require(comment(system, volume.id, (sample.id).0)?.text()
                == "Guest Café\rPersistent comment",
                "file comment persisted through boot")?;
        require(comment(system, volume.id, (folder.id).0)?.text() == "Guest folder comment",
                "folder comment persisted through boot")?;
        system.log("FILE_COMMENTS_PERSISTED\n")?;
        require(sample.put_away_parent() == ROOT_ID, "saved file parent persisted")?;
        require(sample.flags & mac_toolbox::files::CatalogFlags::from_bits_retain(1)
                != mac_toolbox::files::CatalogFlags::from_bits_retain(0)
                && sample.finder_flags().bits() & 0x0800 != 0,
                "catalog lock and stationery persisted across real boot")?;
        let allocation = files.allocation_info(volume.id, sample.id)?;
        require(files.trash_usage(volume.id)?.allocated_bytes
                == allocation.data_bytes + allocation.resource_bytes,
                "persisted exact physical fork allocation")?;
        payload(system, volume.id, (sample.id).0)?;
        files.set_locked_at(volume.id, sample.id, false, WHEN)?;
        files.set_stationery_at(volume.id, sample.id, false, WHEN)?;
        files.set_comment_at(volume.id, sample.id, "Updated comment", WHEN)?;
        require(comment(system, volume.id, (sample.id).0)?.text() == "Updated comment",
                "comment replacement")?;
        files.set_comment_at(volume.id, sample.id, "", WHEN)?;
        files.set_comment_at(volume.id, folder.id, "", WHEN)?;
        system.log("FILE_INFO_EDITS_PERSISTED\n")?;
        let restored = files.put_away_at(volume.id, child.id, WHEN)?;
        require(restored.parent == folder.id && restored.put_away_parent() == folder.id,
                "Put Away retains original parent field")?;
        let restored_sample = files.put_away_at(volume.id, sample.id, WHEN)?;
        require(restored_sample.parent == ROOT_ID, "file restored")?;
        files.rename_at(volume.id, sample.id, "Write Sample", WHEN)?;
        payload(system, volume.id, (sample.id).0)?;
        files.move_to_trash_at(volume.id, sample.id, WHEN)?;
        files.move_to_trash_at(volume.id, folder.id, WHEN)?;
        let usage = files.trash_usage(volume.id)?;
        require(usage.items == 3 && usage.allocated_bytes >= 2600, "recursive Trash usage")?;
        let free = files.volume(volume.id)?.capacity.unwrap().free_bytes;
        require(files.empty_trash_at(volume.id, WHEN)? == 3, "recursive delete count")?;
        require(files.volume(volume.id)?.capacity.unwrap().free_bytes
                == free + usage.allocated_bytes,
                "actual allocation blocks released")?;
        let done = files.new_folder_at(volume.id, ROOT_ID, DONE, WHEN)?;
        files.set_comment_at(volume.id, done.id, "Boot two sentinel", WHEN)?;
        return Ok(2);
    }
    let sample = files.lookup(volume.id, ROOT_ID, "Write Sample")?;
    payload(system, volume.id, (sample.id).0)?;
    let folder = files.new_folder_at(volume.id, ROOT_ID, "Write before rename", WHEN)?;
    let folder = files.rename_at(volume.id, folder.id, FOLDER, WHEN)?;
    let renamed = files.rename_at(volume.id, sample.id, RENAMED_SAMPLE, WHEN)?;
    require(renamed.id == sample.id
            && renamed.created == sample.created
            && renamed.modified == sample.modified
            && renamed.finder_info == sample.finder_info,
            "rename preserves file identity, dates and Finder info")?;
    files.set_stationery_at(volume.id, sample.id, true, WHEN)?;
    files.set_comment_at(volume.id, sample.id, "Guest Café\rPersistent comment", WHEN)?;
    files.set_comment_at(volume.id, folder.id, "Guest folder comment", WHEN)?;
    require(files.catalog(volume.id, sample.id)?.modified == sample.modified,
            "comment does not alter target modification date")?;
    system.log("FILE_COMMENTS_COMMITTED\n")?;
    let allocation = files.allocation_info(volume.id, sample.id)?;
    require(allocation.data_bytes >= sample.data_length
            && allocation.resource_bytes >= sample.resource_length,
            "exact physical lengths include both logical forks")?;
    require(folder.created_at() == WHEN && folder.modified_at() == WHEN, "new folder timestamp")?;
    let child = files.new_folder_at(volume.id, folder.id, "Café", WHEN)?;
    files.move_to_trash_at(volume.id, child.id, WHEN)?;
    files.move_to_trash_at(volume.id, sample.id, WHEN)?;
    files.set_locked_at(volume.id, sample.id, true, WHEN)?;
    require(files.trash_usage(volume.id)?.allocated_bytes
            == allocation.data_bytes + allocation.resource_bytes,
            "physical allocation matches independently summed Trash bytes")?;
    system.log("FILE_INFO_EDITS_COMMITTED\n")?;
    require(files.trash_usage(volume.id)?.items == 2,
            "first boot Trash contains both moved items")?;
    Ok(1)
}

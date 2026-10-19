//! Canonical public SDK assertions shared by host and actual ECALL disk boots.
//! The disposable fixture has a root file `Write Sample` with both known forks.
use mac_toolbox::{Error, NOT_FOUND, System, Timestamp,
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
        Self::Service(error.0)
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
fn payload(system: &System<'_>, volume: u32, id: u32) -> Result<(), Failure>
{
    for (fork, size, multiplier) in [(Fork::Data, 1700usize, 17usize), (Fork::Resource, 900, 31)]
    {
        let mut file = system.files().open(volume, id, fork)?;
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
    require(volume.read_only == 0, "startup volume writable")?;
    if let Ok(done) = files.lookup(volume.id, ROOT_ID, DONE)
    {
        require(files.lookup(volume.id, ROOT_ID, FOLDER) == Err(Error(NOT_FOUND)),
                "removed folder absent after second remount")?;
        require(files.lookup(volume.id, ROOT_ID, "Write Sample") == Err(Error(NOT_FOUND)),
                "deleted file absent after second remount")?;
        require(files.trash_usage(volume.id)?.items == 0, "Trash empty after second remount")?;
        files.move_to_trash(volume.id, done.id, WHEN)?;
        require(files.empty_trash(volume.id, WHEN)? == 1, "final sentinel deletion")?;
        return Ok(3);
    }
    if let Ok(folder) = files.lookup(volume.id, ROOT_ID, FOLDER)
    {
        let trash = files.trash_directory(volume.id, false)?;
        require(trash.finder_flags() & 0x1000 != 0 && trash.finder_flags() & 0x4000 == 0,
                "original Trash folder flags")?;
        let child = files.lookup(volume.id, trash.id, "Café")?;
        require(child.put_away_parent() == folder.id, "saved folder parent persisted")?;
        let sample = files.lookup(volume.id, trash.id, "Write Sample")?;
        require(sample.put_away_parent() == ROOT_ID, "saved file parent persisted")?;
        payload(system, volume.id, sample.id)?;
        let restored = files.put_away(volume.id, child.id, WHEN)?;
        require(restored.parent == folder.id && restored.put_away_parent() == folder.id,
                "Put Away retains original parent field")?;
        let restored_sample = files.put_away(volume.id, sample.id, WHEN)?;
        require(restored_sample.parent == ROOT_ID, "file restored")?;
        payload(system, volume.id, sample.id)?;
        files.move_to_trash(volume.id, sample.id, WHEN)?;
        files.move_to_trash(volume.id, folder.id, WHEN)?;
        let usage = files.trash_usage(volume.id)?;
        require(usage.items == 3 && usage.allocated_bytes >= 2600, "recursive Trash usage")?;
        let free = files.volume(volume.id)?.free_bytes;
        require(files.empty_trash(volume.id, WHEN)? == 3, "recursive delete count")?;
        require(files.volume(volume.id)?.free_bytes == free + usage.allocated_bytes,
                "actual allocation blocks released")?;
        files.new_folder(volume.id, ROOT_ID, DONE, WHEN)?;
        return Ok(2);
    }
    let sample = files.lookup(volume.id, ROOT_ID, "Write Sample")?;
    payload(system, volume.id, sample.id)?;
    let folder = files.new_folder(volume.id, ROOT_ID, FOLDER, WHEN)?;
    require(folder.created_at() == WHEN && folder.modified_at() == WHEN, "new folder timestamp")?;
    let child = files.new_folder(volume.id, folder.id, "Café", WHEN)?;
    files.move_to_trash(volume.id, child.id, WHEN)?;
    files.move_to_trash(volume.id, sample.id, WHEN)?;
    require(files.trash_usage(volume.id)?.items == 2,
            "first boot Trash contains both moved items")?;
    Ok(1)
}

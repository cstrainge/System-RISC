//! Plain host entries retain their bytes/names. Catalog identity is backed by
//! an open object pin, so inode reuse cannot alias a deleted session object.
#![forbid(unsafe_code)]
use super::*;
pub(super) struct Node
{
    pin:    File,
    export: usize
}
fn same_object(a: &Metadata, b: &Metadata) -> Result<bool>
{
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(a.dev() == b.dev() && a.ino() == b.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        Err(Failure(status::UNSUPPORTED))
    }
}
fn date(time: std::time::SystemTime) -> Result<u64>
{
    use std::time::UNIX_EPOCH;
    let (s, us) = match time.duration_since(UNIX_EPOCH)
    {
        Ok(d) =>
        {
            (i64::try_from(d.as_secs()).map_err(|_| Failure(status::INVALID))?, d.subsec_micros())
        }
        Err(error) =>
        {
            let d = error.duration();
            let s = i64::try_from(d.as_secs()).map_err(|_| Failure(status::INVALID))?;
            if d.subsec_nanos() == 0
            {
                (-s, 0)
            }
            else
            {
                (-s.checked_add(1).ok_or(Failure(status::INVALID))?,
                 1_000_000 - d.subsec_nanos().div_ceil(1000))
            }
        }
    };
    mac_hfs_b::time::Timestamp::from_unix(s, us).map(|t| t.0).map_err(|_| Failure(status::INVALID))
}
impl Session
{
    pub(super) fn describe(&mut self, parent: u32, name: &str, reply: &mut Header)
                           -> Result<Vec<u8>>
    {
        let directory = self.directory(parent)?;
        let kind = directory.root.symlink_metadata(name)?.file_type();
        if !kind.is_file() && !kind.is_dir()
        {
            return Err(Failure(status::INVALID));
        }
        let pin = directory.root.open(name)?.into_std();
        let metadata = pin.metadata()?;
        if metadata.is_file() != kind.is_file() || metadata.is_dir() != kind.is_dir()
        {
            return Err(Failure(status::STALE));
        }
        let mut found = None;
        for (id, node) in self.nodes.iter()
        {
            if node.export == directory.export && same_object(&node.pin.metadata()?, &metadata)?
            {
                found = Some(id);
                break;
            }
        }
        let id = match found
        {
            Some(id) => id,
            None =>
            {
                self.nodes.insert(Node { pin,
                                          export: directory.export })?
            }
        };
        reply.object = id;
        reply.value = if metadata.is_dir() { 2 } else { 1 };
        reply.offset = if metadata.is_file() { metadata.len() } else { 0 };
        let modified = metadata.modified()?;
        Ok(wire::Description { created:  date(metadata.created().unwrap_or(modified))?,
                               modified: date(modified)? }.encode()
                                                          .to_vec())
    }
}

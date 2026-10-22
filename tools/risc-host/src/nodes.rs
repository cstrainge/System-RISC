//! Plain host entries retain their bytes/names. Catalog identity is backed by
//! an open object pin, so inode reuse cannot alias a deleted session object.
#![forbid(unsafe_code)]
use super::*;
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Key
{
    export:   usize,
    device:   u64,
    inode:    u64,
    identity: Option<wire::ObjectIdentity>
}
impl Key
{
    fn new(export: usize, metadata: &Metadata, identity: Option<wire::ObjectIdentity>)
           -> Result<Self>
    {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self { export,
                      device: metadata.dev(),
                      inode: metadata.ino(),
                      identity })
        }
        #[cfg(not(unix))]
        {
            let _ = (export, metadata, identity);
            Err(Failure(status::UNSUPPORTED))
        }
    }
}
pub(super) struct Node
{
    pin: File,
    key: Key
}
/// Exact source incarnation on supported Unix filesystems. Birth time is kept
/// at nanosecond resolution, independent of the editable guest creation date.
/// A live pin still supplies the stronger within-session no-reuse guarantee.
fn object_identity(metadata: &Metadata) -> Result<Option<wire::ObjectIdentity>>
{
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let Ok(birth) = metadata.created()
        else
        {
            return Ok(None);
        };
        let (seconds, nanos) = match birth.duration_since(std::time::UNIX_EPOCH)
        {
            Ok(d) =>
            {
                (i64::try_from(d.as_secs()).map_err(|_| Failure(status::INVALID))?,
                 d.subsec_nanos())
            }
            Err(error) =>
            {
                let d = error.duration();
                let seconds = i64::try_from(d.as_secs()).map_err(|_| Failure(status::INVALID))?;
                if d.subsec_nanos() == 0
                {
                    (-seconds, 0)
                }
                else
                {
                    (-seconds.checked_add(1).ok_or(Failure(status::INVALID))?,
                     1_000_000_000 - d.subsec_nanos())
                }
            }
        };
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&metadata.dev().to_le_bytes());
        bytes[8..16].copy_from_slice(&metadata.ino().to_le_bytes());
        bytes[16..24].copy_from_slice(&seconds.to_le_bytes());
        bytes[24..28].copy_from_slice(&nanos.to_le_bytes());
        Ok(Some(wire::ObjectIdentity::from_bytes(bytes)?))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Ok(None)
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
        let identity = object_identity(&metadata)?;
        let key = Key::new(directory.export, &metadata, identity)?;
        // Index physical keys without statting every previously pinned object.
        // The shared table remains authoritative; a cache hit validates its pin.
        let id = match self.node_index.binary_search_by_key(&key, |entry| entry.0)
        {
            Ok(at) =>
            {
                let id = self.node_index[at].1;
                let node = self.nodes.get(id).ok_or(Failure(status::STALE))?;
                let pinned = node.pin.metadata()?;
                if node.key != key
                   || Key::new(directory.export, &pinned, object_identity(&pinned)?)? != key
                {
                    return Err(Failure(status::STALE));
                }
                id
            }
            Err(at) =>
            {
                self.node_index.try_reserve(1).map_err(|_| Failure(status::LIMIT))?;
                let id = self.nodes.insert(Node { pin,
                                                   key })?;
                self.node_index.insert(at, (key, id));
                id
            }
        };
        reply.object = id;
        reply.value = if metadata.is_dir() { 2 } else { 1 };
        reply.offset = if metadata.is_file() { metadata.len() } else { 0 };
        let modified = metadata.modified()?;
        Ok(wire::Description { created: date(metadata.created().unwrap_or(modified))?,
                               modified: date(modified)?,
                               identity,
                               revision: fingerprint(&metadata)? }.encode()
                                                                  .to_vec())
    }
}

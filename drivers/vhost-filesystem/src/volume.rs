use alloc::{string::String, vec::Vec};

use mac_fork_file::{self as forks, Fork, Kind, Layout, Metadata};
use mac_host_protocol::{self as wire, Arguments, Operation as Op};

use crate::{Cause, Reply, Result, Rpc,
            rpc::{buffer, yield_once}};
mod write;
mod scan;
mod plain;
pub use write::{Create, Replace};
const ROOT: u32 = 2;
const MAX_MOUNTS: usize = 16;
const MAX_DIRECTORIES: usize = 128;
const MAX_NODES: usize = 1024;
const MAX_OPEN: usize = 64;

/// This is an index of identities supplied by the resident or persistent catalog,
/// never an ID issuer. Insertion is fallible and lookup does no catalog I/O.
struct Index<T>
{
    entries: Vec<(u32, T)>,
    limit:   usize
}
impl<T> Index<T>
{
    const fn new(limit: usize) -> Self
    {
        Self { entries: Vec::new(),
               limit }
    }

    fn get(&self, id: u32) -> Option<&T>
    {
        self.entries.binary_search_by_key(&id, |item| item.0).ok().map(|at| &self.entries[at].1)
    }

    fn get_mut(&mut self, id: u32) -> Option<&mut T>
    {
        self.entries.binary_search_by_key(&id, |item| item.0).ok().map(|at| &mut self.entries[at].1)
    }

    fn reserve(&mut self, id: u32) -> Result<()>
    {
        if id == 0
        {
            return Err(Cause::Invalid.into());
        }
        if self.get(id).is_some()
        {
            return Err(Cause::Conflict.into());
        }
        if self.entries.len() == self.limit
        {
            return Err(Cause::NoMemory.into());
        }
        self.entries.try_reserve(1).map_err(|_| Cause::NoMemory.into())
    }

    fn insert(&mut self, id: u32, value: T) -> Result<()>
    {
        if id == 0
        {
            return Err(Cause::Invalid.into());
        }
        let at = self.entries
                     .binary_search_by_key(&id, |item| item.0)
                     .map_or_else(Ok, |_| Err(Cause::Conflict))?;
        if self.entries.len() == self.limit
        {
            return Err(Cause::NoMemory.into());
        }
        self.entries.try_reserve(1).map_err(|_| Cause::NoMemory)?;
        self.entries.insert(at, (id, value));
        Ok(())
    }

    fn remove(&mut self, id: u32) -> Option<T>
    {
        self.entries
            .binary_search_by_key(&id, |item| item.0)
            .ok()
            .map(|at| self.entries.remove(at).1)
    }
}
#[derive(Clone, Debug)]
pub struct Node
{
    pub parent:   u32,
    pub metadata: Metadata,
    pub layout:   Layout,
    key:          String,
    host_name:    Option<Vec<u8>>
}
#[derive(Clone, Debug)]
pub struct MountInfo
{
    pub metadata:  Metadata,
    pub read_only: bool
}
impl MountInfo
{
    fn try_clone(&self) -> Result<Self>
    {
        Ok(Self { metadata: self.metadata.try_clone()?,
                  ..*self })
    }
}
impl Node
{
    fn try_clone(&self) -> Result<Self>
    {
        let mut key = String::new();
        key.try_reserve_exact(self.key.len()).map_err(|_| Cause::NoMemory)?;
        key.push_str(&self.key);
        Ok(Self { metadata: self.metadata.try_clone()?,
                  host_name: self.host_name
                                 .as_ref()
                                 .map(|name| -> Result<Vec<u8>> {
                                     let mut out = buffer(name.len())?;
                                     out.copy_from_slice(name);
                                     Ok(out)
                                 })
                                 .transpose()?,
                  key,
                  ..*self })
    }
}
struct PendingChild
{
    name:      Vec<u8>,
    kind:      u32,
    directory: Option<u32>
}
struct Directory
{
    handle:      u32,
    enumeration: Option<u32>,
    revision:    Option<[u8; 32]>,
    children:    Vec<u32>,
    by_name:     Vec<u32>,
    complete:    bool,
    pending:     Option<PendingChild>
}
impl Directory
{
    fn new(handle: u32) -> Self
    {
        Self { handle,
               enumeration: None,
               revision: None,
               children: Vec::new(),
               by_name: Vec::new(),
               complete: false,
               pending: None }
    }
}
struct Mount
{
    root:        u32,
    ejecting:    bool,
    invalidated: bool,
    info:        MountInfo,
    nodes:       Index<Node>,
    directories: Index<Directory>
}
struct Open
{
    volume: u32,
    node:   u32,
    handle: u32,
    length: u64,
    layout: Layout,
    plain:  bool
}
struct Capability
{
    owner: u32,
    id:    u32
}
pub struct Filesystem<R: Rpc>
{
    rpc:           R,
    owner:         u32,
    caps:          Vec<Capability>,
    needs_cleanup: bool,
    session:       u32,
    exports:       u32,
    mounts:        Index<Mount>,
    files:         Index<Open>,
    writes:        Index<write::Pending>
}
impl<R: Rpc> Filesystem<R>
{
    pub fn new(rpc: R) -> Self
    {
        Self { rpc,
               owner: 0,
               caps: Vec::new(),
               needs_cleanup: false,
               session: 0,
               exports: 0,
               mounts: Index::new(MAX_MOUNTS),
               files: Index::new(MAX_OPEN),
               writes: Index::new(8) }
    }

    pub(crate) fn quiescent(&self) -> bool
    {
        self.caps.is_empty()
        && self.mounts.entries.is_empty()
        && self.files.entries.is_empty()
        && self.writes.entries.is_empty()
    }

    pub(crate) fn has_write(&self, volume: u32, token: u32) -> bool
    {
        self.writes.get(token).is_some_and(|pending| pending.volume == volume)
    }

    pub(crate) fn has_mount(&self, volume: u32) -> bool
    {
        self.mounts.get(volume).is_some()
    }

    pub(crate) fn write_node(&self, volume: u32, token: u32) -> Option<u32>
    {
        self.writes.get(token).filter(|pending| pending.volume == volume).map(|p| p.node_id())
    }

    pub fn transport(&mut self) -> &mut R
    {
        &mut self.rpc
    }

    async fn call(&mut self, operation: Op, object: u32, offset: u64, value: u32, payload: &[u8])
                  -> Result<Reply>
    {
        let allocates = matches!(operation,
                                 Op::Export
                                 | Op::OpenDirectory
                                 | Op::OpenRead
                                 | Op::ListBegin
                                 | Op::BeginWrite
                                 | Op::OpenLock);
        let allocates = allocates || operation == Op::PublishDirectory;
        if object != 0 && !self.caps.iter().any(|cap| cap.id == object && cap.owner == self.owner)
        {
            return Err(Cause::NotFound.into());
        }
        if allocates
        {
            self.caps.try_reserve(1).map_err(|_| Cause::NoMemory)?;
        }
        let result = self.rpc
                         .invoke(operation,
                                 Arguments { session: self.session,
                                             object,
                                             offset,
                                             value,
                                             payload })
                         .await;
        let reply = match result
        {
            Ok(reply) => reply,
            Err(error) =>
            {
                self.needs_cleanup = true;
                return Err(error);
            }
        };
        let header = reply.header;
        let checked = header.encode().is_ok()
                      && header.reply
                      && header.operation == operation
                      && header.payload as usize == reply.body.len()
                      && reply.body.len() <= wire::MAX_PAYLOAD
                      && (operation == Op::Hello || header.session == self.session);
        if !checked
        {
            self.needs_cleanup = true;
            return Err(Cause::Invalid.into());
        }
        if header.status != wire::status::OK
        {
            self.needs_cleanup = true;
            if operation == Op::Close && header.status == wire::status::STALE
            {
                self.caps.retain(|cap| cap.id != object);
            }
            return Err(Cause::Host(header.status).into());
        }
        if operation == Op::Write && header.value as usize != payload.len()
        {
            self.needs_cleanup = true;
            return Err(Cause::Invalid.into());
        }
        if allocates
        {
            if header.object == 0 || self.caps.iter().any(|cap| cap.id == header.object)
            {
                self.needs_cleanup = true;
                return Err(Cause::Invalid.into());
            }
            self.caps.push(Capability { owner: self.owner,
                                        id:    header.object });
            // A future can be dropped after acquisition but before handing this
            // lease to a catalog/open/write owner. Maintenance must find such
            // leases even when there was no transport error to trigger it.
            self.needs_cleanup = true;
        }
        if matches!(operation, Op::Close | Op::Commit | Op::PublishDirectory)
        {
            self.caps.retain(|cap| cap.id != object);
        }
        Ok(reply)
    }

    async fn close(&mut self, handle: u32) -> Result<()>
    {
        match self.call(Op::Close, handle, 0, 0, &[]).await
        {
            // A known owned lease already retired by the host cannot alias a
            // replacement slot. Disposing its wrapper still releases ownership.
            Err(error) if error.cause == Cause::Host(wire::status::STALE) => Ok(()),
            result => result.map(|_| ())
        }
    }

    async fn cleanup<T>(&mut self, handle: u32, result: Result<T>) -> Result<T>
    {
        match (result, self.close(handle).await)
        {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Err(mut error), Err(cleanup)) =>
            {
                error.cleanup = Some(cleanup.cause);
                Err(error)
            }
        }
    }

    pub async fn connect(&mut self) -> Result<u32>
    {
        if self.session != 0
        {
            return Ok(self.exports);
        }
        let reply = self.call(Op::Hello, 0, 0, 0, &[]).await?;
        if reply.header.session == 0 || !reply.body.is_empty()
        {
            return Err(Cause::Invalid.into());
        }
        self.session = reply.header.session;
        self.exports = reply.header.value;
        Ok(self.exports)
    }

    async fn open_record(&mut self, parent: u32, name: &[u8]) -> Result<(u32, Node)>
    {
        let opened = self.call(Op::OpenRead, parent, 0, 0, name).await?;
        let handle = opened.header.object;
        if handle == 0
        {
            return Err(Cause::Invalid.into());
        }
        if opened.body.len() != 32
        {
            return self.cleanup(handle, Err(Cause::Invalid.into())).await;
        }
        let read = self.call(Op::Read, handle, 0, forks::METADATA_BYTES as u32, &[]).await;
        let result =
            read.and_then(|reply| {
                    let bytes = reply.body.as_slice().try_into().map_err(|_| Cause::Invalid)?;
                    let (metadata, layout) = forks::decode_metadata(bytes, opened.header.offset)?;
                    let key =
                        mac_hfs_b::names::comparison(&metadata.name).map_err(|_| Cause::Invalid)?;
                    Ok(Node { parent: 0,
                              host_name: None,
                              metadata,
                              layout,
                              key })
                });
        match result
        {
            Ok(node) => Ok((handle, node)),
            Err(error) => self.cleanup(handle, Err(error)).await
        }
    }

    pub async fn mount(&mut self, volume: u32, export: u32) -> Result<MountInfo>
    {
        self.owner = volume;
        if volume == 0 || self.mounts.get(volume).is_some()
        {
            return Err(Cause::Conflict.into());
        }
        if export >= self.connect().await?
        {
            return Err(Cause::NotFound.into());
        }
        let reply = self.call(Op::Export, 0, u64::from(export), 0, &[]).await?;
        let root = reply.header.object;
        let flags = reply.header.value;
        if root == 0 || flags & !wire::ALL_CAPABILITIES != 0 || flags & wire::READ == 0
        {
            return Err(Cause::Invalid.into());
        }
        if flags & wire::WRITE != 0 && flags & wire::ALL_CAPABILITIES != wire::ALL_CAPABILITIES
        {
            return self.cleanup(root, Err(Cause::ReadOnly.into())).await;
        }
        let (record, node) = match self.open_record(root, forks::ROOT_RECORD.as_bytes()).await
        {
            Ok(value) => value,
            Err(error) => return self.cleanup(root, Err(error)).await
        };
        self.close(record).await?;
        if node.metadata.kind != Kind::Root
        {
            return self.cleanup(root, Err(Cause::Invalid.into())).await;
        }
        let snapshots: Result<_> = (|| {
            let info = MountInfo { metadata:  node.metadata.try_clone()?,
                                   read_only: flags & wire::WRITE == 0 };
            let cached = info.try_clone()?;
            Ok((info, cached))
        })();
        let (info, cached) = match snapshots
        {
            Ok(value) => value,
            Err(error) => return self.cleanup(root, Err(error)).await
        };
        let mut mounted = Mount { root,
                                  ejecting: false,
                                  invalidated: false,
                                  info: cached,
                                  nodes: Index::new(MAX_NODES),
                                  directories: Index::new(MAX_DIRECTORIES) };
        if let Err(error) =
            mounted.nodes
                   .insert(ROOT, node)
                   .and_then(|()| mounted.directories.insert(ROOT, Directory::new(root)))
        {
            return self.cleanup(root, Err(error)).await;
        }
        if let Err(error) = self.mounts.insert(volume, mounted)
        {
            return self.cleanup(root, Err(error)).await;
        }
        Ok(info)
    }

    pub fn info(&self, volume: u32) -> Result<&MountInfo>
    {
        let mount = self.mounts.get(volume).ok_or(Cause::NotFound)?;
        if mount.invalidated
        {
            return Err(Cause::Stale.into());
        }
        if mount.ejecting
        {
            return Err(Cause::Busy.into());
        }
        Ok(&mount.info)
    }

    pub fn cached_node(&self, volume: u32, node: u32) -> Result<&Node>
    {
        self.info(volume)?;
        self.mounts.get(volume).unwrap().nodes.get(node).ok_or_else(|| Cause::NotFound.into())
    }

    pub async fn open(&mut self, volume: u32, node: u32, token: u32) -> Result<Layout>
    {
        self.owner = volume;
        if token == 0 || self.files.get(token).is_some()
        {
            return Err(Cause::Conflict.into());
        }
        let mount = self.mounts.get(volume).ok_or(Cause::NotFound)?;
        if mount.ejecting
        {
            return Err(Cause::Busy.into());
        }
        if mount.invalidated
        {
            return Err(Cause::Stale.into());
        }
        let cached = mount.nodes.get(node).ok_or(Cause::NotFound)?;
        if cached.metadata.kind != Kind::File
        {
            return Err(Cause::Invalid.into());
        }
        let plain = cached.host_name.is_some();
        let mut name = buffer(cached.host_name.as_ref().map_or(34, Vec::len))?;
        if let Some(raw) = &cached.host_name
        {
            name.copy_from_slice(raw);
        }
        else
        {
            name.copy_from_slice(&forks::host_name(Kind::File, cached.metadata.identity)?);
        }
        let parent = mount.directories.get(cached.parent).ok_or(Cause::NotFound)?.handle;
        let (handle, opened) = if plain
        {
            self.open_plain(volume, parent, &name).await?
        }
        else
        {
            self.open_record(parent, &name).await?
        };
        if opened.metadata != self.cached_node(volume, node)?.metadata
        {
            return self.cleanup(handle, Err(Cause::Stale.into())).await;
        }
        let layout = opened.layout;
        let result = self.files.insert(token,
                                       Open { volume,
                                              node,
                                              handle,
                                              length: if plain
                                              {
                                                  layout.data.length
                                              }
                                              else
                                              {
                                                  layout.total
                                              },
                                              plain,
                                              layout });
        if let Err(error) = result
        {
            return self.cleanup(handle, Err(error)).await;
        }
        Ok(layout)
    }

    pub async fn read(&mut self, volume: u32, token: u32, kind: Fork, offset: u64, capacity: usize)
                      -> Result<Vec<u8>>
    {
        self.owner = volume;
        if capacity > wire::MAX_PAYLOAD
        {
            return Err(Cause::Invalid.into());
        }
        let open =
            self.files.get(token).filter(|file| file.volume == volume).ok_or(Cause::NotFound)?;
        let (handle, length, layout) = (open.handle, open.length, open.layout.fork(kind));
        if open.plain
        {
            let count = layout.length.saturating_sub(offset).min(capacity as u64) as u32;
            if count == 0
            {
                return buffer(0);
            }
            let reply = self.call(Op::Read, handle, offset, count, &[]).await?;
            if reply.body.len() != count as usize
            {
                return Err(Cause::Stale.into());
            }
            return Ok(reply.body);
        }
        let count = layout.length.saturating_sub(offset).min(capacity as u64) as usize;
        let mut output = buffer(count)?;
        if count == 0
        {
            return Ok(output);
        }
        let before = self.call(Op::Stat, handle, 0, 0, &[]).await?;
        if before.header.offset != length
        {
            return Err(Cause::Stale.into());
        }
        let mut copied = 0;
        while copied < count
        {
            let logical = offset + copied as u64;
            let (physical, bytes, _) = layout.chunk(logical / forks::CHUNK_BYTES as u64)?;
            let reply = self.call(Op::Read,
                                  handle,
                                  physical,
                                  (bytes + forks::HASH_BYTES as usize) as u32,
                                  &[])
                            .await?;
            let verified = forks::decode_chunk(&reply.body, bytes)?;
            let within = (logical % forks::CHUNK_BYTES as u64) as usize;
            let take = (bytes - within).min(count - copied);
            output[copied..copied + take].copy_from_slice(&verified[within..within + take]);
            copied += take;
            yield_once().await;
        }
        let after = self.call(Op::Stat, handle, 0, 0, &[]).await?;
        if after.header.offset != length || before.body != after.body
        {
            return Err(Cause::Stale.into());
        }
        Ok(output)
    }

    pub async fn close_file(&mut self, volume: u32, token: u32) -> Result<()>
    {
        self.owner = volume;
        let handle = self.files
                         .get(token)
                         .filter(|file| file.volume == volume)
                         .ok_or(Cause::NotFound)?
                         .handle;
        self.close(handle).await?;
        self.files.remove(token);
        Ok(())
    }

    pub fn is_open(&self, volume: u32, node: u32) -> bool
    {
        self.files.entries.iter().any(|(_, file)| file.volume == volume && file.node == node)
    }

    /// Refuse eject with open forks. A failed close retains all remaining
    /// capabilities and the ejecting state, allowing a retry without double close.
    pub async fn unmount(&mut self, volume: u32) -> Result<()>
    {
        self.owner = volume;
        if self.files.entries.iter().any(|(_, file)| file.volume == volume)
           || self.writes.entries.iter().any(|(_, pending)| pending.volume == volume)
        {
            return Err(Cause::Busy.into());
        }
        self.mounts.get_mut(volume).ok_or(Cause::NotFound)?.ejecting = true;
        while let Some(id) =
            self.caps.iter().rev().find(|cap| cap.owner == volume).map(|cap| cap.id)
        {
            self.close(id).await?;
            yield_once().await;
        }
        self.mounts.remove(volume);
        Ok(())
    }

    fn referenced(&self, id: u32) -> bool
    {
        self.mounts.entries.iter().any(|(_,mount)|mount.root==id || mount.directories.entries.iter().any(|(_,directory)|directory.handle==id || directory.enumeration==Some(id) || directory.pending.as_ref().is_some_and(|child|child.directory==Some(id))))
            || self.files.entries.iter().any(|(_,file)|file.handle==id)
            || self.writes.entries.iter().any(|(_,pending)|pending.references(id))
    }

    /// Release acquired capabilities not handed to a durable object owner,
    /// including acquisitions abandoned between awaits. Live object owners
    /// retain their own retry path. Clean maintenance is a cheap no-op.
    /// This does not cancel an RPC still in flight: the native adapter must
    /// drain/cancel that lower request before dropping its operation future.
    pub async fn cleanup_orphans(&mut self) -> Result<()>
    {
        if !self.needs_cleanup
        {
            return Ok(());
        }
        loop
        {
            let next = self.caps
                           .iter()
                           .rev()
                           .find(|cap| !self.referenced(cap.id))
                           .map(|cap| (cap.owner, cap.id));
            let Some((owner, id)) = next
            else
            {
                break;
            };
            self.owner = owner;
            self.close(id).await?;
            yield_once().await;
        }
        self.needs_cleanup = false;
        Ok(())
    }

    /// Rebuild unknown external changes after ScanStale. Our own publications
    /// update their affected node/directory in place and never take this path.
    /// Existing fork leases remain bound to their original verified source.
    pub async fn refresh(&mut self, volume: u32) -> Result<()>
    {
        self.owner = volume;
        if self.writes.entries.iter().any(|(_, pending)| pending.volume == volume)
        {
            return Err(Cause::Busy.into());
        }
        let mount = self.mounts.get_mut(volume).ok_or(Cause::NotFound)?;
        if mount.ejecting
        {
            return Err(Cause::Busy.into());
        }
        mount.invalidated = true;
        let (root, identity, read_only) =
            (mount.root, mount.info.metadata.volume, mount.info.read_only);
        let before = self.directory_revision(root).await?;
        let (record, node) = self.open_record(root, forks::ROOT_RECORD.as_bytes()).await?;
        self.close(record).await?;
        if node.metadata.kind != Kind::Root || node.metadata.volume != identity
        {
            return Err(Cause::Stale.into());
        }
        let info = MountInfo { metadata: node.metadata.try_clone()?,
                               read_only };
        let mut nodes = Index::new(MAX_NODES);
        nodes.insert(ROOT, node)?;
        let mut directories = Index::new(MAX_DIRECTORIES);
        directories.insert(ROOT, Directory::new(root))?;
        loop
        {
            let next = self.caps
                           .iter()
                           .rev()
                           .find(|cap| {
                               cap.owner == volume
                               && cap.id != root
                               && !self.files.entries.iter().any(|(_, file)| file.handle == cap.id)
                           })
                           .map(|cap| cap.id);
            let Some(id) = next
            else
            {
                break;
            };
            self.close(id).await?;
            yield_once().await;
        }
        if self.directory_revision(root).await? != before
        {
            return Err(Cause::Stale.into());
        }
        let mount = self.mounts.get_mut(volume).unwrap();
        mount.nodes = nodes;
        mount.directories = directories;
        mount.info = info;
        mount.invalidated = false;
        Ok(())
    }
}

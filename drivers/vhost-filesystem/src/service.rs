//! Filesystem-class projection. This code owns request bytes while it awaits;
//! the native adapter never retains a resident callback's borrowed buffers.
use alloc::vec::Vec;

use mac_filesystem_interface::{self as fs, Arguments, Metadata, Operation};

use crate::{Cause, Create, Filesystem, Node, Replace, Result, Rpc, rpc::buffer};
impl<R: Rpc> Filesystem<R>
{
    /// Execute one filesystem-class request. Publication and retries stay in the
    /// provider; the File Manager remains responsible for app ownership checks.
    pub async fn execute(&mut self, operation: u32, input: &[u8]) -> Result<Vec<u8>>
    {
        let op = Operation::try_from(operation).map_err(|_| Cause::Invalid)?;
        let args = Arguments::decode(input).map_err(|_| Cause::Invalid)?;
        self.validate_request(op, args)?;
        match op
        {
            Operation::Exports =>
            {
                let mut out = buffer(4)?;
                out.copy_from_slice(&self.connect().await?.to_le_bytes());
                Ok(out)
            }
            Operation::Mount =>
            {
                let mut out = buffer(fs::MOUNT_BYTES + fs::METADATA_BYTES + 510)?;
                let export = u32::try_from(args.auxiliary).map_err(|_| Cause::Invalid)?;
                let info = self.mount(args.volume, export).await?;
                let result: Result<Vec<u8>> = (|| {
                    // Mount only reads the root record. Enumerate lazily
                    // when a caller actually requests catalog children.
                    let root = self.project(args.volume, 2)?;
                    let descriptor =
                        fs::Mount { capabilities: fs::READ
                                                  | if info.read_only { 0 } else { fs::WRITE },
                                    filesystem: *b"HOST",
                                    filesystem_version: mac_fork_file::VERSION,
                                    capacity: None,
                                    root };
                    let count = descriptor.encode(&mut out).map_err(|_| Cause::Invalid)?;
                    out.truncate(count);
                    Ok(out)
                })();
                if result.is_err()
                   && let Err(cleanup) = self.unmount(args.volume).await
                {
                    let mut error = result.unwrap_err();
                    error.cleanup = Some(cleanup.cause);
                    return Err(error);
                }
                result
            }
            Operation::Unmount =>
            {
                self.unmount(args.volume).await?;
                Ok(Vec::new())
            }
            Operation::Catalog =>
            {
                self.find(args.volume, args.node).await?;
                if args.flags & fs::INCLUDE_CHILD_COUNT != 0
                   && self.cached_node(args.volume, args.node)?.metadata.kind
                      != mac_fork_file::Kind::File
                {
                    self.scan(args.volume, args.node).await?;
                }
                self.encode_node(args.volume, args.node)
            }
            Operation::Lookup =>
            {
                let id =
                    self.lookup(args.volume, args.parent, args.name().map_err(|_| Cause::Invalid)?)
                        .await?
                        .metadata
                        .catalog_id;
                if args.flags & fs::INCLUDE_CHILD_COUNT != 0
                   && self.cached_node(args.volume, id)?.metadata.kind != mac_fork_file::Kind::File
                {
                    self.scan(args.volume, id).await?;
                }
                self.encode_node(args.volume, id)
            }
            Operation::Scan =>
            {
                self.find(args.volume, args.parent).await?;
                if self.cached_node(args.volume, args.parent)?.metadata.kind
                   == mac_fork_file::Kind::File
                {
                    return Err(Cause::Invalid.into());
                }
                let mut out = buffer(4 + fs::METADATA_BYTES + 510)?;
                let index = usize::try_from(args.offset).map_err(|_| Cause::Invalid)?;
                if self.child(args.volume, args.parent, index).is_err()
                {
                    let done = self.scan_step(args.volume, args.parent).await?;
                    if self.child(args.volume, args.parent, index).is_err()
                    {
                        out[..4].copy_from_slice(&if done { 2u32 } else { 0 }.to_le_bytes());
                        out.truncate(4);
                        return Ok(out);
                    }
                }
                let node = self.child(args.volume, args.parent, index)?;
                // Child count is not needed for drawing a folder icon. Mark its
                // unknown count explicitly; Catalog/Lookup obtain an exact count.
                let count = self.project_node(args.volume, node)?
                                .encode(&mut out[4..])
                                .map_err(|_| Cause::Invalid)?;
                out[..4].copy_from_slice(&1u32.to_le_bytes());
                out.truncate(4 + count);
                Ok(out)
            }
            Operation::Open =>
            {
                let mut out = buffer(16)?;
                self.find(args.volume, args.node).await?;
                let layout = self.open(args.volume, args.node, args.token).await?;
                out[..8].copy_from_slice(&layout.data.length.to_le_bytes());
                out[8..].copy_from_slice(&layout.resources.length.to_le_bytes());
                Ok(out)
            }
            Operation::Read =>
            {
                self.read(args.volume,
                          args.token,
                          match args.flags
                          {
                              fs::DATA_FORK => mac_fork_file::Fork::Data,
                              fs::RESOURCE_FORK => mac_fork_file::Fork::Resource,
                              _ => return Err(Cause::Invalid.into())
                          },
                          args.offset,
                          usize::try_from(args.length).map_err(|_| Cause::Invalid)?)
                    .await
            }
            Operation::Close =>
            {
                self.close_file(args.volume, args.token).await?;
                Ok(Vec::new())
            }
            Operation::BeginCreate =>
            {
                let mut out = buffer(4)?;
                let id = self.begin_create(Create { volume:          args.volume,
                                                    parent:          args.parent,
                                                    token:           args.token,
                                                    name:
                                                        args.name().map_err(|_| Cause::Invalid)?,
                                                    data_length:     args.length,
                                                    resource_length: args.auxiliary,
                                                    when:            args.when })
                             .await?;
                out.copy_from_slice(&id.to_le_bytes());
                Ok(out)
            }
            Operation::BeginReplace =>
            {
                self.begin_replace(Replace { volume:          args.volume,
                                             node:            args.node,
                                             token:           args.token,
                                             data_length:     args.length,
                                             resource_length: args.auxiliary,
                                             when:            args.when })
                    .await?;
                Ok(Vec::new())
            }
            Operation::WriteChunk =>
            {
                self.write_chunk(args.volume,
                                 args.token,
                                 match args.flags
                                 {
                                     fs::DATA_FORK => mac_fork_file::Fork::Data,
                                     fs::RESOURCE_FORK => mac_fork_file::Fork::Resource,
                                     _ => return Err(Cause::Invalid.into())
                                 },
                                 args.payload)
                    .await?;
                Ok(Vec::new())
            }
            Operation::FinishWrite =>
            {
                let mut out = buffer(4)?;
                out.copy_from_slice(&self.finish_write(args.volume, args.token)
                                         .await?
                                         .to_le_bytes());
                Ok(out)
            }
            Operation::AbortWrite =>
            {
                let mut out = buffer(4)?;
                out.copy_from_slice(&u32::from(self.abort_write(args.volume, args.token).await?).to_le_bytes());
                Ok(out)
            }
            Operation::Refresh =>
            {
                self.refresh(args.volume).await?;
                Ok(Vec::new())
            }
            _ => Err(Cause::Unsupported.into())
        }
    }

    fn validate_request(&self, op: Operation, a: Arguments<'_>) -> Result<()>
    {
        let volume = Arguments { volume: a.volume,
                                 ..Default::default() };
        let token = Arguments { token: a.token,
                                ..volume };
        let node = Arguments { node: a.node,
                               ..volume };
        let accepted = match op
        {
            Operation::Exports => Arguments::default(),
            Operation::Mount =>
            {
                Arguments { auxiliary: a.auxiliary,
                            ..volume }
            }
            Operation::Unmount | Operation::Refresh => volume,
            Operation::Catalog =>
            {
                Arguments { flags: a.flags,
                            ..node }
            }
            Operation::Scan =>
            {
                Arguments { parent: a.parent,
                            offset: a.offset,
                            ..volume }
            }
            Operation::Lookup =>
            {
                Arguments { parent: a.parent,
                            flags: a.flags,
                            payload: a.payload,
                            ..volume }
            }
            Operation::Open =>
            {
                Arguments { token: a.token,
                            ..node }
            }
            Operation::Read =>
            {
                Arguments { flags: a.flags,
                            offset: a.offset,
                            length: a.length,
                            ..token }
            }
            Operation::Close | Operation::FinishWrite | Operation::AbortWrite => token,
            Operation::BeginCreate =>
            {
                Arguments { parent: a.parent,
                            length: a.length,
                            auxiliary: a.auxiliary,
                            when: a.when,
                            payload: a.payload,
                            ..token }
            }
            Operation::BeginReplace =>
            {
                Arguments { node: a.node,
                            length: a.length,
                            auxiliary: a.auxiliary,
                            when: a.when,
                            ..token }
            }
            Operation::WriteChunk =>
            {
                Arguments { flags: a.flags,
                            payload: a.payload,
                            ..token }
            }
            _ => return Err(Cause::Unsupported.into())
        };
        if a != accepted
           || matches!(op, Operation::Catalog | Operation::Lookup)
              && a.flags & !fs::INCLUDE_CHILD_COUNT != 0
           || op != Operation::Exports && a.volume == 0
           || matches!(op,
                       Operation::Open
                       | Operation::Read
                       | Operation::Close
                       | Operation::BeginCreate
                       | Operation::BeginReplace
                       | Operation::WriteChunk
                       | Operation::FinishWrite
                       | Operation::AbortWrite)
              && a.token == 0
           || matches!(op, Operation::Catalog | Operation::Open | Operation::BeginReplace)
              && a.node < 2
           || matches!(op, Operation::Scan | Operation::Lookup | Operation::BeginCreate)
              && a.parent < 2
           || op == Operation::Read && a.length > fs::MAX_TRANSFER as u64
        {
            return Err(Cause::Invalid.into());
        }
        Ok(())
    }

    async fn find(&mut self, volume: u32, id: u32) -> Result<()>
    {
        if self.cached_node(volume, id).is_ok()
        {
            return Ok(());
        }
        // Host tokens encode identity but not ancestry. Discover a not-yet-cached
        // persistent ID in bounded steps, retaining the resulting folder caches.
        let mut folders = Vec::new();
        folders.try_reserve(1).map_err(|_| Cause::NoMemory)?;
        folders.push(2);
        let mut at = 0;
        while let Some(&parent) = folders.get(at)
        {
            self.scan(volume, parent).await?;
            if self.cached_node(volume, id).is_ok()
            {
                return Ok(());
            }
            let mut index = 0;
            while let Ok(child) = self.child(volume, parent, index)
            {
                if child.metadata.kind != mac_fork_file::Kind::File
                {
                    folders.try_reserve(1).map_err(|_| Cause::NoMemory)?;
                    folders.push(child.metadata.catalog_id);
                }
                index += 1;
            }
            at += 1;
            crate::rpc::yield_once().await;
        }
        Err(Cause::NotFound.into())
    }

    fn project_node<'a>(&'a self, volume: u32, node: &'a Node) -> Result<Metadata<'a>>
    {
        let m = &node.metadata;
        let directory = m.kind != mac_fork_file::Kind::File;
        let count =
            if directory { self.cached_child_count(volume, m.catalog_id)? } else { Some(0) };
        Ok(Metadata { id: m.catalog_id,
                      parent: node.parent,
                      directory,
                      data_length: node.layout.data.length,
                      resource_length: node.layout.resources.length,
                      created: m.created,
                      modified: m.modified,
                      backup: m.backup,
                      child_count: count.unwrap_or(fs::CHILD_COUNT_UNKNOWN),
                      attributes: m.attributes,
                      finder_info: m.finder_info,
                      revision: m.revision,
                      name: &m.name,
                      comment: &m.comment })
    }

    fn project(&self, volume: u32, id: u32) -> Result<Metadata<'_>>
    {
        self.project_node(volume, self.cached_node(volume, id)?)
    }

    fn encode_node(&self, volume: u32, id: u32) -> Result<Vec<u8>>
    {
        let mut out = buffer(fs::METADATA_BYTES + 510)?;
        let count = self.project(volume, id)?.encode(&mut out).map_err(|_| Cause::Invalid)?;
        out.truncate(count);
        Ok(out)
    }
}

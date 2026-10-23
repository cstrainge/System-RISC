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
        if self.group_busy(args.volume)
           && !matches!(op,
                        Operation::StepIconMoves
                        | Operation::AbortIconMoves
                        | Operation::AbortWrite
                        | Operation::Read
                        | Operation::Close)
        {
            return Err(Cause::Busy.into());
        }
        match op
        {
            Operation::BeginIconMoves | Operation::StepIconMoves | Operation::AbortIconMoves =>
            {
                let mut out = buffer(16)?;
                let progress = if op == Operation::BeginIconMoves
                {
                    self.begin_icon_moves(args.volume,
                                          args.token,
                                          fs::IconMoves::decode(args.payload).map_err(|_| {
                                                                                 Cause::Invalid
                                                                             })?,
                                          args.when)
                        .await?
                }
                else
                {
                    self.step_icon_moves(args.volume, args.token, op == Operation::AbortIconMoves)
                        .await?
                };
                fs::IconMoveProgress::from_words(progress).map_err(|_| Cause::Invalid)?
                                                          .encode(&mut out)
                                                          .map_err(|_| Cause::Invalid)?;
                Ok(out)
            }
            Operation::Exports =>
            {
                let mut out = buffer(4)?;
                out.copy_from_slice(&self.connect().await?.to_le_bytes());
                Ok(out)
            }
            Operation::Mount =>
            {
                let mut out = buffer(fs::MAX_MOUNT_BYTES)?;
                let export = u32::try_from(args.auxiliary).map_err(|_| Cause::Invalid)?;
                self.mount(args.volume, export).await?;
                let result: Result<Vec<u8>> = (|| {
                    // Mount only reads the root record. Enumerate lazily
                    // when a caller actually requests catalog children.
                    let descriptor = self.mount_descriptor(args.volume)?;
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
                let mut out = buffer(4 + fs::MAX_METADATA_BYTES)?;
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
                let mut out = buffer(fs::MAX_METADATA_BYTES)?;
                let create = Create { volume:          args.volume,
                                      parent:          args.parent,
                                      token:           args.token,
                                      properties:
                                          fs::FileProperties::decode(args.payload).map_err(|_| {
                                                                                      Cause::Invalid
                                                                                  })?,
                                      data_length:     args.length,
                                      resource_length: args.auxiliary,
                                      when:            args.when };
                if args.flags & fs::CREATE_DIRECTORY != 0
                {
                    self.begin_directory(create).await?;
                }
                else
                {
                    self.begin_create(create).await?;
                }
                let count = self.project_node(args.volume,
                                              self.pending_node(args.volume, args.token)?)?
                                .encode(&mut out)
                                .map_err(|_| Cause::Invalid)?;
                out.truncate(count);
                Ok(out)
            }
            Operation::Rename | Operation::Move =>
            {
                let mut out = buffer(4 + fs::MAX_METADATA_BYTES)?;
                self.find(args.volume, args.node).await?;
                let cached = self.cached_node(args.volume, args.node)?.try_clone()?;
                let (parent, name) = if op == Operation::Rename
                {
                    (cached.parent, args.name().map_err(|_| Cause::Invalid)?)
                }
                else
                {
                    (args.parent, cached.metadata.name.as_str())
                };
                let staged = self.begin_rename(args.volume, args.node, parent, name, args.token,
                                               args.when)
                                 .await?;
                out[..4].copy_from_slice(&u32::from(staged).to_le_bytes());
                let node = if staged
                {
                    self.pending_node(args.volume, args.token)?
                }
                else
                {
                    self.cached_node(args.volume, args.node)?
                };
                let count = self.project_node(args.volume, node)?
                                .encode(&mut out[4..])
                                .map_err(|_| Cause::Invalid)?;
                out.truncate(4 + count);
                Ok(out)
            }
            Operation::SetMetadata =>
            {
                let mut out = buffer(4 + fs::MAX_METADATA_BYTES)?;
                let patch = fs::MetadataPatch::decode(args.payload).map_err(|_| Cause::Invalid)?;
                let staged = self.begin_metadata(args.volume, args.node, args.token, patch,
                                                 args.when)
                                 .await?;
                out[..4].copy_from_slice(&u32::from(staged).to_le_bytes());
                let node = if staged
                {
                    self.pending_node(args.volume, args.token)?
                }
                else
                {
                    self.cached_node(args.volume, args.node)?
                };
                let count = self.project_node(args.volume, node)?
                                .encode(&mut out[4..])
                                .map_err(|_| Cause::Invalid)?;
                out.truncate(4 + count);
                Ok(out)
            }
            Operation::BeginReplace =>
            {
                let mut out = buffer(fs::MAX_METADATA_BYTES)?;
                self.begin_replace(Replace { volume:             args.volume,
                                             node:               args.node,
                                             token:              args.token,
                                             data_length:        args.length,
                                             resource_length:    args.auxiliary,
                                             when:               args.when,
                                             preserve_resources: args.flags
                                                                 & fs::PRESERVE_RESOURCE_FORK
                                                                 != 0,
                                             properties: if args.flags & fs::REPLACE_PROPERTIES!=0 {
                                                 Some(fs::FileProperties::decode(args.payload).map_err(|_|Cause::Invalid)?)
                                             }else {None} })
                    .await?;
                let count = self.project_node(args.volume,
                                              self.pending_node(args.volume, args.token)?)?
                                .encode(&mut out)
                                .map_err(|_| Cause::Invalid)?;
                out.truncate(count);
                Ok(out)
            }
            Operation::Remove =>
            {
                let mut out = buffer(4 + fs::MAX_METADATA_BYTES)?;
                self.begin_remove(args.volume, args.node, args.token, args.when).await?;
                let count = self.project_node(args.volume,
                                              self.pending_node(args.volume, args.token)?)?
                                .encode(&mut out[4..])
                                .map_err(|_| Cause::Invalid)?;
                out[..4].copy_from_slice(&1u32.to_le_bytes());
                out.truncate(4 + count);
                Ok(out)
            }
            Operation::ReadWrite =>
            {
                self.read_write(args.volume,
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
                let removing = self.removing(args.volume, args.token)?;
                let mut out =
                    buffer(fs::MAX_WRITE_RECEIPT_BYTES.max(fs::MAX_REMOVAL_RECEIPT_BYTES))?;
                let node = self.finish_write(args.volume, args.token).await?;
                let count = if removing
                {
                    fs::RemovalReceipt { removed: true, volume: self.mount_descriptor(args.volume)? }
                        .encode(&mut out).map_err(|_| Cause::Invalid)?
                }
                else
                {
                    self.encode_receipt(args.volume, node, &mut out)?
                };
                out.truncate(count);
                Ok(out)
            }
            Operation::AbortWrite =>
            {
                let removing = self.removing(args.volume, args.token)?;
                let mut out =
                    buffer(fs::MAX_ABORT_RECEIPT_BYTES.max(fs::MAX_REMOVAL_RECEIPT_BYTES))?;
                let node = self.write_node(args.volume, args.token).ok_or(Cause::NotFound)?;
                let published = self.abort_write(args.volume, args.token).await?;
                let count = if removing
                {
                    fs::RemovalReceipt { removed: published, volume: self.mount_descriptor(args.volume)? }
                        .encode(&mut out).map_err(|_| Cause::Invalid)?
                }
                else
                {
                    self.encode_abort_receipt(args.volume, node, published, &mut out)?
                };
                out.truncate(count);
                Ok(out)
            }
            Operation::Refresh =>
            {
                self.refresh(args.volume).await?;
                Ok(Vec::new())
            }
            Operation::CheckDirectory =>
            {
                self.find(args.volume, args.node).await?;
                let (changed, revision) = self.check_directory(args.volume, args.node).await?;
                let mut out = buffer(33)?;
                out[0] = u8::from(changed);
                out[1..].copy_from_slice(&revision);
                Ok(out)
            }
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
            Operation::CheckDirectory => node,
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
            Operation::Read | Operation::ReadWrite =>
            {
                Arguments { flags: a.flags,
                            offset: a.offset,
                            length: a.length,
                            ..token }
            }
            Operation::Close | Operation::FinishWrite | Operation::AbortWrite => token,
            Operation::StepIconMoves | Operation::AbortIconMoves => token,
            Operation::BeginIconMoves =>
            {
                Arguments { when: a.when,
                            payload: a.payload,
                            ..token }
            }
            Operation::BeginCreate =>
            {
                if a.flags & !fs::CREATE_DIRECTORY != 0
                {
                    return Err(Cause::Invalid.into());
                }
                Arguments { parent: a.parent,
                            flags: a.flags,
                            length: a.length,
                            auxiliary: a.auxiliary,
                            when: a.when,
                            payload: a.payload,
                            ..token }
            }
            Operation::Remove =>
            {
                Arguments { token: a.token,
                            when: a.when,
                            ..node }
            }
            Operation::Rename =>
            {
                Arguments { token: a.token,
                            when: a.when,
                            payload: a.payload,
                            ..node }
            }
            Operation::Move =>
            {
                Arguments { token: a.token,
                            when: a.when,
                            parent: a.parent,
                            ..node }
            }
            Operation::SetMetadata =>
            {
                Arguments { token: a.token,
                            when: a.when,
                            payload: a.payload,
                            ..node }
            }
            Operation::BeginReplace =>
            {
                if a.flags & !(fs::PRESERVE_RESOURCE_FORK | fs::REPLACE_PROPERTIES) != 0
                   || (a.flags & fs::REPLACE_PROPERTIES == 0 && !a.payload.is_empty())
                {
                    return Err(Cause::Invalid.into());
                }
                Arguments { node: a.node,
                            flags: a.flags,
                            length: a.length,
                            auxiliary: a.auxiliary,
                            when: a.when,
                            payload: a.payload,
                            ..token }
            }
            Operation::WriteChunk =>
            {
                Arguments { flags: a.flags,
                            payload: a.payload,
                            ..token }
            }
        };
        if a != accepted
           || op == Operation::BeginReplace
              && (a.flags & !(fs::PRESERVE_RESOURCE_FORK | fs::REPLACE_PROPERTIES) != 0
                  || a.flags & fs::PRESERVE_RESOURCE_FORK != 0 && a.auxiliary != 0)
           || matches!(op, Operation::Catalog | Operation::Lookup)
              && a.flags & !fs::INCLUDE_CHILD_COUNT != 0
           || op != Operation::Exports && a.volume == 0
           || matches!(op,
                       Operation::Open
                       | Operation::Read
                       | Operation::Close
                       | Operation::BeginCreate
                       | Operation::BeginReplace
                       | Operation::SetMetadata
                       | Operation::Rename
                       | Operation::Remove
                       | Operation::Move
                       | Operation::WriteChunk
                       | Operation::FinishWrite
                       | Operation::AbortWrite)
              && a.token == 0
           || matches!(op,
                       Operation::Catalog
                       | Operation::Open
                       | Operation::BeginReplace
                       | Operation::SetMetadata
                       | Operation::Remove)
              && a.node < 2
           || matches!(op, Operation::Scan | Operation::Lookup | Operation::BeginCreate)
              && a.parent < 2
           || op == Operation::Move && a.parent < 2
           || op == Operation::Read && a.length > fs::MAX_TRANSFER as u64
        {
            return Err(Cause::Invalid.into());
        }
        Ok(())
    }

    pub(crate) async fn find(&mut self, volume: u32, id: u32) -> Result<()>
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
        let count = if directory
        {
            if self.pending_directory(volume, m.catalog_id)
            {
                Some(0)
            }
            else
            {
                self.cached_child_count(volume, m.catalog_id)?
            }
        }
        else
        {
            Some(0)
        };
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

    fn mount_descriptor(&self, volume: u32) -> Result<fs::Mount<'_>>
    {
        let info = self.info(volume)?;
        Ok(fs::Mount { capabilities:       fs::READ
                                           | fs::CHANGES
                                           | if info.read_only { 0 } else { fs::WRITE },
                       filesystem:         *b"HOST",
                       filesystem_version: mac_fork_file::VERSION,
                       capacity:           None,
                       root:               self.project(volume, 2)? })
    }

    pub(crate) fn encode_receipt(&self, volume: u32, node: u32, output: &mut [u8])
                                 -> Result<usize>
    {
        let receipt = fs::WriteReceipt { file:   self.project(volume, node)?,
                                         volume: self.mount_descriptor(volume)? };
        if node == 2 {fs::MetadataReceipt(receipt).encode(output)} else {receipt.encode(output)}.map_err(|_| Cause::Invalid.into())
    }

    pub(crate) fn encode_removal_receipt(&self, volume: u32, removed: bool, output: &mut [u8])
                                         -> Result<usize>
    {
        fs::RemovalReceipt { removed,
                             volume: self.mount_descriptor(volume)? }.encode(output)
                                                                     .map_err(|_| {
                                                                         Cause::Invalid.into()
                                                                     })
    }

    pub(crate) fn encode_abort_receipt(&self, volume: u32, node: u32, published: bool,
                                       output: &mut [u8])
                                       -> Result<usize>
    {
        let receipt = if published
        {
            Some(fs::WriteReceipt { file:   self.project(volume, node)?,
                                    volume: self.mount_descriptor(volume)? })
        }
        else
        {
            None
        };
        if node == 2
        {
            fs::MetadataAbortReceipt(receipt).encode(output)
        }
        else
        {
            fs::AbortReceipt(receipt).encode(output)
        }.map_err(|_| Cause::Invalid.into())
    }

    fn encode_node(&self, volume: u32, id: u32) -> Result<Vec<u8>>
    {
        let mut out = buffer(fs::MAX_METADATA_BYTES)?;
        let count = self.project(volume, id)?.encode(&mut out).map_err(|_| Cause::Invalid)?;
        out.truncate(count);
        Ok(out)
    }
}

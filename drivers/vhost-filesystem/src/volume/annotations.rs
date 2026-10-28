//! Small optional overrides keyed by a qualified host object incarnation.
use forks::annotation::{self, Annotation};

use super::*;
pub(super) enum Directory
{
    Unknown,
    Missing,
    Open(u32)
}
impl Directory
{
    pub(super) fn references(&self, handle: u32) -> bool
    {
        matches!(self, Self::Open(value) if *value == handle)
    }
}
impl<R: Rpc> Filesystem<R>
{
    pub(super) async fn create_annotation_directory(&mut self, volume: u32) -> Result<u32>
    {
        if let Some(handle) = self.annotation_directory(volume).await?
        {
            return Ok(handle);
        }
        let root = self.mounts.get(volume).ok_or(Cause::NotFound)?.root;
        self.call(Op::MakeDirectory, root, 0, 0, annotation::DIRECTORY.as_bytes()).await?;
        self.mounts.get_mut(volume).unwrap().annotations = Directory::Unknown;
        self.annotation_directory(volume).await?.ok_or_else(|| Cause::Invalid.into())
    }

    /// Persist an identity before publishing a formerly plain object to the
    /// catalog. Names/dates/content remain host-owned; this empty patch records
    /// only its qualified incarnation and the already reserved catalog ID.
    pub(super) async fn persist_plain_identity(&mut self, volume: u32,
                                               object: wire::ObjectIdentity, id: u32)
                                               -> Result<[u8; annotation::BYTES]>
    {
        let lock = self.writer_lock(volume).await?;
        let result = self.persist_identity_locked(volume, object, id).await;
        match (result, self.release_writer(lock).await)
        {
            (Ok(bytes), Ok(())) => Ok(bytes),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Err(mut error), Err(cleanup)) =>
            {
                error.cleanup = Some(cleanup.cause);
                Err(error)
            }
        }
    }

    /// Caller owns the volume writer lease, including during publication cleanup.
    pub(super) async fn persist_identity_locked(&mut self, volume: u32,
                                                object: wire::ObjectIdentity, id: u32)
                                                -> Result<[u8; annotation::BYTES]>
    {
        let record = Annotation { volume:     self.info(volume)?.metadata.volume,
                                  object:     *object.as_bytes(),
                                  catalog_id: id,
                                  revision:   0,
                                  patch:      Default::default() };
        let mut bytes = [0; annotation::BYTES];
        record.encode(&mut bytes)?;
        async {
            // Another cooperating writer may have bound it after our read.
            if let Some(existing) = self.annotation(volume, object).await?
            {
                Annotation::decode(&existing)?;
                return Ok(existing);
            }
            let directory = self.annotation_directory(volume).await?.ok_or(Cause::Invalid)?;
            let name = annotation::key(object.as_bytes());
            let stage = self.call(Op::BeginWrite, directory, 0, 0, &[]).await?.header.object;
            let staged: Result<()> =
                async {
                    self.call(Op::SetLength, stage, bytes.len() as u64, 0, &[]).await?;
                    self.call(Op::Write, stage, 0, 0, &bytes).await?;
                    self.call(Op::Flush, stage, 0, 0, &[]).await?;
                    self.call(Op::Commit, stage, 0, 0, &name).await?;
                    Ok(())
                }.await;
            if let Err(error) = staged
            {
                // Close acknowledges either staged-file removal or durability
                // of an already published record. The caller never exposes an
                // identity after an unacknowledged persistence failure.
                return self.cleanup(stage, Err(error)).await;
            }
            Ok(bytes)
        }.await
    }

    pub(super) async fn decorate_file(&mut self, volume: u32, handle: u32, mut node: Node)
                                      -> Result<Node>
    {
        let reply = self.call(Op::DescribeObject, handle, 0, 0, &[]).await?;
        let source = wire::Description::decode(&reply.body).map_err(|_| Cause::Invalid)?;
        let (id, annotation) =
            self.catalog_identity(volume, reply.header.object, source.identity).await?;
        node.metadata.volume = self.info(volume)?.metadata.volume;
        node.metadata.catalog_id = id;
        node.metadata.identity = forks::catalog_identity(node.metadata.volume, id)?;
        if let Some(bytes) = annotation
        {
            apply(&mut node.metadata, Annotation::decode(&bytes)?)?;
        }
        node.source = Some(Source { identity: source.identity,
                                    revision: source.revision });
        Ok(node)
    }

    /// Directory identity follows the physical directory, never the optional
    /// metadata marker's header or inode. Host renames remain authoritative.
    pub(super) async fn directory_node(&mut self, volume: u32, parent: u32, name: &[u8],
                                       directory: u32)
                                       -> Result<Node>
    {
        let mut node = match self.open_record(directory, forks::DIRECTORY_RECORD.as_bytes()).await
        {
            Ok((record, node)) =>
            {
                self.close(record).await?;
                if node.metadata.kind != Kind::Directory
                {
                    return Err(Cause::Invalid.into());
                }
                self.decorate_file(volume, directory, node).await?
            }
            Err(error) if error.cause == Cause::Host(wire::status::NOT_FOUND) =>
            {
                self.plain_node(volume, parent, name).await?
            }
            Err(error) => return Err(error)
        };
        crate::module_allocation(|| {
            node.metadata.name =
                mac_hfs_b::names::normalize(core::str::from_utf8(name).map_err(|_| {
                                                                          Cause::Invalid
                                                                      })?).map_err(|_| {
                                                                              Cause::Invalid
                                                                          })?;
            node.key =
                mac_hfs_b::names::comparison(&node.metadata.name).map_err(|_| Cause::Invalid)?;
            node.host_name = buffer(name.len())?;
            node.host_name.copy_from_slice(name);
            Ok(())
        })?;
        Ok(node)
    }

    /// Publish an already encoded annotation under the existing writer lease.
    /// Compare the exact prior bytes before replacing; every failed stage owns
    /// an acknowledged cleanup, including errors after the publication barrier.
    pub(super) async fn publish_annotation_locked(&mut self, volume: u32,
                                                  object: wire::ObjectIdentity,
                                                  previous: &[u8; annotation::BYTES],
                                                  desired: &[u8; annotation::BYTES])
                                                  -> Result<()>
    {
        let directory = self.annotation_directory(volume).await?.ok_or(Cause::Invalid)?;
        let name = annotation::key(object.as_bytes());
        let opened = self.call(Op::OpenRead, directory, 0, 0, &name).await?.header.object;
        let result: Result<()> =
            async {
                let current = self.call(Op::Read, opened, 0, annotation::BYTES as u32, &[]).await?;
                if current.body.as_slice() == desired
                {
                    self.call(Op::SyncDirectory, directory, 0, 0, &[]).await?;
                    return Ok(());
                }
                if current.body.as_slice() != previous
                {
                    return Err(Cause::Stale.into());
                }
                let stage = self.call(Op::BeginWrite, directory, 0, 0, &[]).await?.header.object;
                let result: Result<()> =
                    async {
                        self.call(Op::SetLength, stage, annotation::BYTES as u64, 0, &[]).await?;
                        self.call(Op::Write, stage, 0, 0, desired).await?;
                        self.call(Op::Flush, stage, 0, 0, &[]).await?;
                        self.call(Op::Commit, stage, 0, opened, &name).await?;
                        Ok(())
                    }.await;
                match result
                {
                    Ok(()) => Ok(()),
                    Err(error) => self.cleanup(stage, Err(error)).await
                }
            }.await;
        self.cleanup(opened, result).await
    }

    pub(super) async fn open_file_record(&mut self, volume: u32, parent: u32, name: &[u8])
                                         -> Result<(u32, Node)>
    {
        let (handle, mut node) = self.open_record(parent, name).await?;
        let prepared: Result<()> = crate::module_allocation(|| {
            node.metadata.name =
                mac_hfs_b::names::normalize(core::str::from_utf8(name).map_err(|_| {
                                                                          Cause::Invalid
                                                                      })?).map_err(|_| {
                                                                              Cause::Invalid
                                                                          })?;
            node.key =
                mac_hfs_b::names::comparison(&node.metadata.name).map_err(|_| Cause::Invalid)?;
            Ok(())
        });
        if let Err(error) = prepared
        {
            return self.cleanup(handle, Err(error)).await;
        }
        match self.decorate_file(volume, handle, node).await
        {
            Ok(node) => Ok((handle, node)),
            Err(error) => self.cleanup(handle, Err(error)).await
        }
    }

    pub(super) async fn annotation_directory(&mut self, volume: u32) -> Result<Option<u32>>
    {
        let mount = self.mounts.get(volume).ok_or(Cause::NotFound)?;
        match mount.annotations
        {
            Directory::Open(handle) => return Ok(Some(handle)),
            Directory::Missing => return Ok(None),
            Directory::Unknown => ()
        }
        let root = mount.root;
        match self.call(Op::OpenDirectory, root, 0, 0, annotation::DIRECTORY.as_bytes()).await
        {
            Ok(reply) =>
            {
                let handle = reply.header.object;
                self.mounts.get_mut(volume).unwrap().annotations = Directory::Open(handle);
                Ok(Some(handle))
            }
            Err(error) if error.cause == Cause::Host(wire::status::NOT_FOUND) =>
            {
                self.mounts.get_mut(volume).unwrap().annotations = Directory::Missing;
                Ok(None)
            }
            Err(error) => Err(error)
        }
    }

    pub(super) async fn annotation(&mut self, volume: u32, object: wire::ObjectIdentity)
                                   -> Result<Option<[u8; annotation::BYTES]>>
    {
        let Some(directory) = self.annotation_directory(volume).await?
        else
        {
            return Ok(None);
        };
        let opened = match self.call(Op::OpenRead,
                                     directory,
                                     0,
                                     0,
                                     &annotation::key(object.as_bytes()))
                               .await
        {
            Ok(reply) => reply,
            Err(error) if error.cause == Cause::Host(wire::status::NOT_FOUND) => return Ok(None),
            Err(error) => return Err(error)
        };
        let handle = opened.header.object;
        let result = async {
                         if opened.header.offset != annotation::BYTES as u64
                         {
                             return Err(Cause::Format(forks::Error::Corrupt).into());
                         }
                         let reply =
                             self.call(Op::Read, handle, 0, annotation::BYTES as u32, &[]).await?;
                         let bytes: [u8; annotation::BYTES] =
                             reply.body
                                  .as_slice()
                                  .try_into()
                                  .map_err(|_| Cause::Format(forks::Error::Corrupt))?;
                         let record = Annotation::decode(&bytes)?;
                         if record.object != *object.as_bytes()
                            || record.volume != self.info(volume)?.metadata.volume
                         {
                             return Err(Cause::Format(forks::Error::Corrupt).into());
                         }
                         Ok(bytes)
                     }.await;
        self.cleanup(handle, result).await.map(Some)
    }
}

pub(super) fn apply(metadata: &mut Metadata, record: Annotation<'_>) -> Result<()>
{
    crate::module_allocation(|| record.apply(metadata).map_err(Into::into))
}

//! Bind a newly published physical file before acknowledging its logical CNID.
//! Replacing a file preserves its catalog identity and retires the old physical
//! annotation. Imported/copied containers without a binding receive a new ID.
use forks::annotation::{self, Annotation};

use super::*;
impl<R: Rpc> Filesystem<R>
{
    pub(super) async fn bind_file_publication(&mut self, token: u64) -> Result<()>
    {
        let pending = self.writes.get(token).ok_or(Cause::NotFound)?;
        if pending.identity_bound
        {
            return Ok(());
        }
        if !pending.published || pending.metadata_name.is_some() || pending.rename.is_some()
        {
            return Err(Cause::Invalid.into());
        }
        let (volume, id, parent, old) =
            (pending.volume, pending.cnid, pending.parent_handle, pending.previous_object);
        let node = self.cached_node(volume, id)?;
        let mut name = buffer(node.host_name.len())?;
        name.copy_from_slice(&node.host_name);
        let mut desired = node.try_clone()?;
        let is_directory = node.metadata.kind == Kind::Directory;
        let opened = if is_directory
        {
            self.mounts.get(volume).unwrap().directories.get(id).ok_or(Cause::NotFound)?.handle
        }
        else
        {
            self.call(Op::OpenRead, parent, 0, 0, &name).await?.header.object
        };
        let result: Result<()> =
            async {
                let described = self.call(Op::DescribeObject, opened, 0, 0, &[]).await?;
                let source =
                    wire::Description::decode(&described.body).map_err(|_| Cause::Invalid)?;
                let object = source.identity.ok_or(Cause::Unsupported)?;
                let bytes = self.persist_identity_locked(volume, object, id).await?;
                let record = Annotation::decode(&bytes)?;
                if record.catalog_id != id
                {
                    return Err(Cause::Stale.into());
                }
                // Finish all fallible metadata preparation before changing cache state.
                record.apply(&mut desired.metadata)?;
                desired.source = Some(Source { identity: source.identity,
                                               revision: source.revision });
                if let Some(before) = old.filter(|before| *before != object)
                {
                    self.retire_file_annotation(volume, before, id).await?;
                }
                self.bind_published_identity(volume, object, id)?;
                *self.mounts.get_mut(volume).unwrap().nodes.get_mut(id).ok_or(Cause::NotFound)? =
                    desired;
                self.writes.get_mut(token).unwrap().identity_bound = true;
                Ok(())
            }.await;
        if is_directory { result } else { self.cleanup(opened, result).await }
    }

    pub(in crate::volume) async fn retire_file_annotation(&mut self, volume: u32,
                                                          object: wire::ObjectIdentity, id: u32)
                                                          -> Result<()>
    {
        let Some(bytes) = self.annotation(volume, object).await?
        else
        {
            // A preceding unlink may have succeeded before its directory sync
            // failed. A retry still has to acknowledge that durability barrier.
            if let Some(directory) = self.annotation_directory(volume).await?
            {
                self.call(Op::SyncDirectory, directory, 0, 0, &[]).await?;
            }
            return Ok(());
        };
        if Annotation::decode(&bytes)?.catalog_id != id
        {
            return Err(Cause::Stale.into());
        }
        let directory = self.annotation_directory(volume).await?.ok_or(Cause::Invalid)?;
        let name = annotation::key(object.as_bytes());
        let opened = self.call(Op::OpenRead, directory, 0, 0, &name).await?.header.object;
        let result: Result<()> =
            async {
                let current = self.call(Op::Read, opened, 0, annotation::BYTES as u32, &[]).await?;
                if current.body.as_slice() != bytes
                {
                    return Err(Cause::Stale.into());
                }
                self.call(Op::Remove, directory, 0, opened, &name).await?;
                Ok(())
            }.await;
        self.cleanup(opened, result).await
    }
}

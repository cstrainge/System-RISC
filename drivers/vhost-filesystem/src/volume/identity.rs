//! Catalog identities are persistent data, distinct from live capability tokens.
//! Reserve disjoint counter ranges durably; retain bindings across cache refresh.
use super::*;
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Key
{
    object:       Option<wire::ObjectIdentity>,
    session_node: u32
}
pub(super) struct Binding
{
    key:     Key,
    id:      u32,
    // False entries are private reservations after an interrupted first bind;
    // their IDs have never been published through a catalog result.
    durable: bool
}
impl Binding
{
    pub(super) fn catalog_id(&self) -> u32
    {
        self.id
    }
}
impl<R: Rpc> Filesystem<R>
{
    /// Every physical file gets a projection, including copied native fork
    /// containers. Embedded IDs describe the source volume, not this incarnation.
    pub(super) async fn catalog_identity(&mut self, volume: u32, host: u32,
                                         object: Option<wire::ObjectIdentity>)
                                         -> Result<(u32, Option<[u8; forks::annotation::BYTES]>)>
    {
        let mut annotation =
            if let Some(object) = object { self.annotation(volume, object).await? } else { None };
        let stored = annotation.as_ref().map(forks::annotation::Annotation::decode).transpose()?;
        let mut id =
            self.plain_identity(volume, host, object, stored.map(|r| r.catalog_id)).await?;
        if annotation.is_none() && !self.info(volume)?.read_only
        {
            let object = object.ok_or(Cause::Unsupported)?;
            annotation = Some(self.persist_plain_identity(volume, object, id).await?);
            let stored = forks::annotation::Annotation::decode(annotation.as_ref().unwrap())?;
            id = self.plain_identity(volume, host, Some(object), Some(stored.catalog_id)).await?;
        }
        Ok((id, annotation))
    }

    /// Only an acknowledged in-volume replacement may transfer a logical ID
    /// between physical incarnations. Retire the old key; an old inode brought
    /// back by the host is subsequently adopted as a new file, never this ID.
    pub(super) fn bind_published_identity(&mut self, volume: u32, object: wire::ObjectIdentity,
                                          id: u32)
                                          -> Result<()>
    {
        let mount = self.mounts.get_mut(volume).ok_or(Cause::NotFound)?;
        let key = Key { object:       Some(object),
                        session_node: 0 };
        if mount.bindings.iter().any(|b| b.key == key && b.id != id)
        {
            return Err(Cause::Stale.into());
        }
        crate::module_reserve(&mut mount.bindings, 1)?;
        mount.bindings.retain(|b| b.id != id || b.key == key);
        match mount.bindings.binary_search_by_key(&key, |b| b.key)
        {
            Ok(at) => mount.bindings[at].durable = true,
            Err(at) =>
            {
                mount.bindings.insert(at,
                                      Binding { key,
                                                id,
                                                durable: true })
            }
        }
        Ok(())
    }

    pub(super) async fn plain_identity(&mut self, volume: u32, host: u32,
                                       object: Option<wire::ObjectIdentity>, stored: Option<u32>)
                                       -> Result<u32>
    {
        let key = Key { object,
                        session_node: if object.is_some() { 0 } else { host } };
        let mount = self.mounts.get_mut(volume).ok_or(Cause::NotFound)?;
        let at = match mount.bindings.binary_search_by_key(&key, |entry| entry.key)
        {
            Ok(at) =>
            {
                let id = mount.bindings[at].id;
                if let Some(stored) = stored
                {
                    if stored <= ROOT || stored >= 0x8000_0000
                    {
                        return Err(Cause::Invalid.into());
                    }
                    if stored != id
                    {
                        if mount.bindings[at].durable
                           || mount.bindings.iter().any(|entry| entry.id == stored)
                           || mount.nodes.get(stored).is_some()
                        {
                            return Err(Cause::Stale.into());
                        }
                        // A cooperating writer won an interrupted, unexposed
                        // first bind. Adopt its durable record, never a guessed ID.
                        mount.bindings[at].id = stored;
                    }
                    mount.bindings[at].durable = true;
                    return Ok(stored);
                }
                return Ok(id);
            }
            Err(at) => at
        };
        if mount.bindings.len() == MAX_NODES
        {
            return Err(Cause::NoMemory.into());
        }
        crate::module_reserve(&mut mount.bindings, 1)?;
        let id = if let Some(stored) = stored
        {
            if stored <= ROOT || stored >= 0x8000_0000
            {
                return Err(Cause::Invalid.into());
            }
            stored
        }
        else if mount.info.read_only
        {
            // Unannotated read-only objects cannot acquire persistent metadata.
            // Host node pins remain live for this session; volume generations
            // reject IDs retained across eject/remount.
            host | 0x8000_0000
        }
        else
        {
            if mount.plain_next == mount.plain_limit
            {
                return Err(Cause::NoMemory.into());
            }
            let mount = self.mounts.get_mut(volume).unwrap();
            let id = mount.plain_next;
            mount.plain_next += 1;
            id
        };
        let mount = self.mounts.get_mut(volume).unwrap();
        if mount.bindings.iter().any(|entry| entry.id == id) || mount.nodes.get(id).is_some()
        {
            return Err(Cause::Conflict.into());
        }
        mount.bindings.insert(at,
                              Binding { key,
                                        id,
                                        durable: stored.is_some() || mount.info.read_only });
        Ok(id)
    }
}

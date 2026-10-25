//! Stepped group moves with one owned lease and reverse-order undo. Internal
//! rows have no public IDs; the only live token is supplied by resident System.
use mac_filesystem_interface::{IconMoves, MetadataPatch};

use super::*;
#[derive(Clone, Copy)]
enum Pending
{
    None,
    Move,
    Metadata,
    UndoMove,
    UndoMetadata,
    Stamp
}
struct Row
{
    before: Node,
    parent: u32,
    info:   [u8; 32],
    moved:  bool,
    edited: bool
}
struct Stamp
{
    id:       u32,
    modified: u64
}
fn placement_info(before: &Node, row: mac_filesystem_interface::IconMove) -> [u8; 32]
{
    let mut info = before.metadata.finder_info;
    if row.placement & 3 != 0
    {
        let flags = u16::from_be_bytes(info[8..10].try_into().unwrap());
        let (flags, h, v) = if row.placement & 3 == 1
        {
            (flags | 0x100, row.horizontal, row.vertical)
        }
        else
        {
            (flags & !0x100, -1, -1)
        };
        info[8..10].copy_from_slice(&flags.to_be_bytes());
        info[10..12].copy_from_slice(&v.to_be_bytes());
        info[12..14].copy_from_slice(&h.to_be_bytes());
    }
    if row.placement & 4 != 0 && before.parent != row.parent
    {
        info[28..32].copy_from_slice(&before.parent.to_be_bytes());
    }
    info
}
pub(super) struct Group
{
    pub volume:   u32,
    pub lock:     u32,
    locked:       bool,
    rows:         Vec<Row>,
    cursor:       usize,
    undo:         Option<usize>,
    rolling_back: bool,
    committed:    bool,
    pending:      Pending,
    when:         u64,
    error:        Option<crate::Error>,
    stamps:       Vec<Stamp>,
    stamp_cursor: usize,
    undo_stamps:  usize
}
impl<R: Rpc> Filesystem<R>
{
    pub(crate) fn has_group(&self, volume: u32, token: u64) -> bool
    {
        self.groups.get(token).is_some_and(|group| group.volume == volume)
    }

    pub(crate) fn group_busy(&self, volume: u32) -> bool
    {
        let identity = self.mounts.get(volume).and_then(|mount| mount.root_identity);
        self.groups.entries.iter().any(|(_, group)| {
                                      group.volume==volume || identity.is_some()
            && self.mounts.get(group.volume).is_some_and(|mount|mount.root_identity==identity)
                                  })
    }

    pub async fn begin_icon_moves(&mut self, volume: u32, token: u64, rows: IconMoves<'_>,
                                  when: u64)
                                  -> Result<[u32; 4]>
    {
        self.owner = volume;
        self.writable(volume)?;
        if self.group_busy(volume) || self.writes.get(token).is_some()
        {
            return Err(Cause::Busy.into());
        }
        self.groups.reserve(token)?;
        let mut desired = Vec::new();
        let mut stamps = Vec::new();
        let mut ids = Vec::new();
        desired.try_reserve_exact(rows.len()).map_err(|_| Cause::NoMemory)?;
        ids.try_reserve_exact(rows.len()).map_err(|_| Cause::NoMemory)?;
        for i in 0..rows.len()
        {
            ids.push(rows.at(i).ok_or(Cause::Invalid)?.id);
        }
        ids.sort_unstable();
        if ids.windows(2).any(|pair| pair[0] == pair[1])
        {
            return Err(Cause::Invalid.into());
        }
        if (0..rows.len()).all(|index| {
                              let Some(row) = rows.at(index)
                              else
                              {
                                  return false;
                              };
                              self.mounts
                                  .get(volume)
                                  .and_then(|m| m.nodes.get(row.id))
                                  .is_some_and(|before| {
                                      row.expected_parent == before.parent
                                      && row.parent == before.parent
                                      && placement_info(before, row) == before.metadata.finder_info
                                  })
                          })
        {
            return Ok([1, rows.len() as u32, rows.len() as u32, 0]);
        }
        // Complete all storage-independent admission before acquiring a writer.
        // Once acquired, cache snapshots are held until commit or undo ACK.
        let lock = self.writer_lock(volume).await?;
        let prepared: Result<bool> =
            async {
                let mut changed = false;
                for index in 0..rows.len()
                {
                    let row = rows.at(index).ok_or(Cause::Invalid)?;
                    self.find(volume, row.id).await?;
                    self.find(volume, row.parent).await?;
                    let before = self.cached_node(volume, row.id)?.try_clone()?;
                    if before.parent != row.expected_parent
                    {
                        return Err(Cause::Stale.into());
                    }
                    let target = self.cached_node(volume, row.parent)?;
                    if target.metadata.kind == Kind::File
                    {
                        return Err(Cause::Invalid.into());
                    }
                    if target.metadata.attributes & 1 != 0
                    {
                        return Err(Cause::Protected.into());
                    }
                    if before.parent != row.parent
                    {
                        if self.cached_node(volume, before.parent)?.metadata.attributes & 1 != 0
                        {
                            return Err(Cause::Protected.into());
                        }
                        if before.metadata.attributes & 1 != 0
                           || u16::from_be_bytes(before.metadata.finder_info[8..10].try_into()
                                                                                   .unwrap())
                              & 0x1000
                              != 0
                        {
                            return Err(Cause::Protected.into());
                        }
                        if self.is_open(volume, row.id)
                        {
                            return Err(Cause::Busy.into());
                        }
                    }
                    if row.placement & 4 != 0
                       && before.parent != row.parent
                       && (target.parent != ROOT
                           || mac_hfs_b::names::comparison(&target.metadata.name).map_err(|_| {
                                                                                     Cause::Invalid
                                                                                 })?
                              != "trash")
                    {
                        return Err(Cause::Invalid.into());
                    }
                    self.scan(volume, before.parent).await?;
                    self.scan(volume, row.parent).await?;
                    if before.parent != row.parent
                    {
                        let mount = self.mounts.get(volume).unwrap();
                        let target = mount.directories.get(row.parent).ok_or(Cause::NotFound)?;
                        if target.by_name.iter().any(|id| {
                                                    *id != row.id
                                                    && mount.nodes.get(*id).unwrap().key
                                                       == before.key
                                                })
                        {
                            return Err(Cause::Conflict.into());
                        }
                    }
                    let info = placement_info(&before, row);
                    changed |= before.parent != row.parent || before.metadata.finder_info != info;
                    desired.push(Row { before,
                                       parent: row.parent,
                                       info,
                                       moved: false,
                                       edited: false });
                    yield_once().await;
                }
                // Color the proposed parent graph once. Shared ancestry is not
                // rescanned for every selected folder; indexes remain sorted.
                let mut ordered = Vec::new();
                ordered.try_reserve_exact(desired.len()).map_err(|_| Cause::NoMemory)?;
                ordered.extend(desired.iter()
                                      .enumerate()
                                      .map(|(i, row)| (row.before.metadata.catalog_id, i)));
                ordered.sort_unstable_by_key(|entry| entry.0);
                let nodes = &self.mounts.get(volume).unwrap().nodes.entries;
                let mut color = Vec::new();
                let mut stack = Vec::new();
                color.try_reserve_exact(nodes.len()).map_err(|_| Cause::NoMemory)?;
                stack.try_reserve_exact(nodes.len()).map_err(|_| Cause::NoMemory)?;
                color.resize(nodes.len(), 0u8);
                for row in &desired
                {
                    if row.before.metadata.kind == Kind::File
                    {
                        continue;
                    }
                    let mut id = row.before.metadata.catalog_id;
                    loop
                    {
                        if id == ROOT
                        {
                            break;
                        }
                        let at = nodes.binary_search_by_key(&id, |entry| entry.0)
                                      .map_err(|_| Cause::NotFound)?;
                        match color[at]
                        {
                            2 => break,
                            1 => return Err(Cause::Invalid.into()),
                            _ => ()
                        }
                        color[at] = 1;
                        stack.push(at);
                        id = if let Ok(i) = ordered.binary_search_by_key(&id, |entry| entry.0)
                        {
                            desired[ordered[i].1].parent
                        }
                        else
                        {
                            nodes[at].1.parent
                        };
                        if stack.len() > MAX_NODES
                        {
                            return Err(Cause::Invalid.into());
                        }
                    }
                    for at in stack.drain(..)
                    {
                        color[at] = 2;
                    }
                }
                // Within-batch folded-name collisions are errors before publication.
                let mut names = Vec::new();
                names.try_reserve_exact(desired.len()).map_err(|_| Cause::NoMemory)?;
                names.extend(desired.iter().map(|row| (row.parent, row.before.key.as_str())));
                names.sort_unstable();
                if names.windows(2).any(|pair| pair[0] == pair[1])
                {
                    return Err(Cause::Conflict.into());
                }
                // Capture each affected directory once, before any row changes.
                // Root is last during reverse restoration because undo writes
                // also update the volume's date. Catalog counters never rewind.
                let mut parents = Vec::new();
                parents.try_reserve_exact(desired.len() * 2 + 1).map_err(|_| Cause::NoMemory)?;
                parents.push(ROOT);
                for row in &desired
                {
                    if row.before.parent != row.parent
                    {
                        parents.extend_from_slice(&[row.before.parent, row.parent]);
                    }
                }
                parents.sort_unstable();
                parents.dedup();
                stamps.try_reserve_exact(parents.len()).map_err(|_| Cause::NoMemory)?;
                for id in parents
                {
                    stamps.push(Stamp { id,
                                        modified: self.cached_node(volume, id)?
                                                      .metadata
                                                      .modified });
                }
                Ok(changed)
            }.await;
        match prepared
        {
            Ok(true) =>
            {
                let count = desired.len() as u32;
                self.groups.insert(token,
                                    Group { volume,
                                            lock,
                                            locked: true,
                                            rows: desired,
                                            cursor: 0,
                                            undo: None,
                                            rolling_back: false,
                                            committed: false,
                                            pending: Pending::None,
                                            when,
                                            error: None,
                                            undo_stamps: stamps.len(),
                                            stamps,
                                            stamp_cursor: 0 })?;
                Ok([0, 0, count, 0])
            }
            Ok(false) =>
            {
                self.release_writer(lock).await?;
                Ok([1, rows.len() as u32, rows.len() as u32, 0])
            }
            Err(error) => self.cleanup_group_lock(lock, Err(error)).await
        }
    }

    async fn cleanup_group_lock<T>(&mut self, lock: u32, result: Result<T>) -> Result<T>
    {
        match (result, self.release_writer(lock).await)
        {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) => Err(error),
            (result, Err(cleanup)) =>
            {
                let mut error = result.err().unwrap_or(cleanup);
                error.cleanup = Some(cleanup.cause);
                Err(error)
            }
        }
    }

    async fn group_move(&mut self, token: u64, group: &mut Group, index: usize, undo: bool)
                        -> Result<()>
    {
        let row = &group.rows[index];
        let (parent, name) = if undo
        {
            (row.before.parent, row.before.metadata.name.as_str())
        }
        else
        {
            (row.parent, row.before.metadata.name.as_str())
        };
        let staged = self.begin_rename(group.volume, row.before.metadata.catalog_id, parent, name,
                                       token, group.when)
                         .await?;
        if staged
        {
            group.pending = if undo { Pending::UndoMove } else { Pending::Move };
            self.finish_write(group.volume, token).await?;
            group.pending = Pending::None;
        }
        group.rows[index].moved = !undo;
        Ok(())
    }

    async fn group_metadata(&mut self, token: u64, group: &mut Group, index: usize, undo: bool)
                            -> Result<()>
    {
        let row = &group.rows[index];
        let patch = MetadataPatch { finder_info: if undo
                                    {
                                        row.before.metadata.finder_info
                                    }
                                    else
                                    {
                                        row.info
                                    },
                                    finder_mask: [255; 32],
                                    ..Default::default() };
        if self.begin_metadata(group.volume, row.before.metadata.catalog_id, token, patch,
                               group.when)
               .await?
        {
            group.pending = if undo { Pending::UndoMetadata } else { Pending::Metadata };
            self.finish_write(group.volume, token).await?;
            group.pending = Pending::None;
        }
        group.rows[index].edited = !undo;
        Ok(())
    }

    async fn group_drain(&mut self, token: u64, group: &mut Group, index: usize) -> Result<()>
    {
        if matches!(group.pending, Pending::None)
        {
            return Ok(());
        }
        if self.abort_write(group.volume, token).await?
        {
            match group.pending
            {
                Pending::Move => group.rows[index].moved = true,
                Pending::Metadata => group.rows[index].edited = true,
                Pending::UndoMove => group.rows[index].moved = false,
                Pending::UndoMetadata => group.rows[index].edited = false,
                Pending::None | Pending::Stamp => ()
            }
        }
        group.pending = Pending::None;
        Ok(())
    }

    async fn group_stamp(&mut self, token: u64, group: &mut Group, index: usize, undo: bool)
                         -> Result<()>
    {
        let stamp = &group.stamps[index];
        let modified = if undo { stamp.modified } else { group.when };
        let when = if stamp.id == ROOT { modified } else { group.when };
        if self.begin_metadata(group.volume,
                               stamp.id,
                               token,
                               MetadataPatch { modified: Some(modified),
                                               ..Default::default() },
                               when)
               .await?
        {
            group.pending = Pending::Stamp;
            self.finish_write(group.volume, token).await?;
            group.pending = Pending::None;
        }
        Ok(())
    }

    async fn group_unlock(&mut self, group: &mut Group) -> Result<()>
    {
        if group.lock == 0
        {
            return Ok(());
        }
        if group.locked
        {
            self.call(Op::Unlock, group.lock, 0, 0, &[]).await?;
            group.locked = false;
        }
        self.close(group.lock).await?;
        group.lock = 0;
        Ok(())
    }

    pub async fn step_icon_moves(&mut self, volume: u32, token: u64, cancel: bool)
                                 -> Result<[u32; 4]>
    {
        self.owner = volume;
        if self.groups.get(token).is_none_or(|group| group.volume != volume)
        {
            return Err(Cause::NotFound.into());
        }
        let mut group = self.groups.remove(token).unwrap();
        self.batch_lock = Some((volume, group.lock));
        if cancel && !group.committed && !group.rolling_back
        {
            group.rolling_back = true;
            group.undo = group.cursor.min(group.rows.len().saturating_sub(1)).checked_add(1);
        }
        let result: Result<[u32; 4]> =
            async {
                if group.committed
                {
                    self.group_unlock(&mut group).await?;
                    return Ok([1, group.rows.len() as u32, group.rows.len() as u32, 1]);
                }
                if group.rolling_back
                {
                    if let Some(remaining) = group.undo
                    {
                        if remaining != 0
                        {
                            let index = remaining - 1;
                            self.group_drain(token, &mut group, index).await?;
                            if group.rows[index].edited
                            {
                                self.group_metadata(token, &mut group, index, true).await?;
                            }
                            if group.rows[index].moved
                            {
                                self.group_move(token, &mut group, index, true).await?;
                            }
                            group.undo = Some(index);
                            return Ok([2, index as u32, group.rows.len() as u32, 0]);
                        }
                    }
                    if group.undo_stamps != 0
                    {
                        self.group_drain(token, &mut group, 0).await?;
                        let index = group.undo_stamps - 1;
                        self.group_stamp(token, &mut group, index, true).await?;
                        group.undo_stamps = index;
                        return Ok([2, 0, group.rows.len() as u32, 0]);
                    }
                    self.group_unlock(&mut group).await?;
                    return if let Some(error) = group.error.filter(|_| !cancel)
                    {
                        Err(error)
                    }
                    else
                    {
                        Ok([3, 0, group.rows.len() as u32, 0])
                    };
                }
                let index = group.cursor;
                if index < group.rows.len()
                {
                    if group.rows[index].before.parent != group.rows[index].parent
                    {
                        self.group_move(token, &mut group, index, false).await?;
                    }
                    if group.rows[index].before.metadata.finder_info != group.rows[index].info
                    {
                        self.group_metadata(token, &mut group, index, false).await?;
                    }
                    group.cursor += 1;
                }
                if group.cursor == group.rows.len()
                {
                    if group.stamp_cursor < group.stamps.len()
                    {
                        let index = group.stamp_cursor;
                        self.group_stamp(token, &mut group, index, false).await?;
                        group.stamp_cursor += 1;
                        return Ok([0, group.cursor as u32, group.rows.len() as u32, 0]);
                    }
                    group.committed = true;
                    self.group_unlock(&mut group).await?;
                    Ok([1, group.cursor as u32, group.rows.len() as u32, 1])
                }
                else
                {
                    Ok([0, group.cursor as u32, group.rows.len() as u32, 0])
                }
            }.await;
        self.batch_lock = None;
        let terminal = matches!(result, Ok([1 | 3, _, _, _]));
        let result = match result
        {
            Err(error) if !group.rolling_back && !group.committed =>
            {
                group.error = Some(error);
                group.rolling_back = true;
                group.undo = Some((group.cursor + 1).min(group.rows.len()));
                Ok([2, group.cursor as u32, group.rows.len() as u32, 0])
            }
            Err(mut error) if group.rolling_back && !(group.undo == Some(0) && group.lock == 0) =>
            {
                error.cleanup = Some(error.cause);
                if let Some(mount) = self.mounts.get_mut(volume)
                {
                    mount.invalidated = true;
                    mount.info.read_only = true;
                }
                Err(error)
            }
            result => result
        };
        if !terminal
        {
            self.groups
                .insert(token, group)
                .unwrap_or_else(|_| unreachable!("removed group retains its reserved index slot"));
        }
        result
    }
}

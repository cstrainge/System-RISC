//! Ownership and lifetime policy for backing-file capabilities. I/O adapters
//! take owned plans out of their short guard, then acknowledge completion here.
//! `B` is an opaque backing token; it is never a borrowed file or caller buffer.
#![forbid(unsafe_code)]
use mac_abi::{ACCESS_DENIED, ALREADY_EXISTS, BAD_ABI, BAD_ARGUMENT, NO_MEMORY, NOT_FOUND,
              ROLLBACK_FAILED, SERVICE_BUSY, Status};
use mac_file_source_interface::{Info, MAX_TRANSFER};

use crate::toolbox::handles::{Kind, Table};
#[cfg(target_arch = "riscv64")]
pub(crate) mod runtime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity
{
    pub volume: u32,
    pub cnid:   u32,
    pub fork:   u32
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase
{
    Ready,
    Reading(u32),
    Closing,
    Quarantined
}
struct Source<B>
{
    owner:    u64,
    identity: Identity,
    backing:  B,
    length:   u64,
    phase:    Phase
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Read<B>
{
    pub backing: B,
    pub offset:  u64,
    pub length:  usize
}
pub struct Sources<B>
{
    entries: Table<Source<B>>
}
impl<B: Clone> Default for Sources<B>
{
    fn default() -> Self
    {
        Self::new()
    }
}
impl<B: Clone> Sources<B>
{
    pub const fn new() -> Self
    {
        Self { entries: Table::new(Kind::FileSources) }
    }

    fn owned(&self, owner: u64, source: u32) -> Result<&Source<B>, Status>
    {
        let source = self.entries.get(source).ok_or(NOT_FOUND)?;
        if owner == 0 || source.owner != owner
        {
            return Err(ACCESS_DENIED);
        }
        Ok(source)
    }

    fn accessible(source: &Source<B>) -> Result<(), Status>
    {
        match source.phase
        {
            Phase::Ready | Phase::Reading(_) => Ok(()),
            Phase::Closing => Err(SERVICE_BUSY),
            Phase::Quarantined => Err(ROLLBACK_FAILED)
        }
    }

    /// Capture the mounted-volume generation and persistent file/fork identity.
    /// A renamed source retains the same capability; a copied file does not.
    pub fn find(&self, owner: u64, identity: Identity) -> Result<Option<u32>, Status>
    {
        if owner == 0
        {
            return Err(ACCESS_DENIED);
        }
        let Some((id, source)) =
            self.entries
                .iter()
                .find(|(_, source)| source.owner == owner && source.identity == identity)
        else
        {
            return Ok(None);
        };
        Self::accessible(source)?;
        Ok(Some(id))
    }

    /// Call only after the adapter has an acknowledged, owned backing read
    /// lease. On failure the adapter still owns and must release that lease.
    pub fn publish(&mut self, owner: u64, identity: Identity, backing: B, length: u64)
                   -> Result<u32, Status>
    {
        if owner == 0
        {
            return Err(ACCESS_DENIED);
        }
        if identity.volume == 0 || identity.cnid < 2 || identity.fork > 1
        {
            return Err(BAD_ARGUMENT);
        }
        if self.find(owner, identity)?.is_some()
        {
            return Err(ALREADY_EXISTS);
        }
        self.entries
            .insert(Source { owner,
                             identity,
                             backing,
                             length,
                             phase: Phase::Ready })
            .map_err(|_| NO_MEMORY)
    }

    /// Metadata remains available during a read, without entering the backend.
    pub fn info(&self, owner: u64, source: u32) -> Result<Info, Status>
    {
        let source = self.owned(owner, source)?;
        Self::accessible(source)?;
        Ok(Info { length: source.length })
    }

    /// The ticket is issued/validated by the generic request broker. This core
    /// records it to prevent a different request from acknowledging this read.
    pub fn begin_read(&mut self, owner: u64, source: u32, ticket: u32, offset: u64,
                      capacity: usize)
                      -> Result<Read<B>, Status>
    {
        if ticket == 0 || capacity > MAX_TRANSFER
        {
            return Err(BAD_ARGUMENT);
        }
        let entry = self.owned(owner, source)?;
        Self::accessible(entry)?;
        if entry.phase != Phase::Ready
        {
            return Err(SERVICE_BUSY);
        }
        let length =
            entry.length.checked_sub(offset).ok_or(BAD_ARGUMENT)?.min(capacity as u64) as usize;
        let read = Read { backing: entry.backing.clone(),
                          offset,
                          length };
        self.entries.get_mut(source).ok_or(NOT_FOUND)?.phase = Phase::Reading(ticket);
        Ok(read)
    }

    /// A read error/cancellation also waits for actual backend acknowledgement.
    pub fn finish_read(&mut self, owner: u64, source: u32, ticket: u32) -> Result<(), Status>
    {
        if self.owned(owner, source)?.phase != Phase::Reading(ticket)
        {
            return Err(BAD_ABI);
        }
        self.entries.get_mut(source).ok_or(NOT_FOUND)?.phase = Phase::Ready;
        Ok(())
    }

    /// An adapter lost evidence of the lower completion. Retain the binding;
    /// an uncertain read must never make its backing mark reusable.
    pub fn quarantine_read(&mut self, owner: u64, source: u32, ticket: u32) -> Result<(), Status>
    {
        if self.owned(owner, source)?.phase != Phase::Reading(ticket)
        {
            return Err(BAD_ABI);
        }
        self.entries.get_mut(source).ok_or(NOT_FOUND)?.phase = Phase::Quarantined;
        Ok(())
    }

    /// Return an owned token, retaining its source identity until Close ACK.
    pub fn begin_close(&mut self, owner: u64, source: u32) -> Result<B, Status>
    {
        let entry = self.owned(owner, source)?;
        Self::accessible(entry)?;
        if entry.phase != Phase::Ready
        {
            return Err(SERVICE_BUSY);
        }
        let backing = entry.backing.clone();
        self.entries.get_mut(source).ok_or(NOT_FOUND)?.phase = Phase::Closing;
        Ok(backing)
    }

    /// Failure to establish teardown leaves a pinned, explicit failure state.
    pub fn quarantine(&mut self, owner: u64, source: u32) -> Result<(), Status>
    {
        if self.owned(owner, source)?.phase != Phase::Closing
        {
            return Err(BAD_ABI);
        }
        self.entries.get_mut(source).ok_or(NOT_FOUND)?.phase = Phase::Quarantined;
        Ok(())
    }

    /// A later real Close/reset ACK may settle a quarantined record. Never use
    /// this as timeout cleanup or evidence that a remote operation did not run.
    pub fn finish_close(&mut self, owner: u64, source: u32) -> Result<(), Status>
    {
        if !matches!(self.owned(owner, source)?.phase, Phase::Closing | Phase::Quarantined)
        {
            return Err(BAD_ABI);
        }
        self.entries.remove(source).ok_or(NOT_FOUND)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests
{
    use super::*;
    const FILE: Identity = Identity { volume: 66513,
                                      cnid:   42,
                                      fork:   0 };
    #[test]
    fn uncertain_read_acknowledgement_quarantines_the_source_identity()
    {
        let mut sources = Sources::new();
        let source = sources.publish(7, FILE, 11, 20).unwrap();
        sources.begin_read(7, source, 88, 0, 10).unwrap();
        assert_eq!(sources.quarantine_read(7, source, 89), Err(BAD_ABI));
        sources.quarantine_read(7, source, 88).unwrap();
        assert_eq!(sources.info(7, source), Err(ROLLBACK_FAILED));
        assert_eq!(sources.find(7, FILE), Err(ROLLBACK_FAILED));
        assert_eq!(sources.begin_close(7, source), Err(ROLLBACK_FAILED));
    }
    #[test]
    fn reads_and_teardown_retain_generations_until_acknowledged()
    {
        let mut sources = Sources::new();
        let source = sources.publish(77, FILE, 123, 70_000).unwrap();
        assert_eq!(sources.find(77, FILE), Ok(Some(source)));
        assert_eq!(sources.publish(77, FILE, 124, 0), Err(ALREADY_EXISTS));
        assert_eq!(sources.info(78, source), Err(ACCESS_DENIED));
        assert_eq!(sources.begin_read(77, source, 1, 69_999, 8),
                   Ok(Read { backing: 123,
                             offset:  69_999,
                             length:  1 }));
        assert_eq!(sources.info(77, source), Ok(Info { length: 70_000 }));
        assert_eq!(sources.begin_close(77, source), Err(SERVICE_BUSY));
        assert_eq!(sources.begin_read(77, source, 2, 0, 1), Err(SERVICE_BUSY));
        assert_eq!(sources.finish_read(77, source, 2), Err(BAD_ABI));
        sources.finish_read(77, source, 1).unwrap();
        assert_eq!(sources.begin_read(77, source, 2, 70_001, 0), Err(BAD_ARGUMENT));
        assert_eq!(sources.begin_close(77, source), Ok(123));
        assert_eq!(sources.info(77, source), Err(SERVICE_BUSY));
        sources.quarantine(77, source).unwrap();
        assert_eq!(sources.find(77, FILE), Err(ROLLBACK_FAILED));
        assert_eq!(sources.publish(77, FILE, 125, 0), Err(ROLLBACK_FAILED));
        sources.finish_close(77, source).unwrap();
        let replacement = sources.publish(77, FILE, 126, 0).unwrap();
        assert_ne!(replacement, source);
        assert_eq!(replacement & 0xffff, source & 0xffff);
        assert_eq!(sources.info(77, source), Err(NOT_FOUND));
        assert_eq!(sources.finish_close(77, source), Err(NOT_FOUND));
        assert_eq!(sources.info(77, replacement), Ok(Info { length: 0 }));
    }
    #[test]
    fn foreign_tables_and_copied_files_cannot_alias_live_or_disposed_sources()
    {
        let mut a = Sources::new();
        let mut b = Sources::new();
        let one = a.publish(77, FILE, 123, 1).unwrap();
        let other = b.publish(77, FILE, 124, 1).unwrap();
        assert_ne!(one, other);
        assert_eq!(a.info(77, other), Err(NOT_FOUND));
        assert_eq!(b.info(77, one), Err(NOT_FOUND));
        let copied = a.publish(77,
                               Identity { cnid: 43,
                                          ..FILE },
                               125,
                               1)
                      .unwrap();
        assert_ne!(copied, one);
        assert_eq!(a.find(77,
                          Identity { volume: FILE.volume + 65536,
                                     ..FILE }),
                   Ok(None));
        a.begin_close(77, one).unwrap();
        a.finish_close(77, one).unwrap();
        drop(a);
        let mut rebuilt = Sources::new();
        let new = rebuilt.publish(77, FILE, 126, 1).unwrap();
        assert_ne!(new, one);
        assert_eq!(rebuilt.info(77, one), Err(NOT_FOUND));
    }
}

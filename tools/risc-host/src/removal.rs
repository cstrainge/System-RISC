//! Owned removal receipts. Quarantine is reversible; commit is the explicit
//! irreversible barrier. Retry retains the exact physical object and phase.
use super::*;
mod journal;
use journal::Journal;

pub(super) struct Removal
{
    pub owner:  u32,
    parent:     Directory,
    original:   String,
    hidden:     String,
    pin:        File,
    revision:   [u8; 32],
    journal:    Journal,
    directory:  Option<Dir>,
    record:     Option<(String, File)>,
    detached:   bool,
    verified:   bool,
    committing: bool,
    removed:    bool,
    restored:   bool,
    closed:     bool
}
fn approved_empty(directory: Option<&Dir>, record: Option<&(String, File)>) -> Result<()>
{
    let Some(directory) = directory
    else
    {
        return Ok(());
    };
    // Never traverse a supposedly empty directory or silently erase host
    // additions. Only the explicitly approved, pinned bookkeeping record
    // may be removed with the directory.
    let mut entries = directory.entries()?;
    if let Some(entry) = entries.next()
    {
        let entry = entry?;
        let Some((name, pin)) = record
        else
        {
            return Err(Failure(status::NOT_EMPTY));
        };
        if entry.file_name() != std::ffi::OsStr::new(name)
           || !entry.file_type()?.is_file()
           || entries.next().transpose()?.is_some()
        {
            return Err(Failure(status::NOT_EMPTY));
        }
        let actual = regular(directory, name)?;
        if !same_object(&actual.metadata()?, &pin.metadata()?)?
        {
            return Err(Failure(status::CONFLICT));
        }
    }
    Ok(())
}
impl Removal
{
    pub(super) fn disconnect(&mut self)
    {
        if !self.closed
        {
            if self.abort().is_ok()
            {
                let _ = self.journal.cleanup(&self.parent.root);
            }
            // The durable journal, not a second implicit Drop attempt, owns
            // any cleanup that could not be acknowledged during disconnect.
            self.closed = true;
        }
    }

    pub(super) fn private_name(&self, name: &str) -> bool
    {
        name == self.hidden || name == self.journal.name
    }

    fn same_at(&self, name: &str) -> Result<()>
    {
        let metadata = self.parent.root.symlink_metadata(name)?;
        if metadata.file_type().is_symlink()
        {
            return Err(Failure(status::CONFLICT));
        }
        let current = self.parent.root.open(name)?.into_std().metadata()?;
        if !same_object(&current, &self.pin.metadata()?)?
        {
            return Err(Failure(status::CONFLICT));
        }
        Ok(())
    }

    fn check_empty(&self) -> Result<()>
    {
        approved_empty(self.directory.as_ref(), self.record.as_ref())
    }

    fn quarantine(&mut self) -> Result<()>
    {
        if self.restored || self.committing
        {
            return Err(Failure(status::INVALID));
        }
        if !self.detached
        {
            self.same_at(&self.original)?;
            if fingerprint(&self.pin.metadata()?)? != self.revision
            {
                return Err(Failure(status::CONFLICT));
            }
            self.check_empty()?;
            move_noreplace(&self.parent.root, &self.original, &self.parent.root, &self.hidden)?;
            self.detached = true;
        }
        if !self.verified
        {
            self.same_at(&self.hidden)?;
            self.revision = fingerprint(&self.pin.metadata()?)?;
            self.verified = true;
        }
        sync(&self.parent.root)?;
        self.journal.phase(wire::REMOVE_QUARANTINED)
    }

    fn commit(&mut self) -> Result<()>
    {
        if self.restored || !self.detached || !self.verified
        {
            return Err(Failure(status::INVALID));
        }
        // Preflight remains reversible, including unexpected additions.
        if !self.committing
        {
            self.same_at(&self.hidden)?;
            if fingerprint(&self.pin.metadata()?)? != self.revision
            {
                return Err(Failure(status::CONFLICT));
            }
            self.check_empty()?;
            self.journal.phase(wire::REMOVE_COMMITTING)?;
            self.committing = true;
        }
        if !self.removed
        {
            self.same_at(&self.hidden)?;
            if let Some(directory) = &self.directory
            {
                self.check_empty()?;
                if let Some((name, pin)) = &self.record
                {
                    match directory.symlink_metadata(name)
                    {
                        Ok(_) =>
                        {
                            let current = regular(directory, name)?;
                            if !same_object(&current.metadata()?, &pin.metadata()?)?
                            {
                                return Err(Failure(status::CONFLICT));
                            }
                            directory.remove_file(name)?;
                        }
                        Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                        Err(e) => return Err(e.into())
                    }
                    sync(directory)?;
                }
                self.parent.root.remove_dir(&self.hidden)?;
            }
            else
            {
                self.parent.root.remove_file(&self.hidden)?;
            }
            self.removed = true;
        }
        sync(&self.parent.root)
    }

    fn abort(&mut self) -> Result<bool>
    {
        if self.committing
        {
            self.commit()?;
            return Ok(true);
        }
        if self.detached && !self.restored
        {
            self.same_at(&self.hidden)?;
            move_noreplace(&self.parent.root, &self.hidden, &self.parent.root, &self.original)?;
            self.restored = true;
        }
        sync(&self.parent.root)?;
        self.journal.phase(wire::REMOVE_ABORTED)?;
        Ok(false)
    }

    fn state(&self) -> u32
    {
        if self.removed
        {
            wire::REMOVE_REMOVED
        }
        else if self.committing
        {
            wire::REMOVE_COMMITTING
        }
        else if self.restored
        {
            wire::REMOVE_ABORTED
        }
        else if self.detached
        {
            wire::REMOVE_QUARANTINED
        }
        else
        {
            wire::REMOVE_PREPARED
        }
    }
}
impl Drop for Removal
{
    fn drop(&mut self)
    {
        // Connection loss must not strand a merely prepared/quarantined source.
        // Explicit Close uses checked abort and retains the receipt on failure.
        if !self.closed && self.abort().is_ok()
        {
            let _ = self.journal.cleanup(&self.parent.root);
        }
    }
}
impl Session
{
    pub(super) fn begin_remove(&mut self, parent: u32, expected: u32, payload: &[u8])
                               -> Result<u32>
    {
        let parent = self.directory(parent)?;
        self.writable(parent.export)?;
        let (name, record) = wire::remove_names(payload)?;
        let is_directory = self.check_source(&parent, name, expected)?;
        let directory =
            if is_directory { Some(self.directory(expected)?.root.try_clone()?) } else { None };
        let pin = if let Some(directory) = &directory
        {
            directory.open(".")?.into_std()
        }
        else
        {
            self.snapshot(expected, parent.export)?.file.try_clone()?
        };
        let record = match (directory.as_ref(), record)
        {
            (Some(directory), Some(record)) => Some((record.into(), regular(directory, record)?)),
            (None, Some(_)) => return Err(Failure(status::INVALID)),
            (_, None) => None
        };
        approved_empty(directory.as_ref(), record.as_ref())?;
        let revision = fingerprint(&pin.metadata()?)?;
        let hidden = unique()?;
        let journal = Journal::create(&parent.root,
                                      name,
                                      &hidden,
                                      &pin,
                                      record.as_ref().map(|(name, file)| (name.as_str(), file)))?;
        let receipt = Removal { owner: self.id,
                                revision,
                                journal,
                                parent,
                                original: name.into(),
                                hidden,
                                pin,
                                directory,
                                record,
                                detached: false,
                                verified: false,
                                committing: false,
                                removed: false,
                                restored: false,
                                closed: false };
        self.removals.insert(receipt).map_err(Into::into)
    }

    pub(super) fn remove_receipt(&mut self, id: u32) -> Result<&mut Removal>
    {
        self.removals.get_mut(id).filter(|r| r.owner == self.id).ok_or(Failure(status::STALE))
    }

    pub(super) fn quarantine_remove(&mut self, id: u32) -> Result<()>
    {
        self.remove_receipt(id)?.quarantine()
    }

    pub(super) fn commit_remove(&mut self, id: u32) -> Result<()>
    {
        self.remove_receipt(id)?.commit()
    }

    pub(super) fn abort_remove(&mut self, id: u32) -> Result<bool>
    {
        self.remove_receipt(id)?.abort()
    }

    pub(super) fn removal_state(&mut self, id: u32) -> Result<u32>
    {
        Ok(self.remove_receipt(id)?.state())
    }

    pub(super) fn close_removal(&mut self, id: u32) -> Result<()>
    {
        self.abort_remove(id)?;
        let receipt = self.remove_receipt(id)?;
        receipt.journal.cleanup(&receipt.parent.root)?;
        receipt.closed = true;
        self.removals.remove(id);
        Ok(())
    }
}

/// Recover interrupted removals only when private leftovers are encountered.
/// Ordinary enumeration does no extra directory scan. Active intent locks are
/// refused, never recovered out from under another live connection.
pub fn recover_removals_in(directory: &Dir) -> Result<bool>
{
    let mut recovered = false;
    let mut count = 0;
    for entry in directory.entries()?
    {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str()
        else
        {
            continue;
        };
        if !name.starts_with(journal::PREFIX)
        {
            continue;
        }
        if count == 64
        {
            return Err(Failure(status::LIMIT));
        }
        count += 1;
        recover_one(directory, name)?;
        recovered = true;
    }
    Ok(recovered)
}
fn recover_one(parent: &Dir, name: &str) -> Result<()>
{
    let journal = Journal::open(parent, name)?;
    let intent = journal.intent()?;
    let committed = intent.phase == wire::REMOVE_COMMITTING;
    let hidden = intent.hidden.to_owned();
    let original = intent.original.to_owned();
    let record_name = intent.record_name.map(str::to_owned);
    let record_identity = intent.record;
    let expected = intent.source;
    let pin = match parent.open(&hidden)
    {
        Ok(pin) => pin.into_std(),
        Err(e) if e.kind() == io::ErrorKind::NotFound =>
        {
            // Never repeat deletion against the original visible path. If
            // quarantine had not committed, a restore may have already won.
            if intent.phase == wire::REMOVE_QUARANTINED
            {
                let source = regular_or_directory(parent, &original)?;
                if object_identity(&source.metadata()?)? != Some(expected)
                {
                    return Err(Failure(status::CONFLICT));
                }
            }
            sync(parent)?;
            return journal.cleanup(parent);
        }
        Err(e) => return Err(e.into())
    };
    if parent.symlink_metadata(&hidden)?.file_type().is_symlink()
       || object_identity(&pin.metadata()?)? != Some(expected)
       || intent.phase == wire::REMOVE_ABORTED
    {
        return Err(Failure(status::CONFLICT));
    }
    let directory = if pin.metadata()?.is_dir() { Some(parent.open_dir(&hidden)?) } else { None };
    let record = match (directory.as_ref(), record_name)
    {
        (Some(directory), Some(name)) =>
        {
            match regular(directory, &name)
            {
                Ok(file) =>
                {
                    if object_identity(&file.metadata()?)? != record_identity
                    {
                        return Err(Failure(status::CONFLICT));
                    }
                    Some((name, file))
                }
                Err(Failure(status::NOT_FOUND)) if committed => None,
                Err(error) => return Err(error)
            }
        }
        (None, Some(_)) => return Err(Failure(status::CORRUPT)),
        (_, None) => None
    };
    let revision = fingerprint(&pin.metadata()?)?;
    let mut receipt = Removal { owner: 0,
                                parent: Directory::new(Arc::new(parent.try_clone()?), 0, 0)?,
                                original,
                                hidden,
                                pin,
                                revision,
                                journal,
                                directory,
                                record,
                                detached: true,
                                verified: true,
                                committing: committed,
                                removed: false,
                                restored: false,
                                closed: false };
    let result = receipt.abort().and_then(|_| receipt.journal.cleanup(parent));
    // A failed recovery remains in the journal for a later explicit retry;
    // Drop must not independently perform another recovery after reporting it.
    receipt.closed = true;
    result
}
fn regular_or_directory(parent: &Dir, name: &str) -> Result<File>
{
    let metadata = parent.symlink_metadata(name)?;
    if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir())
    {
        return Err(Failure(status::CONFLICT));
    }
    Ok(parent.open(name)?.into_std())
}

#[cfg(test)]
mod tests
{
    use super::*;
    struct Temp(std::path::PathBuf);
    impl Temp
    {
        fn new() -> Self
        {
            let name = unique().unwrap();
            let path = std::env::temp_dir().join(format!("risc-host-recovery-{}", name));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn directory(&self) -> Dir
        {
            Dir::open_ambient_dir(&self.0, cap_std::ambient_authority()).unwrap()
        }
    }
    impl Drop for Temp
    {
        fn drop(&mut self)
        {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn prepared(temp: &Temp) -> Removal
    {
        let root = temp.directory();
        std::fs::write(temp.0.join("Document"), b"never lose these bytes").unwrap();
        let pin = regular(&root, "Document").unwrap();
        let revision = fingerprint(&pin.metadata().unwrap()).unwrap();
        let hidden = unique().unwrap();
        let journal = Journal::create(&root, "Document", &hidden, &pin, None).unwrap();
        Removal { owner: 0,
                  parent: Directory::new(Arc::new(root), 0, 0).unwrap(),
                  original: "Document".into(),
                  hidden,
                  pin,
                  revision,
                  journal,
                  directory: None,
                  record: None,
                  detached: false,
                  verified: false,
                  committing: false,
                  removed: false,
                  restored: false,
                  closed: false }
    }
    fn lost_connection(mut receipt: Removal)
    {
        // Release all descriptors without running the disconnect cleanup, as a
        // killed companion would. Disk bytes remain the exact captured phase.
        receipt.closed = true;
    }
    #[test]
    fn recovery_restores_precommit_sources_and_finishes_only_committed_removals()
    {
        for phase in 0..4
        {
            let temp = Temp::new();
            let mut receipt = prepared(&temp);
            if phase >= 1
            {
                receipt.quarantine().unwrap();
            }
            if phase == 2
            {
                receipt.journal.phase(wire::REMOVE_COMMITTING).unwrap();
                receipt.committing = true;
            }
            if phase == 3
            {
                receipt.commit().unwrap();
            }
            let hidden = receipt.hidden.clone();
            lost_connection(receipt);
            assert!(recover_removals_in(&temp.directory()).unwrap());
            assert!(!temp.0.join(hidden).exists());
            if phase < 2
            {
                assert_eq!(std::fs::read(temp.0.join("Document")).unwrap(),
                           b"never lose these bytes");
            }
            else
            {
                assert!(!temp.0.join("Document").exists());
            }
            assert!(!recover_removals_in(&temp.directory()).unwrap());
            assert_eq!(std::fs::read_dir(&temp.0).unwrap().count(), usize::from(phase < 2));
        }
    }
    #[test]
    fn torn_phase_write_uses_the_previous_complete_slot_without_losing_the_source()
    {
        let temp = Temp::new();
        let mut receipt = prepared(&temp);
        receipt.quarantine().unwrap();
        let name = receipt.journal.name.clone();
        lost_connection(receipt);
        let mut journal = std::fs::OpenOptions::new().write(true).open(temp.0.join(name)).unwrap();
        journal.seek(SeekFrom::Start(1024)).unwrap();
        journal.write_all(b"torn slot").unwrap();
        journal.sync_all().unwrap();
        drop(journal);
        assert!(recover_removals_in(&temp.directory()).unwrap());
        assert_eq!(std::fs::read(temp.0.join("Document")).unwrap(), b"never lose these bytes");
    }
    #[test]
    fn active_journals_and_name_collisions_are_refused_without_erasing_either_object()
    {
        let temp = Temp::new();
        let mut receipt = prepared(&temp);
        assert_eq!(recover_removals_in(&temp.directory()).unwrap_err().0, status::BUSY);
        receipt.quarantine().unwrap();
        let hidden = receipt.hidden.clone();
        lost_connection(receipt);
        std::fs::write(temp.0.join("Document"), b"a foreign replacement").unwrap();
        assert_eq!(recover_removals_in(&temp.directory()).unwrap_err().0, status::CONFLICT);
        assert_eq!(std::fs::read(temp.0.join("Document")).unwrap(), b"a foreign replacement");
        assert_eq!(std::fs::read(temp.0.join(hidden)).unwrap(), b"never lose these bytes");
        std::fs::remove_file(temp.0.join("Document")).unwrap();
        assert!(recover_removals_in(&temp.directory()).unwrap());
        assert_eq!(std::fs::read(temp.0.join("Document")).unwrap(), b"never lose these bytes");
    }
}

//! Owned rename intent: publication and directory durability are separate.
//! A failed fsync never loses whether the directory entry has already moved.
use super::*;
pub(super) struct Rename
{
    pub owner: u32,
    from:      Directory,
    to:        Directory,
    old:       String,
    new:       String,
    expected:  u32,
    published: bool,
    verified:  bool
}
impl Session
{
    pub(super) fn begin_rename(&mut self, from: u32, to: u32, expected: u32, payload: &[u8])
                               -> Result<u32>
    {
        let from = self.directory(from)?;
        let to = self.directory(to)?;
        if from.export != to.export
        {
            return Err(Failure(status::INVALID));
        }
        self.writable(from.export)?;
        let (old, new) = wire::rename_names(payload)?;
        self.check_source(&from, old, expected)?;
        if to.root.symlink_metadata(new).is_ok()
        {
            return Err(Failure(status::CONFLICT));
        }
        self.renames
            .insert(Rename { owner: self.id,
                             from,
                             to,
                             old: old.into(),
                             new: new.into(),
                             expected,
                             published: false,
                             verified: false })
            .map_err(Into::into)
    }

    pub(super) fn rename_state(&self, id: u32) -> Result<bool>
    {
        let r =
            self.renames.get(id).filter(|r| r.owner == self.id).ok_or(Failure(status::STALE))?;
        // An object substituted by a racing host writer is not a successful
        // move of the expected source. Leave the intent owned for recovery.
        if r.published && !r.verified
        {
            return Err(Failure(status::CONFLICT));
        }
        Ok(r.published)
    }

    pub(super) fn finish_rename(&mut self, id: u32) -> Result<()>
    {
        let r =
            self.renames.get(id).filter(|r| r.owner == self.id).ok_or(Failure(status::STALE))?;
        if !r.published
        {
            self.check_source(&r.from, &r.old, r.expected)?;
            move_noreplace(&r.from.root, &r.old, &r.to.root, &r.new)?;
            // No fallible work or allocation between rename and this marker.
            self.renames.get_mut(id).unwrap().published = true;
        }
        let r = self.renames.get(id).unwrap();
        if !r.verified
        {
            self.check_moved(&r.to, &r.new, r.expected)?;
            self.renames.get_mut(id).unwrap().verified = true;
        }
        let r = self.renames.get(id).unwrap();
        sync(&r.from.root)?;
        if !Arc::ptr_eq(&r.from.root, &r.to.root)
        {
            sync(&r.to.root)?;
        }
        Ok(())
    }
}

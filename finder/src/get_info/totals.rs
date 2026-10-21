//! Bounded depth-first directory totals. A single service scans at most one leaf.
use mac_toolbox::{files::{DirectoryCursor, ScanStep},
                  *};
#[derive(Clone, Copy, Default)]
struct Level
{
    directory: Cnid,
    cursor:    DirectoryCursor
}
pub(super) struct Job
{
    volume:        VolumeId,
    root:          Cnid,
    levels:        [Level; 64],
    depth:         usize,
    restarts:      u8,
    pub complete:  bool,
    pub error:     Option<Error>,
    pub logical:   u64,
    pub allocated: u64,
    pub items:     u64
}
impl Job
{
    pub fn new(volume: VolumeId, directory: Cnid) -> Self
    {
        let mut levels = [Level::default(); 64];
        levels[0].directory = directory;
        Self { volume,
               root: directory,
               levels,
               depth: 1,
               restarts: 0,
               complete: false,
               error: None,
               logical: 0,
               allocated: 0,
               items: 0 }
    }

    pub fn step(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        let start = system.events().monotonic_nanoseconds()?;
        for _ in 0..4
        {
            let level = self.levels
                            .get_mut(self.depth.checked_sub(1).ok_or(Error::BadArgument)?)
                            .ok_or(Error::BadArgument)?;
            let result =
                system.files().scan_directory(self.volume, level.directory, &mut level.cursor);
            match result
            {
                Ok(ScanStep::Entry { catalog,
                                     data_allocated,
                                     resource_allocated }) =>
                {
                    self.items = self.items.checked_add(1).ok_or(Error::BadArgument)?;
                    self.logical = self.logical
                                       .checked_add(catalog.data_length)
                                       .and_then(|n| n.checked_add(catalog.resource_length))
                                       .ok_or(Error::BadArgument)?;
                    self.allocated = self.allocated
                                         .checked_add(data_allocated)
                                         .and_then(|n| n.checked_add(resource_allocated))
                                         .ok_or(Error::BadArgument)?;
                    if catalog.is_directory()
                    {
                        // Corrupt cycles/deep hierarchies fail explicitly, never
                        // complete with silently truncated size/count results.
                        if self.depth >= self.levels.len()
                           || self.levels[..self.depth].iter()
                                                       .any(|level| level.directory == catalog.id)
                        {
                            return Err(Error::BadArgument);
                        }
                        self.levels[self.depth] = Level { directory: catalog.id,
                                                          cursor:    DirectoryCursor::default() };
                        self.depth += 1;
                    }
                }
                Ok(ScanStep::Pending) => break,
                Ok(ScanStep::Done) =>
                {
                    self.depth -= 1;
                    if self.depth == 0
                    {
                        self.complete = true;
                        break;
                    }
                }
                Err(Error::ScanStale) =>
                {
                    let restarts = self.restarts.checked_add(1).ok_or(Error::ServiceBusy)?;
                    if restarts > 4
                    {
                        return Err(Error::ServiceBusy);
                    }
                    *self = Self::new(self.volume, self.root);
                    self.restarts = restarts;
                    break;
                }
                Err(error) => return Err(error)
            }
            if system.events().monotonic_nanoseconds()?.saturating_sub(start) >= 300_000
            {
                break;
            }
        }
        Ok(())
    }
}

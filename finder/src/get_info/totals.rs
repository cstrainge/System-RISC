//! Bounded depth-first directory totals. A single service scans at most one leaf.
use mac_toolbox::{files::{DirectoryCursor, SCAN_STALE, ScanStep},
                  *};
#[derive(Clone, Copy, Default)]
struct Level
{
    directory: u32,
    cursor:    DirectoryCursor
}
pub(super) struct Job
{
    volume:        u32,
    root:          u32,
    levels:        [Level; 64],
    depth:         usize,
    restarts:      u8,
    pub complete:  bool,
    pub error:     Option<Status>,
    pub logical:   u64,
    pub allocated: u64,
    pub items:     u64
}
impl Job
{
    pub fn new(volume: u32, directory: u32) -> Self
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

    pub fn step(&mut self, system: &System<'_>) -> Result<(), Status>
    {
        let start = system.events().monotonic_nanoseconds().map_err(|e| e.0)?;
        for _ in 0..4
        {
            let level = self.levels
                            .get_mut(self.depth.checked_sub(1).ok_or(BAD_ARGUMENT)?)
                            .ok_or(BAD_ARGUMENT)?;
            let result =
                system.files().scan_directory(self.volume, level.directory, &mut level.cursor);
            match result
            {
                Ok(ScanStep::Entry { catalog,
                                     data_allocated,
                                     resource_allocated }) =>
                {
                    self.items = self.items.checked_add(1).ok_or(BAD_ARGUMENT)?;
                    self.logical = self.logical
                                       .checked_add(catalog.data_length)
                                       .and_then(|n| n.checked_add(catalog.resource_length))
                                       .ok_or(BAD_ARGUMENT)?;
                    self.allocated = self.allocated
                                         .checked_add(data_allocated)
                                         .and_then(|n| n.checked_add(resource_allocated))
                                         .ok_or(BAD_ARGUMENT)?;
                    if catalog.is_directory()
                    {
                        // Corrupt cycles/deep hierarchies fail explicitly, never
                        // complete with silently truncated size/count results.
                        if self.depth >= self.levels.len()
                           || self.levels[..self.depth].iter()
                                                       .any(|level| level.directory == catalog.id)
                        {
                            return Err(BAD_ARGUMENT);
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
                Err(Error(SCAN_STALE)) =>
                {
                    let restarts = self.restarts.checked_add(1).ok_or(SERVICE_BUSY)?;
                    if restarts > 4
                    {
                        return Err(SERVICE_BUSY);
                    }
                    *self = Self::new(self.volume, self.root);
                    self.restarts = restarts;
                    break;
                }
                Err(error) => return Err(error.0)
            }
            if system.events().monotonic_nanoseconds().map_err(|e| e.0)?.saturating_sub(start)
               >= 300_000
            {
                break;
            }
        }
        Ok(())
    }
}

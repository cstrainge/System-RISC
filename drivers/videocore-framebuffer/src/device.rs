//! The device side of the VIDO contract for a firmware-owned linear
//! framebuffer.
//!
//! Three things matter here and they are not interchangeable: the firmware's
//! scan-out, whose address this driver cannot choose; the Device DMA storage
//! System draws into; and the mode each belongs to. The pitch travels inside the
//! mode record, never in a field of its own, so a mode change cannot leave a
//! stale stride behind.
use mac_video_interface::{Attach, Damage, Outcome, Prepared, Size, Surface};

use crate::{Refusal,
            scanout::{ALLOCATION_ALIGNMENT, Allocation, Bounds, Frame, Platform,
                      plan as copy_plan},
            surface_bytes};

/// The VIDO operations the engine drives. Semantics follow the virtio driver:
/// ATTACH allocates and scans out, PREPARE stages without publishing, COMMIT
/// switches, and RELEASE follows System's CPU-pin retirement — a refusal retains
/// custody of whatever it refused to release.
pub trait Hardware
{
    fn attach(&mut self, attach: &Attach, generation: u64) -> Result<Surface, Refusal>;
    /// `Some` when the host reported a display change. The firmware never does;
    /// this driver owns the mode.
    fn host_size(&mut self) -> Result<Option<Size>, Refusal>;
    fn present(&mut self, damage: Damage) -> Result<(), Refusal>;
    fn prepare(&mut self, size: Size, generation: u64) -> Result<Prepared, Refusal>;
    fn commit(&mut self, allocation: u64, generation: u64) -> Result<Outcome, Refusal>;
    fn abort(&mut self, allocation: u64, generation: u64) -> Result<(), Refusal>;
    fn release(&mut self, allocation: u64, generation: u64) -> Result<(), Refusal>;
    fn pending(&self) -> bool;
    fn cancel(&mut self, generation: u64) -> Result<(), Refusal>;
    fn microseconds(&mut self) -> Result<u64, Refusal>;
}

/// One mode's pair of buffers: the firmware's scan-out and the surface System
/// draws into, both belonging to the same geometry and pitch.
#[derive(Clone, Copy, Debug)]
struct Record
{
    generation: u64,
    allocation: Allocation,
    frame:      Frame
}
impl Record
{
    const fn geometry(self) -> Size
    {
        self.frame.geometry
    }

    const fn publish(self) -> Surface
    {
        Surface { generation: self.generation,
                  allocation: self.allocation.handle,
                  address:    self.allocation.address,
                  capacity:   self.allocation.length,
                  // The firmware offers no preferred size; System's own record
                  // supplies the mode and this driver never learns another.
                  preferred:  Size { width:  0,
                                     height: 0 } }
    }
}

pub struct Device<P: Platform>
{
    platform:      P,
    bounds:        Bounds,
    active:        Option<Record>,
    candidate:     Option<Record>,
    retired:       Option<Record>,
    last_released: Option<(u64, u64)>,
    last_aborted:  Option<(u64, u64)>
}
impl<P: Platform> Device<P>
{
    pub const fn new(platform: P, bounds: Bounds) -> Self
    {
        Self { platform,
               bounds,
               active: None,
               candidate: None,
               retired: None,
               last_released: None,
               last_aborted: None }
    }

    /// Apply `geometry` and take the surface storage for it.
    ///
    /// `fallback` is the geometry the firmware must return to when the
    /// allocation fails after the mode was already applied, because the property
    /// channel has no staged mode. It is `None` only when nothing was displayed
    /// yet.
    fn open(&mut self, geometry: Size, generation: u64, fallback: Option<Size>)
            -> Result<Record, Refusal>
    {
        let frame = self.platform.acquire(geometry, ALLOCATION_ALIGNMENT)?;
        let needed = surface_bytes(&frame);
        let allocation = match self.platform.allocate(needed)
        {
            Ok(allocation) => allocation,
            Err(refusal) =>
            {
                return Err(self.roll_back(fallback, refusal));
            }
        };
        if allocation.length < needed
        {
            let refusal = Refusal::AllocationTooSmall { available: allocation.length,
                                                        needed };
            let freed = self.platform.free(allocation);
            return Err(match freed
            {
                Ok(()) => self.roll_back(fallback, refusal),
                Err(free) => free
            });
        }
        Ok(Record { generation,
                    allocation,
                    frame })
    }

    /// Restore `fallback` after a refusal and report the refusal. A restore that
    /// also fails becomes `RestoreFailed` naming both causes, because a screen
    /// left in a mode nobody committed is worse than a refused mode change.
    fn roll_back(&mut self, fallback: Option<Size>, cause: Refusal) -> Refusal
    {
        let Some(geometry) = fallback
        else
        {
            return cause;
        };
        match self.platform.acquire(geometry, ALLOCATION_ALIGNMENT)
        {
            Ok(_) => cause,
            Err(restore) =>
            {
                Refusal::RestoreFailed { underlying: match restore
                                         {
                                             Refusal::Device(error) => error,
                                             _ => mac_videocore_fb::Error::EmptyAllocation
                                         } }
            }
        }
    }

    /// Give the firmware back the active mode. Returns the refusal that stopped
    /// it, if any, so the caller can keep custody of the candidate.
    fn restore_active(&mut self) -> Result<(), Refusal>
    {
        let Some(geometry) = self.active.map(Record::geometry)
        else
        {
            return Ok(());
        };
        self.platform.acquire(geometry, ALLOCATION_ALIGNMENT)?;
        Ok(())
    }
}
impl<P: Platform> Hardware for Device<P>
{
    fn attach(&mut self, attach: &Attach, generation: u64) -> Result<Surface, Refusal>
    {
        if self.active.is_some() || self.candidate.is_some()
        {
            return Err(Refusal::AlreadyAttached);
        }
        let geometry = Size { width:  attach.width,
                              height: attach.height };
        self.bounds.check(geometry)?;
        let record = self.open(geometry, generation, None)?;
        let published = record.publish();
        self.active = Some(record);
        Ok(published)
    }

    fn host_size(&mut self) -> Result<Option<Size>, Refusal>
    {
        // The firmware configures the display and never tells the ARM that a
        // mode changed: the property channel is request/response only. This
        // driver is the mode owner, so a "host size" here would be its own last
        // request echoed back, and reporting it would make System re-apply a
        // mode it already applied.
        Ok(None)
    }

    fn present(&mut self, damage: Damage) -> Result<(), Refusal>
    {
        if self.candidate.is_some()
        {
            // PREPARE has already asked the firmware for the new mode, so the
            // display no longer fetches with the old pitch. Copying now would
            // shear the image; System retries once the commit lands.
            return Err(Refusal::ModeChangePending);
        }
        let record = self.active.ok_or(Refusal::NotAttached)?;
        let Some(plan) = copy_plan(record.geometry(), record.frame.pitch, damage)
        else
        {
            return Ok(());
        };
        self.platform.blit(record.allocation, &record.frame, &plan)?;
        self.platform.clean(&record.frame, plan.destination_bytes, plan.clean_bytes);
        Ok(())
    }

    fn prepare(&mut self, size: Size, generation: u64) -> Result<Prepared, Refusal>
    {
        self.bounds.check(size)?;
        let previous = self.active.ok_or(Refusal::NotAttached)?.geometry();
        if self.candidate.is_some()
        {
            return Err(Refusal::PreparePending);
        }
        // Asking the firmware for the new geometry applies it now; everything
        // after this point either commits or puts `previous` back.
        let record = self.open(size, generation, Some(previous))?;
        let prepared = Prepared { outcome: Outcome::Applied,
                                  surface: Some(record.publish()) };
        self.candidate = Some(record);
        Ok(prepared)
    }

    fn commit(&mut self, allocation: u64, generation: u64) -> Result<Outcome, Refusal>
    {
        let candidate = self.candidate.ok_or(Refusal::UnknownAllocation { allocation })?;
        if candidate.allocation.handle != allocation
        {
            return Err(Refusal::UnknownAllocation { allocation });
        }
        if candidate.generation != generation
        {
            return Err(Refusal::StaleGeneration { allocation,
                                                  expected: candidate.generation,
                                                  observed: generation });
        }
        if self.retired.is_some()
        {
            // System releases the previous surface before asking for the next
            // mode; holding two would mean the old storage is still pinned.
            return Err(Refusal::PreparePending);
        }
        self.candidate = None;
        self.retired = self.active.replace(candidate);
        Ok(Outcome::Applied)
    }

    fn abort(&mut self, allocation: u64, generation: u64) -> Result<(), Refusal>
    {
        if self.last_aborted == Some((allocation, generation))
        {
            return Ok(());
        }
        let candidate = self.candidate.ok_or(Refusal::UnknownAllocation { allocation })?;
        if candidate.allocation.handle != allocation
        {
            return Err(Refusal::UnknownAllocation { allocation });
        }
        if candidate.generation != generation
        {
            return Err(Refusal::StaleGeneration { allocation,
                                                  expected: candidate.generation,
                                                  observed: generation });
        }
        // The firmware is already in the candidate's mode. Restore the active
        // mode first: freeing the candidate while the display fetched from it
        // would leave the screen on storage this module no longer owns. A failed
        // restore keeps the candidate, so the abort can be retried.
        self.restore_active()?;
        self.platform.free(candidate.allocation)?;
        self.candidate = None;
        self.last_aborted = Some((allocation, generation));
        Ok(())
    }

    fn release(&mut self, allocation: u64, generation: u64) -> Result<(), Refusal>
    {
        if self.last_released == Some((allocation, generation))
        {
            return Ok(());
        }
        let retired = self.retired.ok_or(Refusal::UnknownAllocation { allocation })?;
        if retired.allocation.handle != allocation
        {
            return Err(Refusal::UnknownAllocation { allocation });
        }
        if retired.generation != generation
        {
            return Err(Refusal::StaleGeneration { allocation,
                                                  expected: retired.generation,
                                                  observed: generation });
        }
        // System drops its pin before calling RELEASE. A refusal — the pin is
        // still live, or the service is busy — keeps the record so the same
        // release can be retried instead of leaking the storage.
        self.platform.free(retired.allocation)?;
        self.retired = None;
        self.last_released = Some((allocation, generation));
        Ok(())
    }

    fn pending(&self) -> bool
    {
        // Nothing here is asynchronous: the mailbox is request/response and the
        // DMA service completes or refuses within the call.
        false
    }

    fn cancel(&mut self, generation: u64) -> Result<(), Refusal>
    {
        let Some(candidate) = self.candidate
        else
        {
            return Ok(());
        };
        if candidate.generation != generation
        {
            // Another request's preparation is not this cancellation's to end.
            return Ok(());
        }
        self.abort(candidate.allocation.handle, generation)
    }

    fn microseconds(&mut self) -> Result<u64, Refusal>
    {
        self.platform.now()
    }
}
impl<P: Platform> Device<P>
{
    /// The machine behind this device, for the adapter's own diagnostics and for
    /// tests that need to see what the hardware was actually asked to do.
    pub const fn platform(&self) -> &P
    {
        &self.platform
    }

    pub const fn platform_mut(&mut self) -> &mut P
    {
        &mut self.platform
    }

    /// The geometry the firmware is currently scanning out.
    pub fn active_geometry(&self) -> Option<Size>
    {
        self.active.map(Record::geometry)
    }

    /// The pitch the current mode reports. A mode change must carry its own.
    pub fn active_pitch(&self) -> Option<u32>
    {
        self.active.map(|record| record.frame.pitch)
    }

    /// The mode currently being prepared, if any.
    pub fn candidate_geometry(&self) -> Option<Size>
    {
        self.candidate.map(Record::geometry)
    }

    /// The geometry System's pin still holds after a commit, if any.
    pub fn retired_geometry(&self) -> Option<Size>
    {
        self.retired.map(Record::geometry)
    }

    /// The rows a present of `damage` would copy.
    pub fn present_plan(&self, damage: Damage) -> Option<crate::Plan>
    {
        let record = self.active?;
        copy_plan(record.geometry(), record.frame.pitch, damage)
    }

    /// Whether this driver still holds `allocation`, and at which geometry.
    pub fn holds(&self, allocation: u64) -> Option<Size>
    {
        [self.active, self.candidate, self.retired].into_iter()
                                                   .flatten()
                                                   .find(|record| {
                                                       record.allocation.handle == allocation
                                                   })
                                                   .map(Record::geometry)
    }

    /// The allocation handle held for `geometry`, if this driver holds one.
    pub fn holds_geometry(&self, geometry: Size) -> Option<u64>
    {
        [self.active, self.candidate, self.retired].into_iter()
                                                   .flatten()
                                                   .find(|record| record.geometry() == geometry)
                                                   .map(|record| record.allocation.handle)
    }
}

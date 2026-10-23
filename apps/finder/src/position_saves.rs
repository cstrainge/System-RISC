//! Persist automatic layout as one owned, stepped transaction per folder.
//! No pointer handler enumerates or writes storage; pixels already have positions.
#![forbid(unsafe_code)]
use super::*;
#[derive(Clone, Copy, Default)]
struct SavedPoint
{
    id:    Cnid,
    point: Point,
    index: u16
}
pub(super) struct PositionSave<'a>
{
    group:    files::IconMoveGroup<'a>,
    slot:     usize,
    window:   WindowId,
    location: Location,
    points:   [SavedPoint; CAPACITY],
    count:    usize
}
impl<'a> PositionSave<'a>
{
    #[inline(never)]
    pub(super) fn begin(system: &System<'a>, browser: &Browser, slot: usize)
                        -> Result<Option<Self>, Error>
    {
        let window = browser.windows[slot].ok_or(Error::NotFound)?;
        if window.location.volume == VolumeId::NONE
        {
            return Ok(None);
        }
        let count = cache::with(slot, |folder| {
            if !folder.ready || folder.volume.is_none_or(|v| v.read_only)
            {
                0
            }
            else
            {
                folder.placements.iter().count()
            }
        });
        if count == 0
        {
            return Ok(None);
        }
        let mut rows = [files::IconMove::default(); CAPACITY];
        let mut points = [SavedPoint::default(); CAPACITY];
        cache::with(slot, |folder| {
            for (at, index) in folder.placements.iter().enumerate()
            {
                let item = folder.items[index];
                let point = Point { h: item.icon_bounds.left - 16,
                                    v: item.icon_bounds.top - 28 };
                rows[at] = files::IconMove { id:              item.entry.id,
                                             expected_parent: item.entry.parent,
                                             parent:          item.entry.parent,
                                             placement:       files::Placement::At(point),
                                             to_trash:        false };
                points[at] = SavedPoint { id: item.entry.id,
                                          point,
                                          index: index as u16 };
            }
        });
        let group = system.files().begin_icon_moves(window.location.volume, &rows[..count])?;
        Ok(Some(Self { group,
                       slot,
                       window: window.window,
                       location: window.location,
                       points,
                       count }))
    }

    pub(super) fn advance(&mut self, browser: &mut Browser) -> Result<bool, Error>
    {
        match self.group.step()?
        {
            files::MoveProgress::Applying { .. } | files::MoveProgress::Undoing { .. } =>
            {
                return Ok(false);
            }
            files::MoveProgress::Cancelled => return Err(Error::Cancelled),
            files::MoveProgress::Committed { .. } => ()
        }
        if browser.windows[self.slot].is_none_or(|w| {
                                         w.window != self.window || w.location != self.location
                                     })
        {
            return Ok(true);
        }
        cache::edit(self.slot, |folder| {
            if !folder.ready || folder.window != self.window || folder.location != self.location
            {
                return;
            }
            for saved in &self.points[..self.count]
            {
                let index = saved.index as usize;
                if index >= folder.count
                {
                    continue;
                }
                let item = &mut folder.items[index];
                if item.entry.id != saved.id
                   || item.entry.parent != self.location.folder
                   || item.icon_bounds.left != saved.point.h + 16
                   || item.icon_bounds.top != saved.point.v + 28
                {
                    continue;
                }
                let flags = item.entry.finder_flags().bits() | 0x100;
                item.entry.finder_info[8..10].copy_from_slice(&flags.to_be_bytes());
                item.entry.finder_info[10..12].copy_from_slice(&saved.point.v.to_be_bytes());
                item.entry.finder_info[12..14].copy_from_slice(&saved.point.h.to_be_bytes());
                folder.placements.set(index, false);
            }
        });
        browser.queue_position_save(self.slot);
        Ok(true)
    }
}

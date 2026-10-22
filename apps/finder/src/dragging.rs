//! Finder supplies cached items and target intent. System owns the gesture,
//! feedback, negotiation, modifier policy and terminal outcome.
use mac_toolbox::drag::{self, DragEvent, DragOutcome, DropAction, DropPayload, FileItem, Response,
                        Target, TrackPhase, TrackingOffer};

use super::*;
#[derive(Clone, Copy)]
struct Destination
{
    location:  Location,
    trash:     bool,
    frame:     Rect,
    scroll:    Point,
    highlight: Rect
}
#[derive(Default)]
pub struct Controller<'a>
{
    session: Option<drag::DragSession<'a>>,
    target:  Option<Destination>
}
fn status(error: drag::DragError) -> Error
{
    match error
    {
        drag::DragError::NoSession => Error::NotFound,
        drag::DragError::WrongThread => Error::WrongThread,
        drag::DragError::Unknown(s) => Error::from_status(s),
        _ => Error::BadArgument
    }
}
impl<'a> Controller<'a>
{
    pub fn begin(&mut self, system: &System<'a>, browser: &mut Browser) -> Result<(), Error>
    {
        if self.session.is_some()
        {
            return Ok(());
        }
        let Some(press) = browser.press
        else
        {
            return Ok(());
        };
        let Some(slot) =
            browser.windows.iter().position(|w| w.is_some_and(|w| w.window == browser.front))
        else
        {
            return Ok(());
        };
        let window = browser.windows[slot].unwrap();
        let empty = FileItem { volume: VolumeId::NONE,
                               id:     Cnid::UNSAVED,
                               parent: Cnid::UNSAVED,
                               origin: Point::default(),
                               bounds: Rect::ZERO };
        let mut items = [empty; CAPACITY];
        let mut count = 0;
        let mut outline = Rect::ZERO;
        cache::with(slot, |folder| {
            if !folder.ready
            {
                return;
            }
            for index in window.selected.iter()
            {
                if index >= folder.count
                {
                    continue;
                }
                let item = folder.items[index];
                let mut bounds = item.icon_bounds;
                bounds.offset(folder.frame.left - window.scroll_h,
                              folder.frame.top - window.scroll);
                let mut whole = item.bounds;
                whole.offset(folder.frame.left - window.scroll_h, folder.frame.top - window.scroll);
                outline = if count == 0
                {
                    whole
                }
                else
                {
                    Rect::new(outline.left.min(whole.left),
                              outline.top.min(whole.top),
                              outline.right.max(whole.right),
                              outline.bottom.max(whole.bottom))
                };
                items[count] = FileItem { volume: item.entry.volume,
                                          id: item.entry.id,
                                          parent: item.entry.parent,
                                          origin: Point { h: item.icon_bounds.left - 16,
                                                          v: item.icon_bounds.top - 28 },
                                          bounds };
                count += 1;
            }
        });
        if count == 0
        {
            return Ok(());
        }
        let mut builder =
            system.drags().builder(Some(window.window), press.anchor, outline).map_err(status)?;
        builder.items(items[..count].iter().copied()).map_err(status)?;
        self.session = Some(builder.begin().map_err(status)?);
        Ok(())
    }

    pub fn event(&mut self, system: &System<'a>, browser: &mut Browser, event: Event<'a>)
                 -> Result<bool, Error>
    {
        let Event::Drag { offer,
                          .. } = event
        else
        {
            return Ok(false);
        };
        match offer
        {
            DragEvent::Tracking(offer) =>
            {
                if offer.phase == TrackPhase::Left
                {
                    self.target = None;
                    return Ok(true);
                }
                let target = if offer.flavors().all_file_items()
                {
                    browser.drag_destination(offer)
                }
                else
                {
                    None
                };
                let response =
                    target.map_or(Response::Reject, |target| {
                        Response::Accept { target:    if target.trash
                                           {
                                               Target::Trash { volume: target.location.volume,
                                                               id:     target.location.folder }
                                           }
                                           else
                                           {
                                               Target::Folder { volume: target.location.volume,
                                                                id:     target.location.folder }
                                           },
                                           highlight: target.highlight }
                    });
                match offer.respond(response)
                {
                    Ok(()) => self.target = target,
                    Err(error) =>
                    {
                        self.target = None;
                        let _ = offer.respond(Response::Reject);
                        if status(error) != Error::ReadOnly
                        {
                            return Err(status(error));
                        }
                    }
                }
            }
            DragEvent::Dropped(drop) =>
            {
                let result =
                    self.target
                        .ok_or(Error::BadArgument)
                        .and_then(|target| browser.receive_drag(system, drop, target));
                system.events()
                      .resolve_drag(drop.receive(async { result.map_err(drag::DragError::from) }),
                                    |_| Ok(()))
                      .map_err(status)?;
            }
            DragEvent::Ended { outcome } =>
            {
                let was_click =
                    self.session.as_ref().is_some_and(|s| s.was_click().unwrap_or(false));
                self.session = None;
                self.target = None;
                if let Some(press) = browser.press.take()
                   && was_click
                   && let Some((slot, index)) = press.collapse
                {
                    browser.change_selection(system, slot, Selection::singleton(index), &[])?;
                }
                if matches!(outcome, DragOutcome::Moved | DragOutcome::Copied)
                {
                    browser.last_click = None;
                }
                system.log(match outcome
                           {
                               DragOutcome::Moved => "FINDER_DRAG_ENDED moved\n",
                               DragOutcome::Copied => "FINDER_DRAG_ENDED copied\n",
                               DragOutcome::Rejected => "FINDER_DRAG_ENDED rejected\n",
                               DragOutcome::Cancelled => "FINDER_DRAG_ENDED cancelled\n"
                           })?;
            }
            DragEvent::DataRequested =>
            {
                if let Some(session) = &mut self.session
                {
                    while session.service_promises().map_err(status)?
                    {}
                }
            }
            DragEvent::DataReady =>
            {}
        }
        Ok(true)
    }
}
impl Browser
{
    fn drag_destination(&self, offer: TrackingOffer<'_>) -> Option<Destination>
    {
        let at = offer.position();
        if let Some(window) = offer.window()
        {
            let slot = self.windows.iter().position(|w| w.is_some_and(|w| w.window == window))?;
            let window = self.windows[slot]?;
            return cache::with(slot, |folder| {
                if !folder.ready || folder.volume?.read_only
                {
                    return None;
                }
                let local = Point { h: at.h - folder.frame.left,
                                    v: at.v - folder.frame.top };
                if local.h < 0
                   || local.v < icon_view::INFO_HEIGHT
                   || local.h >= (folder.frame.right - folder.frame.left) - 15
                   || local.v >= (folder.frame.bottom - folder.frame.top) - 15
                {
                    return None;
                }
                let content = Point { h: local.h + window.scroll_h,
                                      v: local.v + window.scroll };
                let mut location = window.location;
                let mut highlight = Rect::new(folder.frame.left,
                                              folder.frame.top + icon_view::INFO_HEIGHT,
                                              folder.frame.right - 15,
                                              folder.frame.bottom - 15);
                let source_here = self.press.is_some_and(|press| {
                                                folder.items[..folder.count].iter()
                                                              .any(|item| {
                                                                  item.location() == press.entry
                                                              })
                                            });
                folder.intersecting(Rect::new(content.h, content.v, content.h + 1, content.v + 1),
                                    |index, item| {
                                        if item.entry.is_directory()
                                           && !(source_here && window.selected.contains(index))
                                        {
                                            location = item.location();
                                            highlight = item.bounds;
                                            highlight.offset(folder.frame.left - window.scroll_h,
                                                             folder.frame.top - window.scroll);
                                        }
                                    });
                let (frame, scroll) = if location == window.location
                {
                    (folder.frame,
                     Point { h: window.scroll_h,
                             v: window.scroll })
                }
                else
                {
                    (Rect::ZERO, Point::default())
                };
                Some(Destination { location,
                                   trash: false,
                                   frame,
                                   scroll,
                                   highlight })
            });
        }
        let index = desktop::hit(at)?;
        let item = desktop::item(index)?;
        if item.is_trash()
        {
            let volume = self.press.map_or(self.startup, |p| p.entry.volume);
            Some(Destination { location:  Location { volume,
                                                     folder: Cnid::UNSAVED },
                               trash:     true,
                               frame:     Rect::ZERO,
                               scroll:    Point::default(),
                               highlight: item.bounds })
        }
        else
        {
            Some(Destination { location:  item.location(),
                               trash:     false,
                               frame:     Rect::ZERO,
                               scroll:    Point::default(),
                               highlight: item.bounds })
        }
    }

    fn receive_drag(&mut self, system: &System<'_>, drop: DropPayload<'_>, mut target: Destination)
                    -> Result<(), Error>
    {
        if drop.action != DropAction::Move
        {
            return Err(Error::NotSupported);
        }
        if target.trash
        {
            target.location.folder =
                system.files().trash_directory(target.location.volume, true)?.id;
        }
        let flavors = drop.flavors();
        let count = flavors.item_count().map_err(status)?;
        if count > CAPACITY
        {
            return Err(Error::NoMemory);
        }
        let origin = drop.origin().map_err(status)?;
        let at = drop.position();
        let mut rows = [files::IconMove::default(); CAPACITY];
        for (index, row) in rows[..count].iter_mut().enumerate()
        {
            let item = flavors.item(index).map_err(status)?;
            let file = system.events().resolve_drag(drop.file(item), |_| Ok(())).map_err(status)?;
            if file.volume != target.location.volume
            {
                return Err(Error::NotSupported);
            }
            let point = Point { h: file.bounds
                                       .left
                                       .saturating_add(at.h - origin.h)
                                       .saturating_sub(target.frame.left)
                                       .saturating_add(target.scroll.h)
                                       .saturating_sub(16),
                                v: file.bounds
                                       .top
                                       .saturating_add(at.v - origin.v)
                                       .saturating_sub(target.frame.top)
                                       .saturating_add(target.scroll.v)
                                       .saturating_sub(28) };
            *row = files::IconMove { id:              file.id,
                                     expected_parent: file.parent,
                                     parent:          target.location.folder,
                                     placement:       if target.trash
                                     {
                                         files::Placement::Clear
                                     }
                                     else
                                     {
                                         files::Placement::At(point)
                                     },
                                     to_trash:        target.trash };
        }
        if !target.trash && target.frame == Rect::ZERO
        {
            // A closed folder/volume icon has no content coordinates. Start the
            // group at the new view's origin while preserving its relative layout.
            let mut left = i16::MAX;
            let mut top = i16::MAX;
            for row in &rows[..count]
            {
                if let files::Placement::At(p) = row.placement
                {
                    left = left.min(p.h);
                    top = top.min(p.v);
                }
            }
            for row in &mut rows[..count]
            {
                if let files::Placement::At(p) = &mut row.placement
                {
                    p.h = p.h.saturating_sub(left);
                    p.v = p.v.saturating_sub(top);
                }
            }
        }
        system.files().move_icons(target.location.volume, &rows[..count])?;
        self.last_click = None;
        self.press = None;
        if target.trash
        {
            self.refresh_trash(system)?;
            self.redraw_trash_icon(system)?;
        }
        system.log("FINDER_DRAG_MOVE_COMMITTED\n")?;
        Ok(())
    }
}

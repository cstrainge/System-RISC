//! Finder supplies cached items and target intent. System owns the gesture,
//! feedback, negotiation, modifier policy and terminal outcome.
use mac_toolbox::drag::{self, DragEvent, DragOutcome, DropAction, DropActions, DropPayload,
                        FileItem, Response, Target, TrackPhase, TrackingOffer};

use super::*;
#[path = "copy_window.rs"]
mod copy_window;
#[path = "drop_operation.rs"]
mod operation;
use operation::PendingDrop;
#[path = "position_saves.rs"]
mod position_saves;
use position_saves::PositionSave;
#[derive(Clone, Copy)]
struct Destination
{
    location:    Location,
    application: bool,
    revision:    u64,
    trash:       bool,
    eject:       bool,
    layout_only: bool,
    frame:       Rect,
    scroll:      Point,
    highlight:   Rect
}
struct ApplicationTarget<'a>
{
    location: Location,
    revision: u64,
    drag:     u32,
    batch:    Option<processes::DocumentOpen<'a>>,
    accepted: bool
}
#[derive(Default)]
pub struct Controller<'a>
{
    session:     Option<drag::DragSession<'a>>,
    positions:   Option<PositionSave<'a>>,
    target:      Option<Destination>,
    pending:     Option<PendingDrop<'a>>,
    failure:     Option<files::TransferFailure>,
    recovery:    Option<files::TransferRecovery>,
    application: Option<ApplicationTarget<'a>>
}
/// Failure cleanup is scoped to dispatch, not a later terminal notification.
struct DispatchGuard<'s, 'a>
{
    controller: &'s mut Controller<'a>,
    browser:    &'s mut Browser,
    keep:       bool
}
impl Drop for DispatchGuard<'_, '_>
{
    fn drop(&mut self)
    {
        if !self.keep
        {
            drop(self.controller.session.take());
            self.controller.target = None;
            self.controller.application = None;
            self.browser.press = None;
            self.browser.desktop.pressed = None;
        }
    }
}
fn status(error: drag::DragError) -> Error
{
    match error
    {
        drag::DragError::NoSession => Error::NotFound,
        drag::DragError::Cancelled => Error::Cancelled,
        drag::DragError::WrongThread => Error::WrongThread,
        drag::DragError::Unknown(s) => Error::from_status(s),
        drag::DragError::PromiseFailed { source,
                                         .. } => source,
        drag::DragError::FlavorUnavailable { .. } => Error::Unknown(-1852)
    }
}
impl<'a> Controller<'a>
{
    pub fn cancel(&mut self, browser: &mut Browser)
    {
        drop(self.session.take());
        self.target = None;
        self.application = None;
        browser.press = None;
        browser.desktop.pressed = None;
    }

    pub fn begin(&mut self, system: &System<'a>, browser: &mut Browser) -> Result<(), Error>
    {
        if self.session.is_some() || self.pending.is_some()
        {
            return Ok(());
        }
        if browser.desktop_active
        {
            let Some((index, anchor)) = browser.desktop.pressed
            else
            {
                return Ok(());
            };
            // Volume ejects operate on one mount. Mixed selections never fall
            // through to the file-transfer path or partially unmount a batch.
            if browser.desktop.selected.get().len() != 1
            {
                return Ok(());
            }
            let item = desktop::item(index).ok_or(Error::NotFound)?;
            if item.is_trash()
            {
                return Ok(());
            }
            let mut builder = system.drags().builder(None, anchor, item.bounds).map_err(status)?;
            builder.item(FileItem { volume:    item.volume,
                                    id:        ROOT_ID,
                                    parent:    ROOT_ID,
                                    origin:    Point::default(),
                                    bounds:    item.icon_bounds,
                                    file_type: TypeCode::ZERO,
                                    directory: true })
                   .map_err(status)?;
            self.session = Some(builder.begin().map_err(status)?);
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
        let empty = FileItem { volume:    VolumeId::NONE,
                               id:        Cnid::UNSAVED,
                               parent:    Cnid::UNSAVED,
                               origin:    Point::default(),
                               bounds:    Rect::ZERO,
                               file_type: TypeCode::ZERO,
                               directory: false };
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
                                          bounds,
                                          file_type: item.entry.file_type,
                                          directory: item.entry.is_directory() };
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

    #[inline(never)]
    pub fn event(&mut self, system: &System<'a>, browser: &mut Browser, event: Event<'a>,
                 position: PointerPosition)
                 -> Result<bool, Error>
    {
        let mut guard = DispatchGuard { controller: self,
                                        browser,
                                        keep: false };
        let result = guard.controller.dispatch(system, guard.browser, event, position);
        // Presentation errors cannot release a drop before its storage owner
        // has drained. The next normal event-loop turn performs that cleanup.
        guard.keep = result.is_ok() || guard.controller.pending.is_some();
        result
    }

    /// No second consumer of the event queue. One catalog row or storage step
    /// advances per normal Finder turn; promised data sleeps until its event.
    pub fn runnable(&self) -> bool
    {
        self.pending.as_ref().is_some_and(PendingDrop::runnable)
    }

    pub fn saving_positions(&self) -> bool
    {
        self.positions.is_some()
    }

    pub fn positions_pending(&self, browser: &Browser) -> bool
    {
        self.positions.is_some() || browser.position_requests.get() != 0
    }

    pub fn advance_positions(&mut self, system: &System<'a>, browser: &mut Browser)
                             -> Result<bool, Error>
    {
        if let Some(job) = &mut self.positions
        {
            match job.advance(browser)
            {
                Ok(false) => return Ok(true),
                Ok(true) =>
                {
                    self.positions = None;
                    return Ok(true);
                }
                Err(error) =>
                {
                    self.positions = None;
                    return Err(error);
                }
            }
        }
        if self.pending.is_some()
           || self.session.is_some()
           || browser.position_requests.get() == 0
           || system.events().peek(events::EventMask::ALL)?.is_some()
        {
            return Ok(false);
        }
        while browser.position_requests.get() != 0
        {
            let slot = browser.position_requests.get().trailing_zeros() as usize;
            let bit = 1 << slot;
            browser.position_requests.set(browser.position_requests.get() & !bit);
            if browser.windows[slot].is_none()
            {
                continue;
            }
            match PositionSave::begin(system, browser, slot)
            {
                Ok(Some(job)) =>
                {
                    self.positions = Some(job);
                    return Ok(true);
                }
                Ok(None) => (),
                Err(Error::ServiceBusy) =>
                {
                    browser.position_requests.set(browser.position_requests.get() | bit);
                    return Ok(false);
                }
                Err(error) => return Err(error)
            }
        }
        Ok(false)
    }

    #[inline(never)]
    pub fn advance(&mut self, system: &System<'a>, browser: &mut Browser) -> Result<bool, Error>
    {
        if self.advance_positions(system, browser)?
        {
            return Ok(false);
        }
        let Some(pending) = &mut self.pending
        else
        {
            return Ok(false);
        };
        if !pending.runnable()
        {
            return Ok(false);
        }
        let completed = pending.advance(system, browser);
        self.recovery = pending.take_recovery();
        if !completed
        {
            return Ok(false);
        }
        let pending = self.pending.take().expect("completed drop owner");
        self.target = None;
        browser.press = None;
        browser.last_click = None;
        self.failure = pending.transfer_failure;
        if let Err(error) = pending.finish(system, browser)
        {
            // The dedicated recovery alert includes both the original cause
            // and cleanup failure. Do not replace it with a second modal.
            if self.recovery.is_none()
            {
                return Err(error);
            }
        }
        Ok(true)
    }

    pub fn take_recovery(&mut self) -> Option<files::TransferRecovery>
    {
        self.recovery.take()
    }

    pub fn take_failure(&mut self) -> Option<files::TransferFailure>
    {
        self.failure.take()
    }

    pub fn ui_event(&mut self, system: &System<'a>, event: &Event<'_>, position: PointerPosition)
                    -> bool
    {
        self.pending.as_mut().is_some_and(|pending| pending.ui_event(system, event, position))
    }

    fn dispatch(&mut self, system: &System<'a>, browser: &mut Browser, event: Event<'a>,
                position: PointerPosition)
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
                    self.application = None;
                    return Ok(true);
                }
                let mut target = if offer.flavors().all_file_items()
                {
                    browser.drag_destination(&offer, position)
                }
                else
                {
                    None
                };
                if let Some(candidate) = target.filter(|target| target.application)
                {
                    let key = (candidate.location, candidate.revision, offer.id().0);
                    if self.application
                           .as_ref()
                           .is_none_or(|cache| (cache.location, cache.revision, cache.drag) != key)
                    {
                        self.application = None;
                        let result = (|| {
                            let batch = system.processes().begin_document_drop(processes::DocumentRef { volume: candidate.location.volume, id: candidate.location.folder }, &offer)?;
                            let accepted = batch.accepts_drop(&offer)?;
                            Ok::<_, Error>((batch, accepted))
                        })();
                        let (batch, accepted) = match result
                        {
                            Ok((batch, accepted)) => (Some(batch), accepted),
                            Err(Error::NotSupported) => (None, false),
                            Err(error) =>
                            {
                                let mut message = Text::<256>::default();
                                writeln!(&mut message, "FINDER_DOCUMENT_TARGET_REJECTED volume={} cnid={} error={error}", candidate.location.volume, candidate.location.folder).map_err(|_| Error::NoMemory)?;
                                system.log(message.as_str())?;
                                (None, false)
                            }
                        };
                        self.application = Some(ApplicationTarget { location: key.0,
                                                                    revision: key.1,
                                                                    drag: key.2,
                                                                    batch,
                                                                    accepted });
                    }
                    if !self.application.as_ref().is_some_and(|cache| cache.accepted)
                    {
                        target = None;
                    }
                }
                else
                {
                    self.application = None;
                }
                let response = target.map_or(Response::Reject, |target| {
                                         Response::Accept { target:    if target.eject
                                                            {
                                                                Target::Eject { volume:
                                                                                    target.location
                                                                                          .volume }
                                                            }
                                                            else if target.application
                                                            {
                                                                Target::Application { volume:
                                                                                    target.location
                                                                                          .volume,
                                                                                id:
                                                                                    target.location
                                                                                          .folder }
                                                            }
                                                            else if target.layout_only
                                                            {
                                                                Target::IconLayout { volume:
                                                                                   target.location
                                                                                         .volume,
                                                                               id:
                                                                                   target.location
                                                                                         .folder }
                                                            }
                                                            else if target.trash
                                                            {
                                                                Target::Trash { volume:
                                                                                    target.location
                                                                                          .volume,
                                                                                id:
                                                                                    target.location
                                                                                          .folder }
                                                            }
                                                            else
                                                            {
                                                                Target::Folder { volume:
                                                                                     target.location
                                                                                           .volume,
                                                                                 id:
                                                                                     target.location
                                                                                           .folder }
                                                            },
                                                            actions:   if target.application
                                                            {
                                                                DropActions::Copy
                                                            }
                                                            else if target.layout_only
                                                                      || target.trash
                                                                      || target.eject
                                                            {
                                                                DropActions::Move
                                                            }
                                                            else
                                                            {
                                                                DropActions::CopyOrMove
                                                            },
                                                            highlight: target.highlight }
                                     });
                match offer.respond(response)
                {
                    Ok(()) => self.target = target,
                    Err(error) =>
                    {
                        self.target = None;
                        if !matches!(status(error), Error::ReadOnly | Error::NotFound)
                        {
                            return Err(status(error));
                        }
                    }
                }
            }
            DragEvent::Dropped(drop) =>
            {
                if self.pending.is_some()
                {
                    return Err(Error::ServiceBusy);
                }
                let target = self.target.ok_or(Error::BadArgument)?;
                let documents = if target.application
                {
                    self.application.take().and_then(|cache| cache.batch)
                }
                else
                {
                    None
                };
                self.pending = Some(PendingDrop::begin(system, drop, target, position, documents)?);
                browser.press = None;
                browser.last_click = None;
            }
            DragEvent::Ended { outcome } =>
            {
                let was_click =
                    self.session.as_ref().is_some_and(|s| s.was_click().unwrap_or(false));
                self.session = None;
                self.target = None;
                self.application = None;
                if let Some((index, _)) = browser.desktop.pressed.take()
                   && was_click
                {
                    browser.change_desktop_selection(system,
                                                     Selection::singleton(index),
                                                     Selection::EMPTY)?;
                }
                if let Some(press) = browser.press.take()
                   && was_click
                   && let Some((slot, index)) = press.collapse
                {
                    browser.change_selection(system, slot, Selection::singleton(index), &[])?;
                }
                // Any actual drag ends the click sequence. A rejected/cancelled
                // gesture must not turn its immediate retry into a double-click
                // that launches the source application instead of dragging it.
                if !was_click
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
            {
                if let Some(pending) = &mut self.pending
                {
                    pending.data_ready();
                }
            }
            DragEvent::CancelRequested =>
            {
                if let Some(pending) = &mut self.pending
                {
                    pending.cancel();
                }
            }
        }
        Ok(true)
    }
}
impl Browser
{
    fn drag_destination(&self, offer: &TrackingOffer<'_>, position: PointerPosition)
                        -> Option<Destination>
    {
        let at = offer.position();
        let root = offer.flavors().is_volume_root();
        if root && offer.window().is_some()
        {
            return None;
        }
        if let Some(window) = offer.window()
        {
            let slot = self.windows.iter().position(|w| w.is_some_and(|w| w.window == window))?;
            let window = self.windows[slot]?;
            return cache::with(slot, |folder| {
                if !folder.ready
                {
                    return None;
                }
                let read_only = folder.volume?.read_only;
                let local = position.in_window(window.window).ok()?.point().ok()?;
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
                let mut application = false;
                let mut revision = 0;
                let mut highlight = Rect::new(folder.frame.left,
                                              folder.frame.top + icon_view::INFO_HEIGHT,
                                              folder.frame.right - 15,
                                              folder.frame.bottom - 15);
                let source_here = self.press.is_some_and(|press| press.source == window.location);
                folder.intersecting(Rect::new(content.h, content.v, content.h + 1, content.v + 1),
                                    |index, item| {
                                        if (item.entry.is_directory()
                                            || item.entry.file_type == TypeCode::new(*b"APPL"))
                                           && !(source_here && window.selected.contains(index))
                                        {
                                            location = item.location();
                                            application = !item.entry.is_directory();
                                            revision =
                                                item.entry.modified.mac_microseconds().unwrap_or(0);
                                            highlight = item.bounds;
                                            highlight.offset(folder.frame.left - window.scroll_h,
                                                             folder.frame.top - window.scroll);
                                        }
                                    });
                if read_only && !application && (!source_here || location != window.location)
                {
                    return None;
                }
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
                                   application,
                                   revision,
                                   trash: false,
                                   eject: false,
                                   layout_only: read_only && !application,
                                   frame,
                                   scroll,
                                   highlight })
            });
        }
        let index = desktop::hit(at)?;
        let item = desktop::item(index)?;
        if item.is_trash()
        {
            let volume = if root
            {
                offer.file_metadata(0).ok()?.volume
            }
            else
            {
                self.press.map_or(self.startup, |p| p.entry.volume)
            };
            Some(Destination { application: false,
                               eject:       root,
                               revision:    0,
                               location:    Location { volume,
                                                       folder: Cnid::UNSAVED },
                               trash:       true,
                               layout_only: false,
                               frame:       Rect::ZERO,
                               scroll:      Point::default(),
                               highlight:   item.bounds })
        }
        else
        {
            if root
            {
                return None;
            }
            Some(Destination { application: false,
                               eject:       false,
                               revision:    0,
                               location:    item.location(),
                               trash:       false,
                               layout_only: false,
                               frame:       Rect::ZERO,
                               scroll:      Point::default(),
                               highlight:   item.bounds })
        }
    }

    #[inline(never)]
    fn rearrange_session_icons(&mut self, system: &System<'_>, location: Location,
                               rows: &[files::IconMove])
                               -> Result<(), Error>
    {
        let slot = self.windows
                       .iter()
                       .position(|w| w.is_some_and(|w| w.location == location))
                       .ok_or(Error::NotFound)?;
        let window = self.windows[slot].unwrap();
        let mut damage = [Rect::ZERO; CAPACITY * 2];
        let mut changed = 0;
        // One local ID index for the batch, rather than scanning a folder for
        // each dragged item. Pointer paths continue to use the spatial index.
        let mut by_id = [(0u32, 0u16); CAPACITY];
        let mut indices = [0u16; CAPACITY];
        let mut relocated = false;
        cache::with(slot, |folder| {
            if !folder.ready
            {
                return Err(Error::ScanStale);
            }
            for (index, item) in folder.items[..folder.count].iter().enumerate()
            {
                by_id[index] = (item.entry.id.0, index as u16);
            }
            by_id[..folder.count].sort_unstable_by_key(|pair| pair.0);
            for (index, row) in rows.iter().enumerate()
            {
                if row.expected_parent != location.folder
                   || row.parent != location.folder
                   || row.to_trash
                   || !matches!(row.placement, files::Placement::At(_))
                {
                    return Err(Error::ReadOnly);
                }
                let at = by_id[..folder.count].binary_search_by_key(&row.id.0, |pair| pair.0)
                                              .map_err(|_| Error::ScanStale)?;
                indices[index] = by_id[at].1;
                let files::Placement::At(point) = row.placement
                else
                {
                    unreachable!()
                };
                if point.h.checked_add(48).is_none() || point.v.checked_add(60).is_none()
                {
                    return Err(Error::BadArgument);
                }
                let icon = folder.items[indices[index] as usize].icon_bounds;
                relocated |= icon.left != point.h + 16 || icon.top != point.v + 28;
            }
            Ok(())
        })?;
        if !relocated
        {
            return Ok(());
        }
        super::session_positions::save(location.volume, location.folder, rows)?;
        cache::edit(slot, |folder| {
            for (row, index) in rows.iter().zip(&indices)
            {
                let files::Placement::At(point) = row.placement
                else
                {
                    unreachable!()
                };
                let item = &mut folder.items[*index as usize];
                let x = point.h.saturating_add(16);
                let y = point.v.saturating_add(28);
                if item.icon_bounds.left == x && item.icon_bounds.top == y
                {
                    continue;
                }
                damage[changed] = item.bounds;
                changed += 1;
                item.icon_bounds = Rect::new(x, y, x.saturating_add(32), y.saturating_add(32));
                item.session_position = true;
            }
            if changed == 0
            {
                return Ok(());
            }
            let width = folder.width;
            folder.width = -1;
            folder.layout(width)?;
            for &index in &indices[..rows.len()]
            {
                let item = &folder.items[index as usize];
                damage[changed] = item.bounds;
                changed += 1;
            }
            Ok::<_, Error>(())
        })?;
        if changed == 0
        {
            return Ok(());
        }
        for rect in &mut damage[..changed]
        {
            rect.offset(-window.scroll_h, -window.scroll);
        }
        self.change_selection(system, slot, window.selected, &damage[..changed])?;
        self.configure_zoom(system, slot)?;
        self.refresh_chrome(system, slot, false, true, false)?;
        system.log("FINDER_ICON_LAYOUT_SESSION_COMMITTED\n")
    }
}

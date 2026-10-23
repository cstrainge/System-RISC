//! The receiver retains its drag payload until the resident transfer confirms
//! commit or whole-batch undo. Finder's main loop owns all event dispatch.
#![forbid(unsafe_code)]
use core::{future::Future,
           pin::Pin,
           task::{Context, Poll, Waker}};

use spinning_top::Spinlock;

use super::{copy_window::{CopyWindow, Operation, Phase, Snapshot},
            *};
// Admission copies this array without parking or invoking a driver. Keep it out
// of Finder's already large stack; no borrow survives begin_transfer().
static SOURCES: Spinlock<[files::TransferSource; CAPACITY]> =
    Spinlock::new([files::TransferSource::new(VolumeId::NONE, Cnid::UNSAVED, Cnid::UNSAVED);
                   CAPACITY]);
pub(super) struct PendingDrop<'a>
{
    transfer:                    Option<files::FileTransfer<'a>>,
    documents:                   Option<processes::DocumentOpen<'a>>,
    take:                        Option<drag::FileTakeFuture<'a>>,
    payload:                     DropPayload<'a>,
    ui:                          Option<CopyWindow<'a>>,
    target:                      Destination,
    rows:                        [files::IconMove; CAPACITY],
    volumes:                     [VolumeId; CAPACITY],
    count:                       usize,
    collected:                   usize,
    delta:                       Point,
    ready:                       bool,
    cancelling:                  bool,
    cancel_sent:                 bool,
    result:                      Option<Result<(), Error>>,
    failure:                     Option<Error>,
    pub(super) transfer_failure: Option<files::TransferFailure>,
    recovery:                    Option<files::TransferRecovery>,
    recovery_seen:               Option<files::TransferRecovery>
}
impl<'a> PendingDrop<'a>
{
    #[inline(never)]
    pub(super) fn begin(_system: &System<'a>, payload: DropPayload<'a>, mut target: Destination,
                        position: PointerPosition, documents: Option<processes::DocumentOpen<'a>>)
                        -> Result<Self, Error>
    {
        let count = payload.flavors().item_count().map_err(status)?;
        if count == 0
        {
            return Err(Error::BadArgument);
        }
        if count > CAPACITY
        {
            return Err(Error::NoMemory);
        }
        if target.application && documents.is_none()
        {
            return Err(Error::BadAbi);
        }
        let origin = if target.application || target.eject
        {
            Point::default()
        }
        else
        {
            payload.origin().map_err(status)?
        };
        let at = position.screen().point();
        if target.frame != Rect::ZERO
        {
            let local = position.in_window(payload.window().ok_or(Error::NotFound)?)?.point()?;
            target.frame =
                Rect::new(i16::try_from(i32::from(at.h) - i32::from(local.h)).map_err(|_| {
                                                                                 Error::BadArgument
                                                                             })?,
                          i16::try_from(i32::from(at.v) - i32::from(local.v)).map_err(|_| {
                                                                                 Error::BadArgument
                                                                             })?,
                          target.frame.right,
                          target.frame.bottom);
        }
        let component = |at: i16, origin: i16, frame: i16, scroll: i16, inset: i32| {
            i16::try_from(i32::from(at) - i32::from(origin) - i32::from(frame) + i32::from(scroll)
                          - inset).map_err(|_| Error::BadArgument)
        };
        let delta = if target.application || target.eject
        {
            Point::default()
        }
        else
        {
            Point { h: component(at.h, origin.h, target.frame.left, target.scroll.h, 16)?,
                    v: component(at.v, origin.v, target.frame.top, target.scroll.v, 28)? }
        };
        Ok(Self { documents,
                  transfer: None,
                  take: None,
                  payload,
                  ui: None,
                  target,
                  rows: [files::IconMove::default(); CAPACITY],
                  volumes: [VolumeId::NONE; CAPACITY],
                  count,
                  collected: 0,
                  delta,
                  ready: true,
                  cancelling: false,
                  cancel_sent: false,
                  result: None,
                  failure: None,
                  transfer_failure: None,
                  recovery: None,
                  recovery_seen: None })
    }

    pub(super) fn runnable(&self) -> bool
    {
        self.result.is_some() || self.ready || self.cancelling && !self.cancel_sent
    }

    pub(super) fn take_recovery(&mut self) -> Option<files::TransferRecovery>
    {
        self.recovery.take()
    }

    pub(super) fn data_ready(&mut self)
    {
        self.ready = true;
    }

    pub(super) fn cancel(&mut self)
    {
        if !self.cancelling
        {
            self.cancelling = true;
            self.ready = true;
        }
    }

    fn fail(&mut self, error: Error)
    {
        self.failure.get_or_insert(error);
        self.cancel();
    }

    pub(super) fn ui_event(&mut self, system: &System<'a>, event: &Event<'_>,
                           position: PointerPosition)
                           -> bool
    {
        if let Event::FileTransferChanged { transfer,
                                            .. } = event
        {
            if self.transfer.as_ref().is_some_and(|owner| owner.id() == *transfer)
            {
                self.ready = true;
                return true;
            }
        }
        let escape = matches!(event,Event::KeyDown{key,input,..} if key.character() == '\u{1b}' || key.character() == '.' && input.modifiers.contains(events::Modifiers::COMMAND));
        let handled = if let Some(ui) = &mut self.ui
        {
            let result = ui.event(system, event, position);
            let stopping = ui.stopping();
            if stopping
            {
                self.cancel();
            }
            match result
            {
                Ok(handled) => handled,
                Err(error) =>
                {
                    self.fail(error);
                    true
                }
            }
        }
        else
        {
            false
        };
        if escape
        {
            self.cancel();
        }
        handled || escape
    }

    pub(super) fn advance(&mut self, system: &System<'a>, browser: &mut Browser) -> bool
    {
        if self.result.is_some()
        {
            return true;
        }
        if self.cancelling && !self.cancel_sent
        {
            self.cancel_sent = true;
            if let Err(error) = self.payload.request_cancel()
            {
                self.failure.get_or_insert(status(error));
            }
            if let Some(ui) = &mut self.ui
            {
                if let Err(error) = ui.request_stop(system)
                {
                    self.failure.get_or_insert(error);
                }
            }
            self.take = None;
        }
        if self.transfer.is_some()
        {
            self.ready = false;
            let owner = self.transfer.as_mut().unwrap();
            let result = if self.cancelling { owner.cancel_step() } else { owner.step() };
            match result
            {
                Ok(progress) =>
                {
                    if progress.recovery != self.recovery_seen
                    {
                        self.recovery = progress.recovery;
                        self.recovery_seen = progress.recovery;
                    }
                    if let Some(recovery) = progress.recovery
                    {
                        // A failed cleanup belongs to the resident quarantine,
                        // not to the global pointer protocol. Drop below keeps
                        // that ledger/its leases alive while releasing this UI
                        // and its drag. Report the commit decision truthfully.
                        self.result = Some(if recovery.committed
                                           {
                                               Ok(())
                                           }
                                           else
                                           {
                                               Err(recovery.original.unwrap_or(recovery.error))
                                           });
                        return true;
                    }
                    match progress.phase
                    {
                        files::TransferPhase::Committed => self.result = Some(Ok(())),
                        files::TransferPhase::Cancelled =>
                        {
                            self.result = Some(Err(self.failure.unwrap_or(Error::Cancelled)))
                        }
                        _ =>
                        {
                            if let Some(ui) = &mut self.ui
                            {
                                let phase = match progress.phase
                                {
                                    files::TransferPhase::Preparing => Phase::Preparing,
                                    files::TransferPhase::Reading => Phase::Reading,
                                    files::TransferPhase::Writing => Phase::Writing,
                                    files::TransferPhase::Verifying => Phase::Verifying,
                                    files::TransferPhase::RollingBack => Phase::Undoing,
                                    files::TransferPhase::Committing
                                    | files::TransferPhase::CleaningUp => Phase::Finishing,
                                    _ => Phase::Copying
                                };
                                let (completed, total) = if progress.total_bytes != 0
                                {
                                    (progress.completed_bytes, progress.total_bytes)
                                }
                                else
                                {
                                    (u64::from(progress.completed_items),
                                     u64::from(progress.total_items))
                                };
                                if let Err(error) =
                                    ui.update(system,
                                              Snapshot { phase,
                                                         item: progress.current
                                                                       .as_ref()
                                                                       .map_or("", |entry| {
                                                                           entry.name()
                                                                       }),
                                                         remaining:
                                                             u64::from(progress.total_items
                                                                       - progress.completed_items),
                                                         completed,
                                                         in_progress: 0,
                                                         total: (total != 0).then_some(total) })
                                {
                                    self.fail(error);
                                }
                            }
                        }
                    }
                }
                Err(error) =>
                {
                    let finished =
                        self.transfer.as_ref().is_some_and(files::FileTransfer::is_finished);
                    self.transfer_failure = Some(error);
                    if finished
                    {
                        self.result = Some(Err(error.cause));
                    }
                    else
                    {
                        self.fail(error.cause);
                        self.ready = true;
                    }
                }
            }
        }
        else if self.cancelling
        {
            self.documents = None;
            self.result = Some(Err(self.failure.unwrap_or(Error::Cancelled)));
        }
        else if let Err(error) = self.step(system, browser)
        {
            self.fail(error);
        }
        self.result.is_some()
    }

    fn step(&mut self, system: &System<'a>, browser: &mut Browser) -> Result<(), Error>
    {
        if self.collected < self.count
        {
            if !self.ready
            {
                return Ok(());
            }
            if self.take.is_none()
            {
                let item = self.payload.flavors().item(self.collected).map_err(status)?;
                self.take = Some(self.payload.file(item));
            }
            let mut context = Context::from_waker(Waker::noop());
            let file = match Pin::new(self.take.as_mut().unwrap()).poll(&mut context)
            {
                Poll::Pending =>
                {
                    self.ready = false;
                    return Ok(());
                }
                Poll::Ready(result) => result.map_err(status)?
            };
            self.take = None;
            if self.target.eject
            {
                if self.count != 1
                   || file.id != ROOT_ID
                   || !file.directory
                   || file.volume != self.target.location.volume
                {
                    return Err(Error::BadArgument);
                }
                self.collected += 1;
                return Ok(());
            }
            if let Some(documents) = &mut self.documents
            {
                documents.push(processes::DocumentRef { volume: file.volume,
                                                        id:     file.id })?;
                self.collected += 1;
                return Ok(());
            }
            let point =
                Point { h: file.bounds.left.checked_add(self.delta.h).ok_or(Error::BadArgument)?,
                        v: file.bounds.top.checked_add(self.delta.v).ok_or(Error::BadArgument)? };
            self.volumes[self.collected] = file.volume;
            self.rows[self.collected] =
                files::IconMove { id:              file.id,
                                  expected_parent: file.parent,
                                  parent:          self.target.location.folder,
                                  placement:       if self.target.trash
                                  {
                                      files::Placement::Clear
                                  }
                                  else
                                  {
                                      files::Placement::At(point)
                                  },
                                  to_trash:        self.target.trash };
            self.collected += 1;
            return Ok(());
        }
        if self.target.eject
        {
            browser.unmount_volume(system, self.target.location.volume)?;
            self.result = Some(Ok(()));
            return Ok(());
        }
        if let Some(documents) = self.documents.take()
        {
            documents.commit()?;
            self.result = Some(Ok(()));
            return Ok(());
        }
        if !self.target.trash && self.target.frame == Rect::ZERO
        {
            let mut left = i16::MAX;
            let mut top = i16::MAX;
            for row in &self.rows[..self.count]
            {
                if let files::Placement::At(p) = row.placement
                {
                    left = left.min(p.h);
                    top = top.min(p.v);
                }
            }
            for row in &mut self.rows[..self.count]
            {
                if let files::Placement::At(p) = &mut row.placement
                {
                    p.h = p.h.checked_sub(left).ok_or(Error::BadArgument)?;
                    p.v = p.v.checked_sub(top).ok_or(Error::BadArgument)?;
                }
            }
        }
        let rows = &self.rows[..self.count];
        if self.target.layout_only
        {
            if self.payload.action != DropAction::Move
               || self.volumes[..self.count].iter().any(|v| *v != self.target.location.volume)
            {
                return Err(Error::ReadOnly);
            }
            browser.rearrange_session_icons(system, self.target.location, rows)?;
            self.result = Some(Ok(()));
        }
        else
        {
            let copy = self.payload.action == DropAction::Copy && !self.target.trash;
            if copy
               || self.target.trash
               || self.volumes[..self.count].iter().zip(rows).any(|(v, row)| {
                                                                 *v != self.target.location.volume
                                                                 || row.expected_parent
                                                                    != row.parent
                                                             })
            {
                self.ui =
                    Some(CopyWindow::new(system,
                                         if copy { Operation::Copy } else { Operation::Move })?);
            }
            let mut sources = SOURCES.lock();
            for (at, (volume, row)) in self.volumes[..self.count].iter().zip(rows).enumerate()
            {
                let source = files::TransferSource::new(*volume, row.id, row.expected_parent);
                sources[at] = match row.placement
                {
                    files::Placement::Keep => source,
                    files::Placement::Clear => source.clear_placement(),
                    files::Placement::At(point) => source.at(point)
                };
            }
            self.transfer=Some(system.files().begin_transfer(files::TransferSpec{
                mode:if copy {files::TransferMode::Copy} else {files::TransferMode::Move},
                destination:if self.target.trash {files::TransferDestination::Trash} else {files::TransferDestination::Folder{volume:self.target.location.volume,parent:self.target.location.folder}},
            },&sources[..self.count])?);
            self.ready = true;
        }
        Ok(())
    }

    pub(super) fn finish(mut self, system: &System<'a>, browser: &mut Browser)
                         -> Result<(), Error>
    {
        let result = self.result.take().ok_or(Error::BadAbi)?;
        self.transfer = None;
        self.take = None;
        let acknowledged =
            self.payload.complete(result.map_err(drag::DragError::from)).map_err(status);
        if result == Err(Error::Cancelled) && acknowledged == Err(Error::Cancelled)
        {
            return system.log("FINDER_DRAG_CANCELLED_AFTER_UNDO\n");
        }
        acknowledged?;
        result?;
        if self.target.eject
        {
            return system.log("FINDER_DRAG_VOLUME_UNMOUNTED\n");
        }
        if self.target.trash
        {
            browser.refresh_trash(system)?;
            browser.redraw_trash_icon(system)?;
        }
        system.log("FINDER_DRAG_TRANSFER_COMMITTED\n")
    }
}

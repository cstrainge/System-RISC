//! A retained phase/clip cursor over ordinary staging. No Canvas, manager
//! borrow, execution pin or application pointer survives an author wait.
#![forbid(unsafe_code)]
use alloc::{sync::Arc, vec::Vec};

use mac_abi::{ACCESS_DENIED, BAD_ABI, NO_MEMORY, NOT_FOUND, OPERATION_CANCELLED, SERVICE_BUSY,
              Status, WindowId};

use super::{super::quickdraw::retained::{ContentCursor, ContentProgress, ContentResume},
            Canvas, RenderCommand, SceneTarget, Toolbox, WHITE, background,
            ordinary_underlay::{copy_rect, screen},
            paint};
use crate::{machine::scheduler::accounting::AllocationAuthority,
            toolbox::{controls::paint::{self as control_paint, producer as control_producer},
                      processes::ResourceBinding,
                      quickdraw::{geometry::{Rect, sect_rect},
                                  region::Region},
                      resources::definitions::callbacks::Notice,
                      windows::manager::{canvas::{CanvasClaim, WindowCanvasGuardian},
                                         paint::{PaintDomain, PaintLoan, PaintOwner,
                                                 PaintProgress, WindowRenderStamp}}}};

#[derive(Clone, Copy)]
enum Phase
{
    Frame,
    Content,
    Grow
}
#[derive(Clone, Copy)]
struct Step
{
    window: WindowId,
    clip:   Rect,
    phase:  Phase
}
struct Read
{
    id:      WindowId,
    stamp:   WindowRenderStamp,
    content: Option<u64>
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthorStage
{
    Prepare,
    Capture,
    Advance,
    Waiting,
    Retire
}
#[derive(Clone)]
enum Owner
{
    Window(Arc<PaintOwner>),
    Control(Arc<control_producer::PaintOwner>)
}
impl Owner
{
    fn same(&self, other: &Self) -> bool
    {
        match (self, other)
        {
            (Self::Window(a), Self::Window(b)) => Arc::ptr_eq(a, b),
            (Self::Control(a), Self::Control(b)) => Arc::ptr_eq(a, b),
            _ => false
        }
    }

    fn active(&self) -> bool
    {
        match self
        {
            Self::Window(o) => o.active(),
            Self::Control(o) => o.active()
        }
    }

    fn ready(&self) -> bool
    {
        match self
        {
            Self::Window(o) => o.ready(),
            Self::Control(o) => o.ready()
        }
    }

    fn take_ready(&self)
    {
        match self
        {
            Self::Window(o) =>
            {
                o.take_ready();
            }
            Self::Control(o) =>
            {
                o.take_ready();
            }
        }
    }

    fn cancel(&self)
    {
        match self
        {
            Self::Window(o) => o.cancel(),
            Self::Control(o) => o.cancel()
        }
    }

    fn checkout(&self) -> Result<Loan, Status>
    {
        match self
        {
            Self::Window(o) => o.checkout().map(Loan::Window),
            Self::Control(o) => o.checkout().map(Loan::Control)
        }
    }
}
enum Loan
{
    Window(PaintLoan),
    Control(control_producer::PaintLoan)
}
impl Loan
{
    fn prepare(&mut self) -> Result<(), ServiceFailure>
    {
        match self
        {
            Self::Window(l) => l.prepare().map_err(Into::into),
            Self::Control(l) => l.prepare().map_err(Into::into)
        }
    }

    fn cancelled(&self, owner: &Owner) -> Result<bool, ServiceFailure>
    {
        match (self, owner)
        {
            (Self::Window(_), Owner::Window(owner)) => Ok(owner.cancelled()),
            (Self::Control(loan), Owner::Control(_)) => loan.cancelled().map_err(Into::into),
            _ => Err(BAD_ABI.into())
        }
    }

    fn finish(&mut self) -> Result<(), ServiceFailure>
    {
        match self
        {
            Self::Window(l) => l.finish().map_err(Into::into),
            Self::Control(l) => l.finish().map_err(Into::into)
        }
    }

    fn advance(&mut self) -> Result<Progress, ServiceFailure>
    {
        match self
        {
            Self::Window(loan) =>
            {
                Ok(match loan.advance()?
                {
                    PaintProgress::Pending => Progress::Pending,
                    PaintProgress::Ready => Progress::Ready,
                    PaintProgress::Cancelled => Progress::Cancelled
                })
            }
            Self::Control(loan) =>
            {
                Ok(match loan.advance()
                {
                    control_producer::PaintProgress::Pending => Progress::Pending,
                    control_producer::PaintProgress::Ready => Progress::Ready,
                    control_producer::PaintProgress::Cancelled => Progress::Cancelled,
                    control_producer::PaintProgress::Failed(failure) => return Err(failure.into())
                })
            }
        }
    }
}
enum Progress
{
    Pending,
    Ready,
    Cancelled
}
struct Author
{
    owner:              Owner,
    stage:              AuthorStage,
    // Original malformed/uncertain retirement cause stays with SAME owner.
    // This is a retained row, not a terminal callback success or a retry hint.
    retirement_failure: Option<ServiceFailure>
}

/// Original election, never recreated from a raw id when admission is Busy.
/// Partial claims stay canonical until admission or guard-free retirement.
struct Election
{
    plan:       Option<control_paint::PaintPlan>,
    binding:    Option<ResourceBinding>,
    allocation: Option<AllocationAuthority>,
    notice:     Option<Arc<Notice>>,
    guardian:   Option<WindowCanvasGuardian>,
    claim:      Option<control_producer::PaintClaim>,
    canvas:     Option<CanvasClaim>
}
impl Election
{
    fn new(plan: control_paint::PaintPlan) -> Self
    {
        Self { plan:       Some(plan),
               binding:    None,
               allocation: None,
               notice:     None,
               guardian:   None,
               claim:      None,
               canvas:     None }
    }

    fn cancelled(&self) -> bool
    {
        self.canvas.as_ref().is_some_and(CanvasClaim::cancelled)
    }
}
struct ContentStep
{
    step:     usize,
    cursor:   ContentCursor,
    resume:   Option<ContentResume>,
    election: Option<Election>
}

#[derive(Default)]
pub(super) struct Storage
{
    reads: Vec<Read>,
    steps: Vec<Step>
}

pub(super) struct Transaction
{
    dimensions:     (usize, usize),
    full:           bool,
    damage:         Vec<(SceneTarget, Region)>,
    reads:          Vec<Read>,
    steps:          Vec<Step>,
    next:           usize,
    author:         Option<Author>,
    content:        Option<ContentStep>,
    retired:        Option<Election>,
    rejected:       Option<control_producer::PaintInputs>,
    admission_wait: bool,
    notice:         Arc<Notice>,
    cancelled:      bool,
    failure:        Option<ServiceFailure>
}
impl Transaction
{
    fn take_storage(&mut self) -> Storage
    {
        self.reads.clear();
        self.steps.clear();
        Storage { reads: core::mem::take(&mut self.reads),
                  steps: core::mem::take(&mut self.steps) }
    }

    pub(super) fn can_resume(&self) -> bool
    {
        // Callback completion is serviced outside Desktop. Merely receiving
        // more input must not poll the callback or replay the drawn prefix.
        self.author.is_none()
        && self.retired.is_none()
        && self.rejected.is_none()
        && (!self.admission_wait
            || self.content
                   .as_ref()
                   .and_then(|c| c.election.as_ref())
                   .is_some_and(Election::cancelled))
    }

    pub(super) fn source_returned(&mut self)
    {
        // A real common return merely permits a pure SAME-target readiness
        // check. No source cloning/drop or reservation occurs on this edge.
        self.admission_wait = false;
    }

    fn unpublished(&self) -> bool
    {
        self.retired.is_some()
        || self.rejected.is_some()
        || self.content.as_ref().is_some_and(|c| c.election.is_some())
    }

    fn cancel(&mut self)
    {
        let first = !self.cancelled;
        self.cancelled = true;
        self.admission_wait = false;
        if let Some(author) = &self.author
        {
            author.owner.cancel();
        }
        if let Some(canvas) =
            self.content.as_ref().and_then(|c| c.election.as_ref()).and_then(|e| e.canvas.as_ref())
        {
            canvas.cancel();
        }
        if first && self.unpublished()
        {
            // New real retirement intent, not author completion/availability.
            self.notice.request();
        }
    }

    fn valid(&self, toolbox: &Toolbox) -> bool
    {
        if self.cancelled || self.dimensions != (crate::display::width(), crate::display::height())
        {
            return false;
        }
        // Geometry/source and z order determine visibility, independently of
        // content. Only windows actually read by this transaction contribute
        // their committed-content stamp.
        let mut current =
            toolbox.windows.back_to_front().filter(|window| !window.uses_system_resources());
        for read in &self.reads
        {
            let Some(window) = current.next()
            else
            {
                return false;
            };
            if window.id != read.id
               || !toolbox.windows.render_stamp(read.id).is_ok_and(|stamp| stamp == read.stamp)
               || read.content.is_some_and(|revision| {
                                  toolbox.windows.content_revision(read.id) != Ok(revision)
                              })
            {
                return false;
            }
        }
        current.next().is_none()
    }
}

fn push<T>(items: &mut Vec<T>, item: T) -> Result<(), Status>
{
    items.try_reserve(1).map_err(|_| NO_MEMORY)?;
    items.push(item);
    Ok(())
}

impl Toolbox
{
    fn plan_ordinary_transaction(&mut self, full: bool, damage: &[(SceneTarget, Region)])
                                 -> Result<Transaction, Status>
    {
        let storage = core::mem::take(&mut self.presentation.ordinary_underlay.storage);
        let mut transaction = Transaction { dimensions: (crate::display::width(),
                                                         crate::display::height()),
                                            full,
                                            damage: Vec::new(),
                                            reads: storage.reads,
                                            steps: storage.steps,
                                            next: 0,
                                            author: None,
                                            content: None,
                                            retired: None,
                                            rejected: None,
                                            admission_wait: false,
                                            notice: self.windows.operation_notice(),
                                            cancelled: false,
                                            failure: None };
        let result = (|| -> Result<(), Status> {
            let ordinary =
                |window: &crate::toolbox::windows::manager::Window| !window.uses_system_resources();
            for window in self.windows.back_to_front().filter(|window| ordinary(window))
            {
                let affected = full || damage.iter().any(|(id, _)| *id == window.id);
                push(&mut transaction.reads,
                     Read { id:      window.id,
                            stamp:   self.windows.render_stamp(window.id)?,
                            content: if affected
                            {
                                Some(self.windows.content_revision(window.id)?)
                            }
                            else
                            {
                                None
                            } })?;
                if !affected
                {
                    continue;
                }
                if full
                {
                    // As in draw_layer, ordinary frames render back to front.
                    // Content and partial grow adornments use the actual visible
                    // fragments rather than guessing a surrounding rectangle.
                    push(&mut transaction.steps,
                         Step { window: window.id,
                                clip:   screen(),
                                phase:  Phase::Frame })?;
                }
                else
                {
                    // Chrome requests share the retained damage queue but are
                    // allowed outside content. Ordinary content requests remain
                    // inside content and therefore add no author Frame call.
                    let bounds = window.bounds;
                    let frame = window.frame_rects();
                    for (_, region) in damage.iter().filter(|(id, _)| *id == window.id)
                    {
                        for band in region.bands()
                        {
                            for span in &band.spans
                            {
                                let mut global =
                                    Rect::new(span.left, band.top, span.right, band.bottom);
                                global.offset(bounds.left, bounds.top);
                                for part in frame
                                {
                                    let mut target = Rect::ZERO;
                                    if !sect_rect(global, part, &mut target)
                                    {
                                        continue;
                                    }
                                    let mut result = Ok(());
                                    self.windows.for_each_layer_frame_rect(window.id, target, &ordinary,
                                    |clip| {
                                        if result.is_ok()
                                        {
                                            result = push(&mut transaction.steps,
                                                Step { window: window.id, clip, phase: Phase::Frame });
                                        }
                                    });
                                    result?;
                                }
                            }
                        }
                    }
                }
                let mut result = Ok(());
                self.windows.for_each_layer_visible_rect(window.id, &ordinary, |visible| {
                                if result.is_err()
                                {
                                    return;
                                }
                                if full
                                {
                                    result = push(&mut transaction.steps,
                                                  Step { window: window.id,
                                                         clip:   visible,
                                                         phase:  Phase::Content });
                                    return;
                                }
                                for (_, region) in damage.iter().filter(|(id, _)| *id == window.id)
                                {
                                    for band in region.bands()
                                    {
                                        for span in &band.spans
                                        {
                                            if result.is_err()
                                            {
                                                return;
                                            }
                                            let mut global = Rect::new(span.left, band.top,
                                                                       span.right, band.bottom);
                                            global.offset(window.bounds.left, window.bounds.top);
                                            let mut clip = Rect::ZERO;
                                            if sect_rect(global, visible, &mut clip)
                                            {
                                                result = push(&mut transaction.steps,
                                                              Step { window: window.id,
                                                                     clip,
                                                                     phase: Phase::Content });
                                                if result.is_ok()
                                                {
                                                    result = push(&mut transaction.steps,
                                                                  Step { window: window.id,
                                                                         clip,
                                                                         phase: Phase::Grow });
                                                }
                                            }
                                        }
                                    }
                                }
                            });
                result?;
                if full
                {
                    push(&mut transaction.steps,
                         Step { window: window.id,
                                clip:   screen(),
                                phase:  Phase::Grow })?;
                }
            }
            Ok(())
        })();
        if let Err(status) = result
        {
            self.presentation.ordinary_underlay.storage = transaction.take_storage();
            return Err(status);
        }
        Ok(transaction)
    }

    fn paint_ordinary_step(&mut self, transaction: &mut Transaction, pixels: &mut [u32])
                           -> Result<(), Status>
    {
        let step = *transaction.steps.get(transaction.next).ok_or(BAD_ABI)?;
        let window = *self.windows.window(step.window).ok_or(NOT_FOUND)?;
        let scopes = self.windows.resource_scopes();
        let mut canvas = Canvas::new(pixels, transaction.dimensions.0, transaction.dimensions.1);
        canvas.set_clip(step.clip);
        if matches!(step.phase, Phase::Content)
        {
            if transaction.content.is_none()
            {
                transaction.content = Some(ContentStep { step:     transaction.next,
                                                         cursor:   ContentCursor::default(),
                                                         resume:   None,
                                                         election: None });
            }
            let state = transaction.content.as_mut().ok_or(BAD_ABI)?;
            if state.step != transaction.next
            {
                return Err(BAD_ABI);
            }
            if state.election.is_some()
            {
                return self.admit_content_election(transaction, &window);
            }
            let fonts = crate::kernel::fonts();
            let content =
                self.presentation.content.iter().find(|content| content.id == step.window);
            let cdef = self.cdef;
            let cache = &mut self.presentation.content_cache;
            canvas.set_origin(window.bounds.left, window.bounds.top);
            let mut elected = None;
            let result = scopes.with_window(&window, || {
                                   let mut replay = |canvas: &mut Canvas<'_>| {
                                       if !state.cursor.started()
                                       {
                                           canvas.paint_rect(window.local_bounds(), WHITE);
                                       }
                                       if let Some(content) = content
                                       {
                                           match content.paint_step(&mut state.cursor,
                                                                    canvas,
                                                                    fonts.as_deref(),
                                                                    |canvas, command| {
                                                                        paint(canvas, command, cdef)
                                                                    })?
                                           {
                                               ContentProgress::Complete => (),
                                               ContentProgress::User(plan) =>
                                               {
                                                   elected = Some(plan);
                                                   return Err(SERVICE_BUSY);
                                               }
                                           }
                                       }
                                       Ok(())
                                   };
                                   if content.is_some_and(|c| {
                                                 c.has_user_sources()
                                          || c.iter_commands()
                                              .any(|command| {
                                                  matches!(command, RenderCommand::ScrollBox(_))
                                              })
                                             })
                                   {
                                       replay(&mut canvas)
                                   }
                                   else
                                   {
                                       cache.try_paint(window.id.0, window.bounds, &mut canvas,
                                                       replay)
                                   }
                               })
                               .and_then(core::convert::identity);
            if let Some(plan) = elected
            {
                let (plan, resume) = plan.into_parts()?;
                state.resume = Some(resume);
                state.election = Some(Election::new(plan));
                return self.admit_content_election(transaction, &window);
            }
            result?;
            transaction.content = None;
            return Ok(());
        }
        let domain = match step.phase
        {
            Phase::Frame => PaintDomain::Frame,
            Phase::Grow => PaintDomain::GrowIcon,
            Phase::Content => return Err(BAD_ABI)
        };
        scopes.with_window(&window, || {
                  if self.windows.draw_trusted_paint(step.window, domain, &mut canvas)?
                  {
                      return Ok(());
                  }
                  let Some(plan) = self.windows.plan_paint(step.window, domain, &canvas)?
                  else
                  {
                      return Ok(());
                  };
                  transaction.author = Some(Author { owner:
                                                         Owner::Window(self.windows
                                                                           .admit_paint(plan)?),
                                                     stage:              AuthorStage::Prepare,
                                                     retirement_failure: None });
                  Err(SERVICE_BUSY)
              })
              .and_then(core::convert::identity)
    }

    fn admit_content_election(&mut self, transaction: &mut Transaction,
                              window: &crate::toolbox::windows::manager::Window)
                              -> Result<(), Status>
    {
        let election =
            transaction.content.as_mut().and_then(|c| c.election.as_mut()).ok_or(BAD_ABI)?;
        control_producer::PaintOwner::check_policy(election.plan.as_ref().ok_or(BAD_ABI)?)?;
        if election.cancelled()
        {
            return Err(OPERATION_CANCELLED);
        }
        if election.guardian.is_none()
        {
            election.guardian = Some(self.windows.canvas_guardian(window.id)?);
        }
        let guardian = election.guardian.as_ref().ok_or(BAD_ABI)?;
        if election.binding.is_none()
        {
            election.binding = Some(self.processes.resource_binding(window.owner)?);
        }
        if election.allocation.is_none()
        {
            election.allocation = Some(self.processes.allocation(window.owner)?);
        }
        let allocation = election.allocation.as_ref().ok_or(BAD_ABI)?;
        if election.binding.as_ref().ok_or(BAD_ABI)?.ledger().owner() != guardian.storage_owner()
           || allocation.owner() != guardian.storage_owner()
        {
            return Err(ACCESS_DENIED);
        }
        if election.notice.is_none()
        {
            let target =
                crate::allocator::Target::Owner { account: allocation.clone(),
                                                  class:
                                                      mac_kernel_model::policy::MemoryClass::Kernel };
            election.notice = Some(crate::allocator::with_target(target, || {
                                       Notice::linked(self.windows.operation_notice())
                                   })?);
        }
        let notice = election.notice.as_ref().ok_or(BAD_ABI)?;
        if election.canvas.is_none()
        {
            if !self.windows.canvas_available(guardian)?
            {
                transaction.admission_wait = true;
                return Err(SERVICE_BUSY);
            }
            match self.windows.claim_canvas(guardian, notice.clone())
            {
                Ok(canvas) => election.canvas = Some(canvas),
                Err(status) =>
                {
                    transaction.admission_wait = status == SERVICE_BUSY;
                    return Err(status);
                }
            }
        }
        let plan = election.plan.as_ref().ok_or(BAD_ABI)?;
        if election.claim.is_none()
        {
            let claim = match plan.origin()
            {
                control_paint::PaintOrigin::Stateless => None,
                control_paint::PaintOrigin::Managed(origin) =>
                {
                    match self.controls.claim_managed_paint(&origin, allocation)
                    {
                        Ok(claim) => Some(control_producer::PaintClaim::Managed(claim)),
                        Err(status) =>
                        {
                            transaction.admission_wait = status == SERVICE_BUSY;
                            return Err(status);
                        }
                    }
                }
                control_paint::PaintOrigin::ScrollBox(origin) =>
                {
                    match plan.with_scroll_box_scope(|| {
                                  self.scroll_boxes.claim_scroll_box_paint(&origin, plan.source())
                              })
                              .and_then(core::convert::identity)
                    {
                        Ok(claim) => Some(control_producer::PaintClaim::ScrollBox(claim)),
                        Err(status) =>
                        {
                            transaction.admission_wait = status == SERVICE_BUSY;
                            return Err(status);
                        }
                    }
                }
            };
            election.claim = claim;
        }
        // All further failure custody is returned by the factory. Ordinary cell
        // metadata admission is under Desktop; scratch/packet/author work is not.
        let plan = election.plan.take().ok_or(BAD_ABI)?;
        let canvas = election.canvas.take().ok_or(BAD_ABI)?;
        let claim = election.claim.take();
        let result = control_producer::PaintOwner::prepare(plan,
                                                           canvas,
                                                           claim,
                                                           allocation.clone(),
                                                           notice.clone());
        transaction.retired = transaction.content.as_mut().ok_or(BAD_ABI)?.election.take();
        transaction.admission_wait = false;
        match result
        {
            Ok(owner) =>
            {
                // SAME canonical transaction owns the real Arc BEFORE enqueue.
                transaction.author = Some(Author { owner:              Owner::Control(owner),
                                                   stage:              AuthorStage::Prepare,
                                                   retirement_failure: None });
                transaction.notice.request();
                Err(SERVICE_BUSY)
            }
            Err((failure, inputs)) =>
            {
                transaction.rejected = Some(inputs);
                transaction.failure = Some(failure.into());
                transaction.cancel();
                Err(failure.status())
            }
        }
    }

    /// Continue exactly the retained phase and clip. Pixels remain private
    /// until all phases and their semantic dependencies have been accepted.
    pub(super) fn update_ordinary_underlay(&mut self, damage: &mut Vec<(SceneTarget, Region)>,
                                           full: bool, available: usize,
                                           screen_damage: &mut Vec<Rect>, copy_full: &mut bool)
                                           -> Result<bool, Status>
    {
        self.presentation.ordinary_underlay.full_deferred |= full;
        let existing = self.presentation.ordinary_underlay.transaction.is_some();
        if self.presentation.ordinary_underlay.staging.len() != crate::display::pixels()
           || self.presentation.ordinary_underlay.committed.len() != crate::display::pixels()
        {
            return Err(mac_abi::NOT_SUPPORTED);
        }
        let mut transaction = if let Some(transaction) =
            self.presentation.ordinary_underlay.transaction.take()
        {
            transaction
        }
        else
        {
            let underlay = &mut self.presentation.ordinary_underlay;
            let requested_full = full || underlay.full_deferred || !underlay.valid;
            let mut queued = core::mem::take(&mut underlay.deferred);
            if super::shared_scene_allocation(|| queued.try_reserve(damage.len())).is_err()
            {
                underlay.deferred = queued;
                return Err(NO_MEMORY);
            }
            let mut index = 0;
            while index < damage.len()
            {
                if self.system_scene(damage[index].0)
                {
                    index += 1;
                }
                else
                {
                    queued.push(damage.swap_remove(index));
                }
            }
            let mut transaction = match super::shared_scene_allocation(|| {
                      self.plan_ordinary_transaction(requested_full, &queued)
                  })
            {
                Ok(transaction) => transaction,
                Err(status) =>
                {
                    let underlay = &mut self.presentation.ordinary_underlay;
                    underlay.deferred = queued;
                    underlay.full_deferred |= requested_full;
                    underlay.refuse(available, status);
                    return Err(status);
                }
            };
            transaction.damage = queued;
            if requested_full
            {
                let mut canvas = Canvas::new(&mut self.presentation.ordinary_underlay.staging,
                                             crate::display::width(),
                                             crate::display::height());
                background(&mut canvas);
            }
            transaction
        };
        if (existing && full)
           || !transaction.valid(self)
           || transaction.content
                         .as_ref()
                         .and_then(|c| c.election.as_ref())
                         .is_some_and(Election::cancelled)
        {
            transaction.cancel();
        }
        if transaction.cancelled
        {
            if transaction.author.is_some() || transaction.unpublished()
            {
                self.presentation.ordinary_underlay.transaction = Some(transaction);
                return Err(SERVICE_BUSY);
            }
            let underlay = &mut self.presentation.ordinary_underlay;
            if transaction.full
            {
                copy_rect(&underlay.committed, &mut underlay.staging, screen());
            }
            else
            {
                for step in &transaction.steps
                {
                    copy_rect(&underlay.committed, &mut underlay.staging, step.clip);
                }
            }
            underlay.storage = transaction.take_storage();
            // The actual staging owner has returned, including after rollback.
            // A retained mode request can now try its real preflight again.
            self.presentation.video_mode_wait = false;
            underlay.deferred = core::mem::take(&mut transaction.damage);
            underlay.full_deferred |= transaction.full;
            if let Some(status) = transaction.failure
            {
                underlay.refuse(available, status.status());
            }
            else
            {
                underlay.refused_available = None;
            }
            return Err(OPERATION_CANCELLED);
        }
        if transaction.author.is_some()
           || transaction.retired.is_some()
           || transaction.rejected.is_some()
        {
            self.presentation.ordinary_underlay.transaction = Some(transaction);
            return Err(SERVICE_BUSY);
        }
        if transaction.admission_wait
        {
            self.presentation.ordinary_underlay.transaction = Some(transaction);
            return Err(SERVICE_BUSY);
        }
        let mut pixels = core::mem::take(&mut self.presentation.ordinary_underlay.staging);
        let result: Result<(), Status> = crate::context::without_recovery(|| {
            while transaction.next < transaction.steps.len()
            {
                self.paint_ordinary_step(&mut transaction, &mut pixels)?;
                transaction.next += 1;
            }
            Ok(())
        });
        self.presentation.ordinary_underlay.staging = pixels;
        if let Err(status) = result
        {
            if transaction.author.is_none()
               && !(status == SERVICE_BUSY && transaction.admission_wait)
            {
                // No admitted callback to preserve. Roll back only this
                // transaction, retaining its exact work for an actual retry.
                transaction.cancel();
                if status != OPERATION_CANCELLED
                {
                    transaction.failure.get_or_insert(status.into());
                }
            }
            self.presentation.ordinary_underlay.transaction = Some(transaction);
            return Err(status);
        }
        if transaction.full
        {
            // The committed ordinary base includes the actual menu bar, as
            // it did before callbacks could suspend. Service windows may later
            // expose it without borrowing an application's drawing context.
            let mut canvas = Canvas::new(&mut self.presentation.ordinary_underlay.staging,
                                         crate::display::width(),
                                         crate::display::height());
            let result = self.presentation.menu_cache.paint(&mut canvas,
                                                            (self.foreground,
                                                             self.menus.selected_menu(),
                                                             self.menus.application_selected(),
                                                             self.menus.system_selected()),
                                                            |canvas| {
                                                                self.menus
                                                                    .draw_bar(&mut self.system_bar,
                                                                              &mut self.app_menu,
                                                                              canvas)
                                                            });
            if let Err(error) = result
            {
                transaction.cancel();
                transaction.failure.get_or_insert(error.status.into());
                self.presentation.ordinary_underlay.transaction = Some(transaction);
                return Err(error.status);
            }
        }
        // The pass was protected by the same Desktop guard since its last
        // semantic validation. No author output can sneak in at publication.
        let underlay = &mut self.presentation.ordinary_underlay;
        let full = transaction.full;
        if full
        {
            copy_rect(&underlay.staging, &mut underlay.committed, screen());
        }
        else
        {
            for step in &transaction.steps
            {
                copy_rect(&underlay.staging, &mut underlay.committed, step.clip);
            }
        }
        underlay.valid = true;
        underlay.full_deferred = false;
        underlay.refused_available = None;
        underlay.refusal = None;
        let back = crate::platform::back_buffer();
        let mut expose = |rect| {
            self.windows.for_each_ordinary_underlay_rect(rect, |visible| {
                            super::accumulate_presentation_damage(screen_damage, copy_full,
                                                                  visible);
                            copy_rect(&underlay.committed, back, visible);
                        });
        };
        if full
        {
            expose(screen());
        }
        else
        {
            let mut previous = None;
            for step in &transaction.steps
            {
                if previous != Some(step.clip)
                {
                    expose(step.clip);
                    previous = Some(step.clip);
                }
            }
        }
        let changed = full || !transaction.steps.is_empty();
        underlay.storage = transaction.take_storage();
        self.presentation.video_mode_wait = false;
        transaction.damage.clear();
        underlay.deferred = core::mem::take(&mut transaction.damage);
        Ok(changed)
    }
}

fn desktop<R>(operation: impl FnOnce(&mut Toolbox) -> R) -> Result<R, crate::state::AccessError>
{
    crate::kernel::with_window_toolbox_owned((), |(), toolbox| operation(toolbox))
        .map_err(|(cause, ())| cause)
}

/// Real common maintenance only. Ordinary work cannot inherit a protected
/// interrupted caller's permit/activity during guard-free preparation/enqueue.
pub(crate) fn pump_ordinary_paint()
{
    super::tracking_grow::pump_tracking_grow();
    crate::context::without_recovery(pump_ordinary_paint_inner);
}
fn pump_ordinary_paint_inner()
{
    // Move unpublished custody out before any retirement Drop. It has never
    // admitted an author; taking it cannot be mistaken for a callback ACK.
    let retired = desktop(|toolbox| {
        let valid = toolbox.presentation
                           .ordinary_underlay
                           .transaction
                           .as_ref()
                           .is_some_and(|transaction| transaction.valid(toolbox));
        let Some(transaction) = toolbox.presentation.ordinary_underlay.transaction.as_mut()
        else
        {
            return None;
        };
        let cancelled_canvas = transaction.content
                                          .as_ref()
                                          .and_then(|c| c.election.as_ref())
                                          .is_some_and(Election::cancelled);
        if !valid || cancelled_canvas
        {
            transaction.cancel();
        }
        let elected = if transaction.cancelled
        {
            transaction.content.as_mut().and_then(|c| c.election.take())
        }
        else
        {
            None
        };
        Some((transaction.retired.take(), transaction.rejected.take(), elected))
    });
    let Ok(retired) = retired
    else
    {
        return;
    };
    let returned = retired.as_ref().is_some_and(|(old, rejected, elected)| {
                                       old.is_some() || rejected.is_some() || elected.is_some()
                                   });
    drop(retired);

    let selected = desktop(|toolbox| {
        if let Some(transaction) = toolbox.presentation.ordinary_underlay.transaction.as_mut()
        {
            if let Some(author) = &mut transaction.author
            {
                if author.retirement_failure.is_some()
                {
                    return None;
                }
                if author.stage == AuthorStage::Waiting && !author.owner.ready()
                {
                    return None;
                }
                author.owner.take_ready();
                if author.stage == AuthorStage::Waiting
                {
                    author.stage = AuthorStage::Advance;
                }
                return Some(author.owner.clone());
            }
            if returned || transaction.can_resume()
            {
                toolbox.present();
            }
        }
        None
    });
    let Ok(Some(owner)) = selected
    else
    {
        return;
    };
    if !owner.active()
    {
        // Even the final original account/owner Arc leaves Desktop before Drop.
        let retired = desktop(|toolbox| {
            let transaction = toolbox.presentation.ordinary_underlay.transaction.as_mut()?;
            if !transaction.author.as_ref().is_some_and(|author| author.owner.same(&owner))
            {
                return None;
            }
            let retired = transaction.author.take();
            toolbox.present();
            retired
        });
        drop(retired);
        return;
    }
    let Ok(mut loan) = owner.checkout()
    else
    {
        return;
    };
    if let Err(failure) = service_paint(&owner, &mut loan)
    {
        // A window paint loan refuses with SERVICE_BUSY while its canvas still
        // carries an outstanding geometry claim, and the common Window return is
        // that refusal's retry edge. It is the same not-yet admission as a busy
        // Desktop, so the elected author stays in place and is serviced again
        // when it is ready. Cancelling here discarded an owned definition's
        // whole frame and left the scene stamp frozen.
        let retryable = matches!(failure, ServiceFailure::Desktop(crate::state::AccessError::Busy))
                        || matches!(failure, ServiceFailure::Callback(SERVICE_BUSY));
        if !retryable
        {
            owner.cancel();
            let fresh = desktop(|toolbox| {
                let Some(transaction) = toolbox.presentation.ordinary_underlay.transaction.as_mut()
                else
                {
                    return false;
                };
                if !transaction.author.as_ref().is_some_and(|author| author.owner.same(&owner))
                {
                    return false;
                }
                transaction.cancel();
                if failure.status() != OPERATION_CANCELLED && transaction.failure.is_none()
                {
                    transaction.failure = Some(failure);
                    true
                }
                else
                {
                    false
                }
            });
            if fresh == Ok(true)
            {
                serial!("ORDINARY_AUTHOR_FAILED cause={failure:?}");
            }
            // Actual terminal failure/cancel can retire now. A still-running
            // Pending refuses and remains in SAME owner until its real notice.
            if let Err(cause) = loan.finish()
               && cause.status() != SERVICE_BUSY
            {
                let fresh = desktop(|toolbox| {
                    let Some(transaction) =
                        toolbox.presentation.ordinary_underlay.transaction.as_mut()
                    else
                    {
                        return false;
                    };
                    let Some(author) = transaction.author.as_mut()
                    else
                    {
                        return false;
                    };
                    if !author.owner.same(&owner) || author.retirement_failure.is_some()
                    {
                        return false;
                    }
                    author.retirement_failure = Some(cause);
                    true
                });
                if fresh == Ok(true)
                {
                    serial!("ORDINARY_AUTHOR_RETIRE_RETAINED cause={cause:?}");
                }
            }
        }
    }
    drop(loan);
}

#[derive(Clone, Copy, Debug)]
enum ServiceFailure
{
    Desktop(crate::state::AccessError),
    Callback(Status),
    Control(control_producer::PaintFailure),
    Window(crate::toolbox::windows::manager::paint::PaintFailure)
}
impl From<Status> for ServiceFailure
{
    fn from(status: Status) -> Self
    {
        Self::Callback(status)
    }
}
impl From<control_producer::PaintFailure> for ServiceFailure
{
    fn from(failure: control_producer::PaintFailure) -> Self
    {
        Self::Control(failure)
    }
}
impl From<crate::toolbox::windows::manager::paint::PaintFailure> for ServiceFailure
{
    fn from(failure: crate::toolbox::windows::manager::paint::PaintFailure) -> Self
    {
        Self::Window(failure)
    }
}
impl ServiceFailure
{
    fn status(&self) -> Status
    {
        match self
        {
            Self::Desktop(cause) => cause.status(),
            Self::Callback(status) => *status,
            Self::Control(cause) => cause.status(),
            Self::Window(cause) => cause.status()
        }
    }
}
fn same_transaction(toolbox: &Toolbox, owner: &Owner) -> Result<(), Status>
{
    let transaction = toolbox.presentation.ordinary_underlay.transaction.as_ref().ok_or(BAD_ABI)?;
    if !transaction.author.as_ref().is_some_and(|author| author.owner.same(owner))
    {
        return Err(BAD_ABI);
    }
    Ok(())
}
fn set_stage(toolbox: &mut Toolbox, owner: &Owner, stage: AuthorStage) -> Result<(), Status>
{
    same_transaction(toolbox, owner)?;
    toolbox.presentation
           .ordinary_underlay
           .transaction
           .as_mut()
           .ok_or(BAD_ABI)?
           .author
           .as_mut()
           .ok_or(BAD_ABI)?
           .stage = stage;
    Ok(())
}
fn service_paint(owner: &Owner, loan: &mut Loan) -> Result<(), ServiceFailure>
{
    let (stage, cancelled) =
        desktop(|toolbox| {
            same_transaction(toolbox, owner)?;
            let valid = !toolbox.presentation.redraw
                        && toolbox.presentation
                                  .ordinary_underlay
                                  .transaction
                                  .as_ref()
                                  .is_some_and(|transaction| transaction.valid(toolbox));
            let transaction =
                toolbox.presentation.ordinary_underlay.transaction.as_mut().ok_or(BAD_ABI)?;
            if !valid
            {
                transaction.cancel();
            }
            let stage = transaction.author.as_ref().ok_or(BAD_ABI)?.stage;
            Ok::<_, Status>((stage, transaction.cancelled))
        }).map_err(ServiceFailure::Desktop)??;
    let cancelled = cancelled || loan.cancelled(owner)?;
    if stage == AuthorStage::Retire
    {
        return loan.finish();
    }
    if !cancelled && stage == AuthorStage::Prepare
    {
        loan.prepare()?;
        desktop(|toolbox|set_stage(toolbox,owner,AuthorStage::Capture)).map_err(ServiceFailure::Desktop)??;
    }
    if !cancelled && matches!(stage, AuthorStage::Prepare | AuthorStage::Capture)
    {
        desktop(|toolbox| {
            same_transaction(toolbox, owner)?;
            let transaction =
                toolbox.presentation.ordinary_underlay.transaction.as_ref().ok_or(BAD_ABI)?;
            if !transaction.valid(toolbox)
            {
                return Err(ServiceFailure::Callback(OPERATION_CANCELLED));
            }
            let step = *transaction.steps.get(transaction.next).ok_or(BAD_ABI)?;
            let dimensions = transaction.dimensions;
            let window = *toolbox.windows.window(step.window).ok_or(NOT_FOUND)?;
            let mut canvas = Canvas::new(&mut toolbox.presentation.ordinary_underlay.staging,
                                         dimensions.0, dimensions.1);
            canvas.set_clip(step.clip);
            match loan
            {
                Loan::Window(loan) => toolbox.windows.capture_paint(loan, &canvas)?,
                Loan::Control(loan) =>
                {
                    if !matches!(step.phase, Phase::Content)
                    {
                        return Err(BAD_ABI.into());
                    }
                    canvas.set_origin(window.bounds.left, window.bounds.top);
                    loan.capture(&toolbox.windows, &toolbox.controls, &toolbox.scroll_boxes,
                                 &mut canvas)?;
                }
            }
            set_stage(toolbox, owner, AuthorStage::Advance)?;
            Ok::<_, ServiceFailure>(())
        }).map_err(ServiceFailure::Desktop)??;
    }
    match loan.advance()?
    {
        Progress::Pending =>
        {
            desktop(|toolbox|set_stage(toolbox,owner,AuthorStage::Waiting)).map_err(ServiceFailure::Desktop)??;
            Ok(())
        }
        Progress::Cancelled =>
        {
            desktop(|toolbox| {
                same_transaction(toolbox, owner)?;
                toolbox.presentation
                       .ordinary_underlay
                       .transaction
                       .as_mut()
                       .ok_or(BAD_ABI)?
                       .cancel();
                Ok::<_, Status>(())
            }).map_err(ServiceFailure::Desktop)??;
            loan.finish()
        }
        Progress::Ready =>
        {
            desktop(|toolbox| {
                same_transaction(toolbox, owner)?;
                let transaction =
                    toolbox.presentation.ordinary_underlay.transaction.as_ref().ok_or(BAD_ABI)?;
                if !transaction.valid(toolbox)
                {
                    return Err(ServiceFailure::Callback(OPERATION_CANCELLED));
                }
                let step = *transaction.steps.get(transaction.next).ok_or(BAD_ABI)?;
                let dimensions = transaction.dimensions;
                let window = *toolbox.windows.window(step.window).ok_or(NOT_FOUND)?;
                let mut canvas = Canvas::new(&mut toolbox.presentation.ordinary_underlay.staging,
                                             dimensions.0, dimensions.1);
                canvas.set_clip(step.clip);
                let control = match loan
                {
                    Loan::Window(loan) =>
                    {
                        toolbox.windows.replay_paint(loan, &mut canvas)?;
                        false
                    }
                    Loan::Control(loan) =>
                    {
                        if !matches!(step.phase, Phase::Content)
                        {
                            return Err(BAD_ABI.into());
                        }
                        canvas.set_origin(window.bounds.left, window.bounds.top);
                        loan.replay(&toolbox.windows, &toolbox.controls, &toolbox.scroll_boxes,
                                    &mut canvas)?;
                        true
                    }
                };
                let transaction =
                    toolbox.presentation.ordinary_underlay.transaction.as_mut().ok_or(BAD_ABI)?;
                if control
                {
                    let content = transaction.content.as_mut().ok_or(BAD_ABI)?;
                    if content.step != transaction.next
                    {
                        return Err(BAD_ABI.into());
                    }
                    let resume = content.resume.take().ok_or(BAD_ABI)?;
                    content.cursor.advance_user(resume)?;
                    // The same content step continues at its exact subcall.
                    // Only ContentComplete advances the outer step index.
                }
                else
                {
                    transaction.next = transaction.next.checked_add(1).ok_or(BAD_ABI)?;
                }
                transaction.author.as_mut().ok_or(BAD_ABI)?.stage = AuthorStage::Retire;
                Ok::<_, ServiceFailure>(())
            }).map_err(ServiceFailure::Desktop)??;
            loan.finish()
        }
    }
}

//! Owned User CDEF1 effects for the SAME canonical SBOX. No object/Canvas borrow
//! or user pointer survives a callback wait; logical changes commit once.
#![forbid(unsafe_code)]
use alloc::boxed::Box;
use core::{future::Future,
           pin::Pin,
           task::{Context, Poll, Waker}};

use mac_object_table::{domain::DomainKind, store::CompletionKey};

use super::*;
use crate::{machine::scheduler::accounting::AllocationAuthority,
            toolbox::{controls::effects::{Author, DetachedCall, WaitSlot},
                      resources::definitions::{RetainedDefinition,
                                               callbacks::{CompletionTarget, Pending}}}};

#[derive(Clone, Copy)]
struct Request
{
    operation: u32,
    id:        u64,
    flags:     u32,
    point:     Point,
    ticks:     u64,
    part:      i16
}
impl Request
{
    fn matches(&self, call: &Call) -> bool
    {
        self.operation == call.operation
        && self.id == call.id
        && self.flags == call.flags
        && self.point == call.point
        && self.ticks == call.ticks
        && self.part == call.part
        && call.surface.is_null()
    }
}
struct Readset
{
    key:       CompletionKey,
    window:    u64,
    recipient: OwnerRef,
    storage:   OwnerRef,
    view:      Viewport,
    // Original source reader outlives all viewport readers, with no new bind.
    source:    RetainedDefinition
}
impl Readset
{
    fn validate(&self, manager: &Manager) -> Result<(), Status>
    {
        if !self.recipient.accepting() || !self.storage.accepting()
        {
            return Err(ACCESS_DENIED);
        }
        let object = manager.get(self.key.id(), self.recipient.owner().incarnation())?;
        if manager.objects.completion_key(self.key.id()) != Some(self.key)
           || self.key.owner() != self.storage.owner()
           || object.window != self.window
           || object.recipient_owner != self.recipient.owner()
           || object.view.spec != self.view.spec
           || object.view.top != self.view.top
           || object.view.tracking != self.view.tracking
           || object.view.highlight != self.view.highlight
           || !Arc::ptr_eq(&object.view.raster, &self.view.raster)
           || !self.source.matches(&object.definition)
        {
            return Err(SERVICE_BUSY);
        }
        Ok(())
    }
}
pub(crate) struct Plan
{
    request: Request,
    current: Readset
}
impl Plan
{
    pub(crate) fn window(&self) -> u64
    {
        self.current.window
    }

    pub(crate) fn prepare(self, completion: CompletionTarget, allocation: AllocationAuthority)
                          -> Result<Operation, Status>
    {
        if allocation.owner() != self.current.storage.owner()
        {
            return Err(ACCESS_DENIED);
        }
        let target =
            crate::allocator::Target::Owner { account: allocation.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        crate::allocator::with_target(target, || {
            let wait = WaitSlot::new()?;
            let author = Author::staged(crate::machine::traps::gateway,
                                        completion,
                                        wait.clone(),
                                        allocation.clone());
            let future = Box::try_new(calculate(self, author)).map_err(|_| NO_MEMORY)?;
            Ok(Operation { future: Some(Box::into_pin(future)),
                           wait,
                           allocation })
        })
    }
}
pub(crate) struct Operation
{
    future:     Option<Pin<Box<dyn Future<Output = Result<Prepared, Status>> + Send>>>,
    wait:       Arc<WaitSlot>,
    allocation: AllocationAuthority
}
pub(crate) enum Progress
{
    Wait(Arc<Pending>),
    Ready(Prepared)
}
impl Operation
{
    pub(crate) fn pending(&self) -> Option<Arc<Pending>>
    {
        self.wait.pending()
    }

    pub(crate) fn advance(&mut self) -> Result<Progress, Status>
    {
        let target =
            crate::allocator::Target::Owner { account: self.allocation.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        crate::allocator::with_target(target, || {
            if let Some(pending) = self.wait.pending().filter(|pending| !pending.ready())
            {
                return Ok(Progress::Wait(pending));
            }
            let future = self.future.as_mut().ok_or(BAD_ARGUMENT)?;
            match future.as_mut().poll(&mut Context::from_waker(Waker::noop()))
            {
                Poll::Ready(result) =>
                {
                    drop(self.future.take());
                    result.map(Progress::Ready)
                }
                Poll::Pending => self.wait.pending().map(Progress::Wait).ok_or(BAD_ABI)
            }
        })
    }
}
pub(crate) struct Prepared
{
    plan:      Plan,
    next:      Option<Viewport>,
    bar:       Option<super::super::Drawing>,
    drawing:   Option<Drawing>,
    committed: bool,
    output:    Output
}
impl Prepared
{
    pub(crate) fn window(&self) -> u64
    {
        self.plan.current.window
    }

    pub(crate) fn drawing(&mut self) -> &mut Option<Drawing>
    {
        &mut self.drawing
    }

    pub(crate) fn is_draw(&self) -> bool
    {
        self.plan.request.operation == DRAW
    }

    pub(crate) fn matches(&self, call: &Call) -> bool
    {
        self.plan.request.matches(call)
    }

    /// Only after the common retained publisher consumed the staged Drawing.
    /// Admission checked all fallible conditions in the same Desktop turn.
    pub(crate) fn finish_draw(&mut self, call: &mut Call)
    {
        self.output.write(call);
        self.committed = true;
    }
}
async fn hit(author: &Author, reference: DefinitionRef, state: ScrollbarState, point: Point)
             -> Result<DetachedCall, Status>
{
    let call = DetachedCall::capture(&ControlCall { operation: HIT_SCROLLBAR,
                                                    scrollbar: state,
                                                    point,
                                                    ..Default::default() })?;
    author.prepare(call, reference, None).await.map(|(call, _)| call)
}
async fn calculate(plan: Plan, author: Author) -> Result<Prepared, Status>
{
    if !plan.current.recipient.accepting() || !plan.current.storage.accepting()
    {
        return Err(ACCESS_DENIED);
    }
    let mut view = plan.current.view.clone();
    let reference = plan.current.source.reference();
    let mut part = plan.request.part;
    let mut bar = None;
    match plan.request.operation
    {
        DRAW =>
        {
            let call = DetachedCall::capture(&ControlCall { operation: DRAW_SCROLLBAR,
                                                            scrollbar: view.state(),
                                                            ..Default::default() })?;
            let (_, drawing) = author.prepare(call, reference, None).await?;
            bar = Some(drawing.ok_or(BAD_ABI)?);
        }
        TRACK_BEGIN =>
        {
            let call = hit(&author, reference, view.state(), plan.request.point).await?;
            part = i16::try_from(call.result).map_err(|_| BAD_ABI)?;
            if matches!(part,
                        SCROLL_UP | SCROLL_DOWN | SCROLL_PAGE_UP | SCROLL_PAGE_DOWN | SCROLL_THUMB)
            {
                if part != SCROLL_THUMB
                {
                    step(&mut view, part)?;
                }
                view.tracking = Some(Tracking::new(part, plan.request.point,
                                                   call.scrollbar.value, plan.request.ticks));
                view.highlight = part;
            }
        }
        TRACK_POLL =>
        {
            let tracking = view.tracking.ok_or(BAD_ABI)?;
            let state = view.state();
            let author_ref = &author;
            let transition =
                tracking.poll_with(plan.request.point,
                                   plan.request.flags & BUTTON_DOWN != 0,
                                   plan.request.ticks,
                                   Some(state),
                                   0,
                                   reference,
                                   &author,
                                   |point| {
                                       async move {
                                           let call =
                                               hit(author_ref, reference, state, point).await?;
                                           i16::try_from(call.result).map_err(|_| BAD_ABI)
                                       }
                                   })
                        .await?;
            apply_transition(&mut view, transition)?;
        }
        _ => return Err(BAD_ARGUMENT)
    }
    let output = Output::of(plan.current.window, &view, plan.current.view.top,
                            plan.current.view.highlight, part);
    Ok(Prepared { plan,
                  next: Some(view),
                  bar,
                  drawing: None,
                  committed: false,
                  output })
}
impl Manager
{
    pub(crate) fn validate_user_prepared(&self, prepared: &Prepared) -> Result<(), Status>
    {
        prepared.plan.current.validate(self)
    }

    /// Pure snapshot admission. Actual System/recovery User authors are an
    /// explicit unsupported policy; they never fall into ordinary accounting.
    pub(crate) fn plan_user_operation(&self, recipient: &OwnerRef, storage: &OwnerRef,
                                      call: &Call, fonts: &FontManager)
                                      -> Result<Option<Plan>, Status>
    {
        if !matches!(call.operation, DRAW | TRACK_BEGIN | TRACK_POLL)
        {
            return Ok(None);
        }
        let entry = self.preflight_definition(recipient, storage, call, fonts)?.ok_or(NOT_FOUND)?;
        if !crate::toolbox::resources::definitions::is_user(entry)?
        {
            return Ok(None);
        }
        if !call.surface.is_null()
        {
            return Err(if call.operation == DRAW { NOT_SUPPORTED } else { BAD_ARGUMENT });
        }
        let object = self.get(call.id, recipient.owner().incarnation())?;
        if storage.owner().kind() != DomainKind::Process || object.recovery
        {
            return Err(NOT_SUPPORTED);
        }
        if object.recipient_owner != recipient.owner()
           || self.objects.owner(call.id) != Some(storage.owner())
        {
            return Err(ACCESS_DENIED);
        }
        if call.operation == TRACK_BEGIN && object.view.tracking.is_some()
        {
            return Err(SERVICE_BUSY);
        }
        if call.operation == DRAW && !object.view.missing().is_empty()
        {
            return Err(SERVICE_BUSY);
        }
        if call.operation == TRACK_POLL
           && object.view.tracking.is_none_or(|t| !t.needs_author(call.point))
        {
            return Ok(None);
        }
        let key = self.objects.completion_key(call.id).ok_or(NOT_FOUND)?;
        Ok(Some(Plan { request: Request { operation: call.operation,
                                          id:        call.id,
                                          flags:     call.flags,
                                          point:     call.point,
                                          ticks:     call.ticks,
                                          part:      call.part },
                       current: Readset { key,
                                          window: object.window,
                                          recipient: recipient.clone(),
                                          storage: storage.clone(),
                                          view: object.view.clone(),
                                          source: object.definition.retain_source() } }))
    }

    /// Stamps a real validated Drawing without resident state effects. On any
    /// refusal the SAME prepared result remains with its original native call.
    pub(crate) fn stage_user_drawing(&self, prepared: &mut Prepared, call: &Call)
                                     -> Result<(), Status>
    {
        if !prepared.matches(call)
        {
            return Err(BAD_ARGUMENT);
        }
        prepared.plan.current.validate(self)?;
        if prepared.committed || !prepared.is_draw()
        {
            return Err(BAD_ARGUMENT);
        }
        if prepared.drawing.is_some()
        {
            return Ok(());
        }
        let object = self.objects.get(prepared.plan.current.key.id()).ok_or(NOT_FOUND)?;
        let bar = prepared.bar.as_ref().ok_or(BAD_ABI)?;
        if !bar.source.matches(&object.definition)
        {
            return Err(BAD_ABI);
        }
        let bar = prepared.bar.take().ok_or(BAD_ABI)?;
        prepared.drawing =
            Some(Drawing { id: prepared.plan.current.key.id(),
                           origin: ScrollBoxPaintOrigin::publication(prepared.plan.current.key,
                                                                     object),
                           bounds: object.view.spec.viewport,
                           raster: object.view.raster.clone(),
                           bar,
                           #[cfg(systemrisc)]
                           _recovery_origin: object._recovery_origin.clone() });
        Ok(())
    }

    /// All key/source/readset checks precede infallible scalar/Arc swaps. Old
    /// actual viewport readers return to the caller for guard-free destruction.
    pub(crate) fn commit_user_operation(&mut self, prepared: &mut Prepared, call: &mut Call)
                                        -> Result<ViewportRetired, Status>
    {
        if prepared.committed || !prepared.matches(call)
        {
            return Err(BAD_ARGUMENT);
        }
        prepared.plan.current.validate(self)?;
        if prepared.is_draw()
        {
            return Err(BAD_ARGUMENT);
        }
        if prepared.next.is_none()
        {
            return Err(BAD_ABI);
        }
        let object = self.objects.get_mut(prepared.plan.current.key.id()).ok_or(NOT_FOUND)?;
        let next = prepared.next.take().ok_or(BAD_ABI)?;
        let retired = Some(core::mem::replace(&mut object.view, next));
        prepared.output.write(call);
        prepared.committed = true;
        Ok(ViewportRetired { _viewport: retired })
    }
}
#[must_use = "drop original viewport readers outside Desktop"]
pub(crate) struct ViewportRetired
{
    // Actual old raster/source owners must leave Desktop before destruction.
    _viewport: Option<Viewport>
}
struct Output
{
    window: u64,
    spec:   Spec,
    top:    u32,
    first:  u32,
    end:    u32,
    flags:  u32,
    part:   i16
}
impl Output
{
    fn of(window: u64, view: &Viewport, old_top: u32, old_highlight: i16, part: i16) -> Self
    {
        let missing = view.missing();
        Self { window,
               spec: view.spec,
               top: view.top,
               first: missing.start,
               end: missing.end,
               flags: u32::from(view.tracking.is_some()) * TRACKING
                      | u32::from(view.top != old_top || view.highlight != old_highlight)
                        * CHANGED,
               part }
    }

    fn write(&mut self, call: &mut Call)
    {
        call.window = self.window;
        call.spec = self.spec;
        call.top = self.top;
        call.first = self.first;
        call.end = self.end;
        call.flags = self.flags;
        call.part = self.part;
    }
}

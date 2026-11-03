//! Definition effects for the same owned Control Manager calculations. Only an
//! actual broker Pending may suspend; no manager/Canvas/executor borrow escapes.
use alloc::sync::Arc;
use core::{future::{Future, poll_fn},
           pin::pin,
           task::{Context, Poll, Waker}};

use spinning_top::Spinlock;

use super::*;
use crate::toolbox::resources::definitions::{self,
                                             callbacks::{self, CompletionTarget, Pending,
                                                         packet::{Arguments, Object, Reply,
                                                                  Request}}};

/// Manager-owned call state. Surface pointers exist only in the synchronous ABI
/// adapter, never in an owned continuation or across an await.
#[derive(Clone, Copy)]
pub(super) struct DetachedCall
{
    pub(super) id:        u64,
    pub(super) relation:  mac_abi::controls::reflow::Relation,
    pub(super) reflow:    u32,
    pub(super) layout:    mac_abi::controls::reflow::Payload,
    pub(super) flags:     u32,
    pub(super) ticks:     u64,
    pub(super) step:      i16,
    pub(super) page:      i16,
    pub(super) operation: u32,
    pub(super) window:    u64,
    pub(super) scrollbar: ScrollbarState,
    pub(super) point:     mac_abi::geometry::Point,
    pub(super) anchor:    mac_abi::geometry::Point,
    pub(super) result:    i32,
    pub(super) standard:  ControlDefinitionState,
    pub(super) procedure: i16,
    pub(super) reserved:  i16
}
impl DetachedCall
{
    pub(super) fn capture(call: &ControlCall) -> Result<Self, Status>
    {
        if !call.surface.is_null()
        {
            return Err(BAD_ARGUMENT);
        }
        Ok(Self { id:        call.id,
                  relation:  call.relation,
                  reflow:    call.reflow,
                  layout:    call.layout,
                  flags:     call.flags,
                  ticks:     call.ticks,
                  step:      call.step,
                  page:      call.page,
                  operation: call.operation,
                  window:    call.window,
                  scrollbar: call.scrollbar,
                  point:     call.point,
                  anchor:    call.anchor,
                  result:    call.result,
                  standard:  call.standard,
                  procedure: call.procedure,
                  reserved:  call.reserved })
    }

    pub(super) fn wire(self) -> ControlCall
    {
        ControlCall { id:        self.id,
                      relation:  self.relation,
                      reflow:    self.reflow,
                      layout:    self.layout,
                      flags:     self.flags,
                      ticks:     self.ticks,
                      step:      self.step,
                      page:      self.page,
                      operation: self.operation,
                      window:    self.window,
                      scrollbar: self.scrollbar,
                      point:     self.point,
                      anchor:    self.anchor,
                      result:    self.result,
                      standard:  self.standard,
                      procedure: self.procedure,
                      reserved:  self.reserved,
                      surface:   core::ptr::null_mut() }
    }
}

pub(super) struct WaitSlot(Spinlock<Option<Arc<Pending>>>);
impl WaitSlot
{
    pub(super) fn new() -> Result<Arc<Self>, Status>
    {
        Arc::try_new(Self(Spinlock::new(None))).map_err(|_| NO_MEMORY)
    }

    pub(super) fn pending(&self) -> Option<Arc<Pending>>
    {
        self.0.lock().clone()
    }

    fn register(&self, pending: &Arc<Pending>) -> Result<(), Status>
    {
        let mut slot = self.0.lock();
        if let Some(previous) = slot.as_ref()
        {
            if !Arc::ptr_eq(previous, pending)
            {
                return Err(BAD_ABI);
            }
        }
        else
        {
            *slot = Some(pending.clone());
        }
        Ok(())
    }

    fn clear(&self, pending: &Arc<Pending>) -> Result<(), Status>
    {
        let retired = {
            let mut slot = self.0.lock();
            if !slot.as_ref().is_some_and(|current| Arc::ptr_eq(current, pending))
            {
                return Err(BAD_ABI);
            }
            slot.take()
        };
        drop(retired);
        Ok(())
    }
}

pub(super) struct Author
{
    gate:         TrapGate,
    asynchronous: Option<(CompletionTarget,
                          Arc<WaitSlot>,
                          crate::machine::scheduler::accounting::AllocationAuthority)>
}
impl Author
{
    pub(super) fn trusted(gate: TrapGate) -> Self
    {
        Self { gate,
               asynchronous: None }
    }

    pub(super) fn staged(gate: TrapGate, completion: CompletionTarget, wait: Arc<WaitSlot>,
                         allocation: crate::machine::scheduler::accounting::AllocationAuthority)
                         -> Self
    {
        Self { gate,
               asynchronous: Some((completion, wait, allocation)) }
    }

    async fn invoke(&self, reference: DefinitionRef, request: Request) -> Result<Reply, Status>
    {
        let (completion, wait, allocation) = self.asynchronous.as_ref().ok_or(SERVICE_BUSY)?;
        let pending = callbacks::enqueue_shared_for_object(reference,
                                                           request,
                                                           completion.clone(),
                                                           allocation).map_err(callbacks::EnqueueError::status)?;
        wait.register(&pending)?;
        let reply = poll_fn(|_| {
                        match pending.poll()
                        {
                            Ok(Some(reply)) => Poll::Ready(Ok(reply)),
                            Ok(None) => Poll::Pending,
                            Err(status) => Poll::Ready(Err(status))
                        }
                    }).await;
        wait.clear(&pending)?;
        reply
    }

    pub(super) async fn reflow(&self, call: DetachedCall,
                               payload: mac_abi::controls::reflow::Payload,
                               reference: DefinitionRef)
                               -> Result<mac_abi::controls::reflow::Payload, Status>
    {
        if !definitions::is_user(reference)?
        {
            let mut wire = call.wire();
            let mut payload = payload;
            super::reflow::invoke(&mut wire, &mut payload, reference, self.gate)?;
            return Ok(payload);
        }
        let id = definition_id(&call.wire())?;
        let reply = self.invoke(reference,
                                Request { kind: CDEF,
                                          id,
                                          variation: variation(call.procedure, id),
                                          message: payload.message,
                                          reflow: 1,
                                          object: if id == 1
                                          {
                                              Object::Scrollbar(call.scrollbar)
                                          }
                                          else
                                          {
                                              Object::Control(call.standard)
                                          },
                                          arguments: Arguments::Layout(payload),
                                          surface: None })
                        .await?;
        zero(reply.result)?;
        reply.layout()
    }

    pub(super) async fn grid_axis(&self, reference: DefinitionRef,
                                  constraint: mac_abi::controls::reflow::AxisConstraint,
                                  percent_basis: Option<i64>,
                                  tracks: &mut [mac_abi::controls::reflow::GridTrack],
                                  items: &mut [mac_abi::controls::reflow::GridItem])
                                  -> Result<(i64, i64), Status>
    {
        if !definitions::is_user(reference)?
        {
            return super::reflow::grid_axis(reference, self.gate, constraint, percent_basis,
                                            tracks, items);
        }
        let copy = |values: &[_]| {
            let mut owned = alloc::vec::Vec::new();
            owned.try_reserve_exact(values.len()).map_err(|_| NO_MEMORY)?;
            owned.extend_from_slice(values);
            Ok::<_, Status>(owned)
        };
        let mut owned_tracks = alloc::vec::Vec::new();
        owned_tracks.try_reserve_exact(tracks.len()).map_err(|_| NO_MEMORY)?;
        owned_tracks.extend_from_slice(tracks);
        let owned_items = copy(items)?;
        let reply =
            self.invoke(reference,
                        Request { kind:      CDEF,
                                  id:        CDEF_TABLE,
                                  variation: 0,
                                  message:   mac_abi::controls::reflow::CALCULATE_GRID_AXIS,
                                  reflow:    1,
                                  object:    Object::Control(ControlDefinitionState::default()),
                                  arguments: Arguments::GridAxis { constraint,
                                                                   percent_basis:
                                                                       percent_basis.unwrap_or(-1),
                                                                   tracks: owned_tracks,
                                                                   items: owned_items },
                                  surface:   None })
                .await?;
        zero(reply.result)?;
        let (minimum, preferred) = reply.grid_axis(tracks, items)?;
        super::reflow::validate_axis(minimum, preferred, constraint, tracks, items)
    }

    pub(super) async fn change(&self, reference: DefinitionRef, reflow: u32,
                               mut change: ControlChange)
                               -> Result<ControlChange, Status>
    {
        if !definitions::is_user(reference)?
        {
            let mut call = DefinitionCall { header: Header::for_type::<DefinitionCall>(),
                                            kind: CDEF,
                                            id: CDEF_PROGRESS_BAR,
                                            variation: 0,
                                            message: CHANGE_BOUNDS_DEFINITION,
                                            reserved: 0,
                                            param: 0,
                                            reflow,
                                            object: (&mut change as *mut ControlChange).cast(),
                                            surface: core::ptr::null_mut(),
                                            gate: self.gate };
            let result = definitions::invoke(reference, &mut call);
            if result < 0
            {
                return Err(result as Status);
            }
            return Ok(change);
        }
        let reply = self.invoke(reference,
                                Request { kind: CDEF,
                                          id: CDEF_PROGRESS_BAR,
                                          variation: 0,
                                          message: CHANGE_BOUNDS_DEFINITION,
                                          reflow,
                                          object: Object::Change(change),
                                          arguments: Arguments::Scalar(0),
                                          surface: None })
                        .await?;
        if reply.result < 0
        {
            return Err(Status::try_from(reply.result).unwrap_or(BAD_ABI));
        }
        reply.change()
    }

    pub(super) async fn validate(&self, call: DetachedCall, reference: DefinitionRef)
                                 -> Result<DetachedCall, Status>
    {
        if !definitions::is_user(reference)?
        {
            let mut wire = call.wire();
            super::validate(&mut wire, reference, self.gate)?;
            return DetachedCall::capture(&wire);
        }
        let request = user_validation_request(&call.wire())?;
        let reply = self.invoke(reference, request).await?;
        DetachedCall::capture(&apply_user_validation(&call.wire(), &reply)?)
    }

    pub(super) async fn prepare(&self, mut call: DetachedCall, reference: DefinitionRef,
                                title: Option<&Arc<snapshot::Title>>)
                                -> Result<(DetachedCall, Option<Drawing>), Status>
    {
        if !definitions::is_user(reference)?
        {
            let mut wire = call.wire();
            let drawing = super::prepare_with_title(&mut wire, reference, self.gate, title)?;
            return Ok((DetachedCall::capture(&wire)?, drawing));
        }
        let id = preflight(&call.wire())?;
        if matches!(call.operation, DRAW_SCROLLBAR | DRAW_STANDARD)
        {
            call = self.validate(call, reference).await?;
            let drawing = super::drawing_validated(&call.wire(), reference, self.gate, title)?;
            return Ok((call, Some(drawing)));
        }
        validate_state(&call.wire(), id)?;
        let mut bits = (call.point.h as u16 as u64) | ((call.point.v as u16 as u64) << 16);
        let tracking = matches!(call.operation, TRACK_SCROLLBAR | TRACK_SPLITTER);
        if tracking
        {
            bits |= ((call.anchor.h as u16 as u64) << 32) | ((call.anchor.v as u16 as u64) << 48);
        }
        let reply = self.invoke(reference,
                                Request { kind: CDEF,
                                          id,
                                          variation: variation(call.procedure, id),
                                          message: if tracking { 2 } else { 1 },
                                          reflow: call.reflow,
                                          object: if id == 1
                                          {
                                              Object::Scrollbar(call.scrollbar)
                                          }
                                          else
                                          {
                                              Object::Control(call.standard)
                                          },
                                          arguments: Arguments::Scalar(bits as i64),
                                          surface: None })
                        .await?;
        if reply.result < 0
        {
            return Err(Status::try_from(reply.result).unwrap_or(BAD_ABI));
        }
        if id == 1
        {
            call.scrollbar = reply.scrollbar()?;
        }
        else
        {
            call.standard = reply.control()?;
        }
        call.result = i32::try_from(reply.result).map_err(|_| BAD_ABI)?;
        Ok((call, None))
    }
}

fn zero(result: i64) -> Result<(), Status>
{
    if result == 0
    {
        Ok(())
    }
    else if result < 0
    {
        Err(Status::try_from(result).unwrap_or(BAD_ABI))
    }
    else
    {
        Err(BAD_ABI)
    }
}

/// The trusted facade performs no asynchronous effects. Poll once on the stack;
/// a Process definition cannot be run here and remains a typed staged request.
pub(super) fn immediate<T>(future: impl Future<Output = Result<T, Status>>) -> Result<T, Status>
{
    let mut future = pin!(future);
    match future.as_mut().poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => Err(BAD_ABI)
    }
}

//! Owned drawing to the authentic checked caller Surface. This has no Window
//! or CTRL identity; SBOX keeps its independent genuine optimistic readset.
#![forbid(unsafe_code)]
use alloc::{boxed::Box, sync::Arc};
use core::{future::Future,
           pin::Pin,
           task::{Context, Poll, Waker}};

use mac_abi::{controls::*, scrollbox as sb, *};

use super::{effects::{Author, DetachedCall, WaitSlot},
            paint::{self,
                    producer::{PaintOwner, PaintProgress}}};
use crate::{machine::{scheduler::accounting::AllocationAuthority,
                      traps::user::surface::{Loan, Staging}},
            toolbox::resources::definitions::{RetainedDefinition,
                                              callbacks::{CompletionTarget, Notice}}};

enum Kind
{
    Control(DetachedCall),
    ScrollBox
    {
        id:    u64,
        flags: u32,
        point: mac_abi::geometry::Point,
        ticks: u64,
        part:  i16
    }
}
pub(crate) struct Input(Kind);
impl Input
{
    pub(crate) fn control(call: &ControlCall) -> Result<Self, Status>
    {
        if !matches!(call.operation, DRAW_STANDARD | DRAW_SCROLLBAR) || call.surface.is_null()
        {
            return Err(BAD_ARGUMENT);
        }
        let mut plain = *call;
        plain.surface = core::ptr::null_mut();
        super::preflight(&plain)?;
        Ok(Self(Kind::Control(DetachedCall::capture(&plain)?)))
    }

    pub(crate) fn scroll_box(call: &sb::Call) -> Result<Self, Status>
    {
        if call.operation != sb::DRAW || call.surface.is_null()
        {
            return Err(BAD_ARGUMENT);
        }
        Ok(Self(Kind::ScrollBox { id:    call.id,
                                  flags: call.flags,
                                  point: call.point,
                                  ticks: call.ticks,
                                  part:  call.part }))
    }

    pub(crate) fn control_call(&self) -> Option<ControlCall>
    {
        match &self.0
        {
            Kind::Control(c) => Some(c.wire()),
            _ => None
        }
    }

    pub(crate) fn scroll_box_call(&self) -> Option<sb::Call>
    {
        match self.0
        {
            Kind::ScrollBox { id,
                              flags,
                              point,
                              ticks,
                              part } =>
            {
                Some(sb::Call { operation: sb::DRAW,
                                id,
                                flags,
                                point,
                                ticks,
                                part,
                                ..Default::default() })
            }
            _ => None
        }
    }

    pub(crate) fn admit_control(self, source: RetainedDefinition, allocation: AllocationAuthority)
                                -> Result<Admission, Status>
    {
        if !matches!(self.0, Kind::Control(_))
        {
            return Err(BAD_ABI);
        }
        Ok(Admission { input: self,
                       kind: Initial::Control(source),
                       allocation })
    }

    pub(crate) fn admit_scroll_box(self, plan: super::scrollbox::callbacks::Plan,
                                   allocation: AllocationAuthority)
                                   -> Result<Admission, Status>
    {
        if !matches!(self.0, Kind::ScrollBox { .. })
        {
            return Err(BAD_ABI);
        }
        Ok(Admission { input: self,
                       kind: Initial::ScrollBox(plan),
                       allocation })
    }
}
enum Initial
{
    Control(RetainedDefinition),
    ScrollBox(super::scrollbox::callbacks::Plan)
}
pub(crate) struct Admission
{
    input:      Input,
    kind:       Initial,
    allocation: AllocationAuthority
}
impl Admission
{
    pub(crate) fn allocation(&self) -> &AllocationAuthority
    {
        &self.allocation
    }

    pub(crate) fn start(self, mut loan: Loan) -> Result<Operation, Status>
    {
        if loan.origin().owner() != self.allocation.owner()
        {
            return Err(ACCESS_DENIED);
        }
        let target =
            crate::allocator::Target::Owner { account: self.allocation.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        crate::allocator::with_target(target, || {
            let notice = Notice::new()?;
            loan.origin().observe(notice.clone())?;
            let staging = loan.prepare_staging()?;
            let waiter = loan.origin().thread();
            let validation = match self.kind
            {
                Initial::Control(source) =>
                {
                    let call = self.input.control_call().ok_or(BAD_ABI)?;
                    let detached = DetachedCall::capture(&call)?;
                    let wait = WaitSlot::new()?;
                    let author = Author::staged(crate::machine::traps::gateway,
                                                CompletionTarget::Thread(waiter),
                                                wait.clone(),
                                                self.allocation.clone());
                    let future = Box::try_new(async move {
                                     let result =
                                         author.prepare(detached, source.reference(), None).await?;
                                     drop(source);
                                     Ok((result.0, result.1.ok_or(BAD_ABI)?))
                                 }).map_err(|_| NO_MEMORY)?;
                    Validation::Control { future: Some(Box::into_pin(future)),
                                          wait }
                }
                Initial::ScrollBox(plan) =>
                {
                    Validation::ScrollBox(plan.prepare(CompletionTarget::Thread(waiter),
                                                       self.allocation.clone())?)
                }
            };
            Ok(Operation { input: self.input,
                           validation: Some(validation),
                           ready: None,
                           draw: None,
                           cursor: Cursor::Control(paint::Cursor::default()),
                           elected: None,
                           owner: None,
                           resume: None,
                           captured: false,
                           allocation: self.allocation,
                           notice,
                           staging,
                           loan,
                           desktop_wait: None,
                           done: false })
        })
    }
}
enum Validation
{
    Control
    {
        future: Option<Pin<Box<dyn Future<Output = Result<(DetachedCall, super::Drawing), Status>>
                                   + Send>>>,
        wait:   Arc<WaitSlot>
    },
    ScrollBox(super::scrollbox::callbacks::Operation)
}
enum Ready
{
    Control(DetachedCall),
    ScrollBox(super::scrollbox::callbacks::Prepared)
}
enum Draw
{
    Control(super::Drawing),
    ScrollBox(super::scrollbox::Drawing)
}
enum Cursor
{
    Control(paint::Cursor),
    ScrollBox(super::scrollbox::PaintCursor)
}
/// All source/scratch/packet readers precede authentic caller-buffer custody.
/// Dropping a stopped Call never exposes private staging as output.
pub(crate) struct Operation
{
    input:        Input,
    validation:   Option<Validation>,
    ready:        Option<Ready>,
    draw:         Option<Draw>,
    cursor:       Cursor,
    // Original elected subcall survives only genuine Desktop contention.
    // Snapshot/cursor position grants no account or object authority.
    elected:      Option<paint::PaintPlan>,
    owner:        Option<Arc<PaintOwner>>,
    resume:       Option<paint::Resume>,
    captured:     bool,
    allocation:   AllocationAuthority,
    notice:       Arc<Notice>,
    staging:      Staging,
    loan:         Loan,
    desktop_wait: Option<crate::kernel::DesktopEpoch>,
    done:         bool
}
impl Operation
{
    pub(crate) fn advance(&mut self) -> Result<Option<Status>, Status>
    {
        self.desktop_wait = None;
        if self.done
        {
            return Err(BAD_ARGUMENT);
        }
        let target =
            crate::allocator::Target::Owner { account: self.allocation.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        crate::allocator::with_target(target, || self.advance_owned())
    }

    fn advance_owned(&mut self) -> Result<Option<Status>, Status>
    {
        if self.loan.origin().cancelled()
        {
            return Err(OPERATION_CANCELLED);
        }
        if let Some(validation) = &mut self.validation
        {
            match validation
            {
                Validation::Control { future,
                                      wait } =>
                {
                    if wait.pending().is_some_and(|p| !p.ready())
                    {
                        return Ok(None);
                    }
                    let result = future.as_mut()
                                       .ok_or(BAD_ABI)?
                                       .as_mut()
                                       .poll(&mut Context::from_waker(Waker::noop()));
                    match result
                    {
                        Poll::Pending => return Ok(None),
                        Poll::Ready(result) =>
                        {
                            let (call, draw) = result?;
                            self.ready = Some(Ready::Control(call));
                            self.draw = Some(Draw::Control(draw));
                        }
                    }
                }
                Validation::ScrollBox(operation) =>
                {
                    match operation.advance()?
                    {
                        super::scrollbox::callbacks::Progress::Wait(_) => return Ok(None),
                        super::scrollbox::callbacks::Progress::Ready(prepared) =>
                        {
                            self.ready = Some(Ready::ScrollBox(prepared));
                            self.cursor = Cursor::ScrollBox(Default::default());
                        }
                    }
                }
            }
            drop(self.validation.take());
        }
        if self.draw.is_none()
        {
            let call = self.input.scroll_box_call().ok_or(BAD_ABI)?;
            let Some(Ready::ScrollBox(prepared)) = &mut self.ready
            else
            {
                return Err(BAD_ABI);
            };
            let epoch = crate::kernel::window_desktop_epoch();
            let result = crate::toolbox::runtime::offscreen_toolbox(|toolbox| {
                toolbox.offscreen_stage_scroll_box(prepared, &call)
            });
            let draw = match result
            {
                Ok(result) => result?,
                Err(crate::state::AccessError::Busy) =>
                {
                    self.desktop_wait = Some(epoch);
                    return Ok(None);
                }
                Err(cause) => return Err(cause.status())
            };
            self.draw = Some(Draw::ScrollBox(draw));
        }
        loop
        {
            if let Some(owner) = self.owner.clone()
            {
                let mut loan = owner.checkout()?;
                if !self.captured
                {
                    loan.prepare().map_err(|f| f.status())?;
                    let epoch = crate::kernel::window_desktop_epoch();
                    let result =
                        self.staging.with_canvas(|canvas| {
                                         crate::toolbox::runtime::offscreen_toolbox(|toolbox| {
                                             toolbox.offscreen_capture(&mut loan, canvas)
                                         })
                                     })?;
                    match result
                    {
                        Ok(result) => result?,
                        Err(crate::state::AccessError::Busy) =>
                        {
                            self.desktop_wait = Some(epoch);
                            return Ok(None);
                        }
                        Err(cause) => return Err(cause.status())
                    }
                    self.captured = true;
                }
                match loan.advance()
                {
                    PaintProgress::Pending => return Ok(None),
                    PaintProgress::Cancelled =>
                    {
                        loan.finish()?;
                        return Err(OPERATION_CANCELLED);
                    }
                    PaintProgress::Failed(cause) =>
                    {
                        loan.finish()?;
                        return Err(cause.status());
                    }
                    PaintProgress::Ready => ()
                }
                let epoch = crate::kernel::window_desktop_epoch();
                let result =
                    self.staging.with_canvas(|canvas| {
                                     crate::toolbox::runtime::offscreen_toolbox(|toolbox| {
                                         toolbox.offscreen_replay(&mut loan, canvas)
                                     })
                                 })?;
                match result
                {
                    Ok(result) => result?,
                    Err(crate::state::AccessError::Busy) =>
                    {
                        self.desktop_wait = Some(epoch);
                        return Ok(None);
                    }
                    Err(cause) => return Err(cause.status())
                };
                loan.finish()?;
                drop(loan);
                drop(self.owner.take());
                let resume = self.resume.take().ok_or(BAD_ABI)?;
                match &mut self.cursor
                {
                    Cursor::Control(c) => c.advance_user(resume)?,
                    Cursor::ScrollBox(c) => c.advance_user(resume)?
                }
            }
            let mut plan = if let Some(plan) = self.elected.take()
            {
                plan
            }
            else
            {
                let progress = self.staging.with_canvas(|canvas| {
                                                 match (self.draw.as_ref().ok_or(BAD_ABI)?,
                                                        &mut self.cursor)
                                                 {
                                                     (Draw::Control(d), Cursor::Control(c)) =>
                                                     {
                                                         d.paint_step(c, canvas)
                                                     }
                                                     (Draw::ScrollBox(d), Cursor::ScrollBox(c)) =>
                                                     {
                                                         d.paint_step(c, canvas)
                                                     }
                                                     _ => Err(BAD_ABI)
                                                 }
                                             })??;
                match progress
                {
                    paint::Progress::Complete =>
                    {
                        if let Some(Ready::ScrollBox(prepared)) = &self.ready
                        {
                            let epoch = crate::kernel::window_desktop_epoch();
                            match crate::toolbox::runtime::offscreen_toolbox(|toolbox| {
                                      toolbox.offscreen_validate_scroll_box(prepared)
                                  })
                            {
                                Ok(result) => result?,
                                Err(crate::state::AccessError::Busy) =>
                                {
                                    self.desktop_wait = Some(epoch);
                                    return Ok(None);
                                }
                                Err(cause) => return Err(cause.status())
                            }
                        }
                        self.done = true;
                        return Ok(Some(OK));
                    }
                    paint::Progress::User(plan) => plan
                }
            };
            let epoch = crate::kernel::window_desktop_epoch();
            let claim = match crate::toolbox::runtime::offscreen_toolbox(|toolbox| {
                      toolbox.offscreen_paint_claim(&plan)
                  })
            {
                Ok(result) => result?,
                Err(crate::state::AccessError::Busy) =>
                {
                    // Keep SAME original elected plan and waiting cursor.
                    // A service/object Busy below remains a terminal refusal.
                    self.elected = Some(plan);
                    self.desktop_wait = Some(epoch);
                    return Ok(None);
                }
                Err(cause) => return Err(cause.status())
            };
            self.resume = Some(plan.take_resume()?);
            let owner =
                PaintOwner::prepare_surface(plan,
                                            self.loan.origin(),
                                            claim,
                                            self.allocation.clone(),
                                            self.notice.clone()).map_err(|(failure, inputs)| {
                                                                    drop(inputs);
                                                                    failure.status()
                                                                })?;
            self.owner = Some(owner.clone());
            self.captured = false;
        }
    }

    pub(crate) fn wait(&self) -> Result<(), Status>
    {
        if let Some(epoch) = self.desktop_wait
        {
            return crate::machine::scheduler::callbacks::wait_desktop(epoch).map(|_| ());
        }
        if let Some(validation) = &self.validation
        {
            return match validation
            {
                Validation::Control { wait,
                                      .. } =>
                {
                    let pending = wait.pending().ok_or(BAD_ABI)?;
                    crate::machine::scheduler::callbacks::wait_call(&pending).map(|_| ())
                }
                Validation::ScrollBox(operation) =>
                {
                    let pending = operation.pending().ok_or(BAD_ABI)?;
                    crate::machine::scheduler::callbacks::wait_call(&pending).map(|_| ())
                }
            };
        }
        let owner = self.owner.as_ref().ok_or(BAD_ABI)?;
        let loan = owner.checkout()?;
        let pending = loan.pending()?.ok_or(BAD_ABI)?;
        crate::machine::scheduler::callbacks::wait_call(pending).map(|_| ())
    }

    /// Final buffer checks precede the sole pixel publication. Caller first
    /// prepares scalar output in its local copy, then publishes it infallibly.
    pub(crate) fn commit(&mut self) -> Result<(), Status>
    {
        if !self.done
        {
            return Err(BAD_ABI);
        }
        self.loan.commit(&self.staging)
    }

    pub(crate) fn write_control(&self, call: &mut ControlCall) -> Result<(), Status>
    {
        if !self.done
        {
            return Err(BAD_ABI);
        }
        let Some(Ready::Control(output)) = &self.ready
        else
        {
            return Err(BAD_ABI);
        };
        call.standard = output.standard;
        call.scrollbar = output.scrollbar;
        call.result = output.result;
        Ok(())
    }

    pub(crate) fn write_scroll_box(&mut self, call: &mut sb::Call) -> Result<(), Status>
    {
        if !self.done
        {
            return Err(BAD_ABI);
        }
        let Some(Ready::ScrollBox(output)) = &mut self.ready
        else
        {
            return Err(BAD_ABI);
        };
        output.finish_draw(call);
        Ok(())
    }
}

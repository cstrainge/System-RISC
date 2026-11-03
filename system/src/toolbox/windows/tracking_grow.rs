//! One actual grow gesture, with latest-preview coalescing. Author work and
//! backing leave Desktop through affine loans; the clean scene is never used
//! as scratch. No hardware event is observed or routed here.
#![forbid(unsafe_code)]
use mac_object_table::domain::DomainId;
use spinning_top::Spinlock;

use super::*;
use crate::{machine::scheduler::accounting::AllocationAuthority,
            toolbox::{handles::InvocationGate,
                      processes::ResourceBinding,
                      quickdraw::feedback::{GrowMask, GrowOverlay},
                      resources::definitions::callbacks::Notice,
                      windows::{Window,
                                manager::{canvas::CanvasClaim,
                                          paint::{PaintLoan, PaintOwner, PaintPlan,
                                                  PaintProgress}}}}};

pub(super) struct State
{
    cell:   Spinlock<Cell>,
    notice: Arc<Notice>
}
struct Cell
{
    desired:      Option<PaintPlan>,
    completed:    Option<PaintPlan>,
    pointer_down: bool,
    accepted:     Option<(PaintPlan, Arc<GrowMask>)>,
    spares:       [Option<Arc<GrowMask>>; 2],
    work:         Option<Work>,
    active:       Option<Arc<PaintOwner>>,
    busy:         bool,
    returned:     bool,
    erased:       bool,
    contender:    bool
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage
{
    Start,
    Calculate,
    Surface,
    SetSurface,
    Prepare,
    Capture,
    Advance,
    Replay,
    Publish,
    Retire,
    Done
}
#[derive(Clone, Copy)]
enum Wait
{
    None,
    Source,
    Memory(usize),
    Callback,
    Erase
}
struct Work
{
    plan:       PaintPlan,
    stage:      Stage,
    wait:       Wait,
    cancelled:  bool,
    failure:    Option<Status>,
    allocation: Option<AllocationAuthority>,
    binding:    Option<ResourceBinding>,
    owner:      Option<Arc<PaintOwner>>,
    calculated: Option<Window>,
    mask:       Option<Arc<GrowMask>>,
    // The last field: source/scratch/reply readers drain before target return.
    canvas:     Option<CanvasClaim>
}
struct Loan
{
    state: Arc<State>,
    work:  Option<Work>
}
impl State
{
    pub(super) fn new(notice: Arc<Notice>) -> Result<Arc<Self>, Status>
    {
        Arc::try_new(Self { notice,
                            cell: Spinlock::new(Cell { desired:      None,
                                                       completed:    None,
                                                       pointer_down: false,
                                                       accepted:     None,
                                                       spares:       [None, None],
                                                       work:         None,
                                                       active:       None,
                                                       busy:         false,
                                                       returned:     false,
                                                       erased:       false,
                                                       contender:    false }) }).map_err(|_| {
                                                                                    NO_MEMORY
                                                                                })
    }

    fn with<R>(&self, f: impl FnOnce(&mut Cell) -> R) -> R
    {
        InvocationGate::run(|| f(&mut self.cell.lock()))
    }

    pub(super) fn observe(&self, input: Input)
    {
        if let Input::Button { down,
                               .. } = input
        {
            let changed = self.with(|cell| {
                                  cell.pointer_down = down;
                                  if !down && cell.desired.is_some()
                                  {
                                      cell.desired = None;
                                      true
                                  }
                                  else
                                  {
                                      false
                                  }
                              });
            if changed
            {
                self.notice.request();
            }
        }
    }

    pub(super) fn source_returned(&self)
    {
        self.with(|cell| cell.returned = true);
    }

    pub(super) fn erased(&self)
    {
        let wake =
            self.with(|cell| {
                    cell.erased = true;
                    cell.work.as_ref().is_some_and(|work| matches!(work.wait, Wait::Erase))
                    || cell.accepted.as_ref().is_some_and(|(plan, _)| {
                                                 !cell.desired
                                                  .as_ref()
                                                  .is_some_and(|desired| plan.same_gesture(desired))
                                             })
                    || cell.spares.iter().flatten().any(|mask| {
                                                       !cell.desired
                                                            .as_ref()
                                                            .is_some_and(|desired| {
                                                                desired.storage_owner()
                                                                == mask.storage_owner()
                                                            })
                                                   })
                });
        if wake
        {
            self.notice.request();
        }
    }

    pub(super) fn overlay(&self) -> Option<GrowOverlay>
    {
        self.with(|cell| {
                cell.accepted
                    .as_ref()
                    .filter(|(plan, _)| {
                        cell.desired.as_ref().is_some_and(|desired| plan.same_gesture(desired))
                    })
                    .map(|(_, mask)| GrowOverlay(mask.clone()))
            })
    }

    fn current(&self, plan: &PaintPlan) -> bool
    {
        self.with(|cell| cell.desired.as_ref().is_some_and(|desired| plan.same_tracking(desired)))
    }

    fn retire_masks(&self)
    {
        let retired =
            self.with(|cell| {
                    let mut retired: [Option<Arc<GrowMask>>; 3] = [None, None, None];
                    if cell.accepted.as_ref().is_some_and(|(plan, mask)| {
                                                 Arc::strong_count(mask)==1
                && !cell.desired.as_ref().is_some_and(|desired|plan.same_gesture(desired))
                                             })
                    {
                        retired[0] = cell.accepted.take().map(|(_, mask)| mask);
                    }
                    for (slot, out) in cell.spares.iter_mut().zip(&mut retired[1..])
                    {
                        if slot.as_ref().is_some_and(|mask| {
                                            Arc::strong_count(mask) == 1
                                            && !cell.desired.as_ref().is_some_and(|desired| {
                                                                         desired.storage_owner()
                                                                         == mask.storage_owner()
                                                                     })
                                        })
                        {
                            *out = slot.take();
                        }
                    }
                    retired
                });
        drop(retired);
    }

    fn checkout(self: &Arc<Self>) -> Option<Loan>
    {
        let available = crate::kernel::heap_available();
        let work =
            self.with(|cell| {
                    if cell.busy
                    {
                        cell.contender = true;
                        return None;
                    }
                    let changed =
                        cell.work.as_ref().is_some_and(|work| {
                                              !work.cancelled
                && !cell.desired.as_ref().is_some_and(|desired|work.plan.same_tracking(desired))
                                          });
                    if let Some(work) = cell.work.as_ref()
                    {
                        let ready =
                            changed
                            || (!work.cancelled
                                && (work.owner.as_ref().is_some_and(|owner| owner.cancelled())
                                    || work.canvas
                                           .as_ref()
                                           .is_some_and(|canvas| canvas.cancelled())))
                            || match work.wait
                            {
                                Wait::None => true,
                                Wait::Source => cell.returned,
                                Wait::Memory(before) => cell.returned && available > before,
                                Wait::Callback =>
                                {
                                    work.owner.as_ref().is_some_and(|owner| owner.ready())
                                }
                                Wait::Erase => cell.erased
                            };
                        if !ready
                        {
                            return None;
                        }
                    }
                    else
                    {
                        let plan = cell.desired?;
                        if cell.completed.as_ref().is_some_and(|done| done.same_tracking(&plan))
                        {
                            return None;
                        }
                        let mask = cell.spares
                                       .iter_mut()
                                       .find(|slot| {
                                           slot.as_ref()
                                               .is_some_and(|mask| {
                                                   mask.storage_owner() == plan.storage_owner()
                                               })
                                       })
                                       .and_then(Option::take);
                        cell.work = Some(Work { plan,
                                                stage: Stage::Start,
                                                wait: Wait::None,
                                                cancelled: false,
                                                failure: None,
                                                allocation: None,
                                                binding: None,
                                                owner: None,
                                                calculated: None,
                                                mask,
                                                canvas: None });
                    }
                    cell.busy = true;
                    cell.returned = false;
                    cell.erased = false;
                    cell.work.take()
                })?;
        Some(Loan { state: self.clone(),
                    work:  Some(work) })
    }
}
impl Drop for Loan
{
    fn drop(&mut self)
    {
        // Transfer only; no semantic owner or last mask destruction under the
        // short IRQ/preemption gate. A real contender gets one availability edge.
        let wake = self.state.with(|cell| {
                                 cell.work = self.work.take();
                                 cell.busy = false;
                                 core::mem::take(&mut cell.contender)
                             });
        if wake
        {
            self.state.notice.request();
        }
    }
}
impl Toolbox
{
    pub(super) fn cancel_tracking_grow_input(&mut self)
    {
        let plan =
            self.presentation.tracking_grow.with(|cell| {
                                               cell.desired
                                                   .or_else(|| {
                                                       cell.work.as_ref().map(|work| work.plan)
                                                   })
                                                   .or_else(|| {
                                                       cell.accepted.as_ref().map(|(plan, _)| *plan)
                                                   })
                                           });
        if let Some(plan) = plan
        {
            if self.windows.cancel_tracking_grow(&plan)
            {
                self.presentation.tracking_grow.with(|cell| cell.desired = None);
                self.presentation.feedback_dirty = true;
                self.windows.operation_notice().request();
            }
        }
    }

    pub(super) fn sync_tracking_grow(&mut self)
    {
        let desired = if self.presentation.tracking_grow.with(|cell| cell.pointer_down)
                         && !self.menus.is_tracking()
        {
            self.windows.plan_tracking_grow().ok().flatten()
        }
        else
        {
            None
        };
        let changed =
            self.presentation.tracking_grow.with(|cell| {
                                               let same = match (&cell.desired, &desired)
                                               {
                                                   (None, None) => true,
                                                   (Some(a), Some(b)) => a.same_tracking(b),
                                                   _ => false
                                               };
                                               if same
                                               {
                                                   return false;
                                               }
                                               cell.desired = desired;
                                               true
                                           });
        if changed
        {
            self.presentation.feedback_dirty = true;
            self.windows.operation_notice().request();
        }
    }
}
fn desktop<R>(f: impl FnOnce(&mut Toolbox) -> R) -> Result<R, crate::state::AccessError>
{
    crate::kernel::with_window_toolbox_owned((), |(), toolbox| f(toolbox)).map_err(|(cause, ())| {
                                                                              cause
                                                                          })
}
fn validate(toolbox: &Toolbox, state: &State, work: &Work) -> Result<(), Status>
{
    if !state.current(&work.plan)
    {
        return Err(OPERATION_CANCELLED);
    }
    toolbox.windows.validate_tracking_grow(&work.plan)
}
fn admit(toolbox: &mut Toolbox, state: &State, work: &mut Work) -> Result<(), Status>
{
    validate(toolbox, state, work)?;
    let storage = work.plan.storage_owner();
    if storage != DomainId::SYSTEM
    {
        if work.binding.is_none()
        {
            work.binding = Some(toolbox.processes.resource_binding(work.plan.window().owner)?);
        }
        if work.allocation.is_none()
        {
            work.allocation = Some(toolbox.processes.allocation(work.plan.window().owner)?);
        }
        if work.binding.as_ref().ok_or(BAD_ABI)?.ledger().owner() != storage
           || work.allocation.as_ref().ok_or(BAD_ABI)?.owner() != storage
        {
            return Err(ACCESS_DENIED);
        }
    }
    let guardian = toolbox.windows.canvas_guardian(work.plan.window_id())?;
    if guardian.storage_owner() != storage
    {
        return Err(ACCESS_DENIED);
    }
    if !toolbox.windows.canvas_available(&guardian)?
    {
        return Err(SERVICE_BUSY);
    }
    if work.plan.is_user()?
    {
        if storage == DomainId::SYSTEM
        {
            return Err(NOT_SUPPORTED);
        }
        if state.with(|cell| cell.active.is_some())
        {
            return Err(BAD_ABI);
        }
        let owner = toolbox.windows.admit_paint(work.plan)?;
        // Canonical holder exists before any prepare/capture/enqueue.
        state.with(|cell| cell.active = Some(owner.clone()));
        work.owner = Some(owner);
        work.stage = Stage::Calculate;
    }
    else
    {
        work.canvas = Some(toolbox.windows.claim_canvas(&guardian, state.notice.clone())?);
        work.stage = Stage::Calculate;
    }
    Ok(())
}
#[derive(Clone, Copy, Debug)]
enum Failure
{
    Desktop(crate::state::AccessError),
    Status(Status),
    Author(Status),
    Paint(crate::toolbox::windows::manager::paint::PaintFailure)
}
impl From<Status> for Failure
{
    fn from(status: Status) -> Self
    {
        Self::Status(status)
    }
}
fn in_desktop<R>(f: impl FnOnce(&mut Toolbox) -> Result<R, Status>) -> Result<R, Failure>
{
    desktop(f).map_err(Failure::Desktop)?.map_err(Into::into)
}
/// Actual common Window maintenance; no timer/input polling of Pending. The
/// common observer remains consumed only by kernel::window_work.
pub(crate) fn pump_tracking_grow()
{
    crate::context::without_recovery(|| {
        let selected = desktop(|toolbox| {
            toolbox.sync_tracking_grow();
            toolbox.presentation.tracking_grow.clone()
        });
        let Ok(state) = selected
        else
        {
            return;
        };
        state.retire_masks();
        let Some(mut loan) = state.checkout()
        else
        {
            return;
        };
        let Some(work) = loan.work.as_mut()
        else
        {
            return;
        };
        let result = service(&loan.state, work);
        if let Err(cause) = result
        {
            match cause
            {
                Failure::Desktop(crate::state::AccessError::Busy) => work.wait = Wait::Source,
                Failure::Status(SERVICE_BUSY) =>
                {
                    if !matches!(work.wait, Wait::Erase)
                    {
                        work.wait = Wait::Source;
                    }
                }
                Failure::Status(NO_MEMORY) =>
                {
                    work.wait = Wait::Memory(crate::kernel::heap_available())
                }
                _ =>
                {
                    if work.failure.is_none()
                    {
                        work.failure = Some(match cause
                                            {
                                                Failure::Desktop(cause) => cause.status(),
                                                Failure::Status(status)
                                                | Failure::Author(status) => status,
                                                Failure::Paint(cause) => cause.status()
                                            });
                        serial!("TRACKING_GROW_FAILED window={} cause={cause:?}; original author \
                                 retained until drain",
                                work.plan.window_id().0);
                    }
                    work.cancelled = true;
                    work.wait = Wait::None;
                }
            }
        }
        if work.cancelled && work.stage != Stage::Done
        {
            if let Some(owner) = &work.owner
            {
                owner.cancel();
                if let Ok(mut author) = owner.checkout()
                {
                    match author.advance()
                    {
                        Ok(PaintProgress::Pending) => work.wait = Wait::Callback,
                        _ =>
                        {
                            match author.finish()
                            {
                                Ok(()) => work.stage = Stage::Done,
                                Err(SERVICE_BUSY) => work.wait = Wait::Callback,
                                Err(status) =>
                                {
                                    if work.failure.is_none()
                                    {
                                        work.failure = Some(status);
                                    }
                                    work.wait = Wait::Source;
                                }
                            }
                        }
                    }
                }
            }
            else
            {
                work.stage = Stage::Done;
            }
        }
        if work.stage == Stage::Done
        {
            // Finish/move the original ticket/source/backing outside Desktop.
            let retired = loan.state.with(|cell| {
                                        if !work.cancelled || work.failure.is_some()
                                        {
                                            cell.completed = Some(work.plan);
                                        }
                                        if let Some(mask) = work.mask.take()
                                        {
                                            if let Some(slot) =
                                                cell.spares.iter_mut().find(|slot| slot.is_none())
                                            {
                                                *slot = Some(mask);
                                            }
                                            else
                                            {
                                                return (cell.active.take(), Some(mask));
                                            }
                                        }
                                        (cell.active.take(), None)
                                    });
            drop(retired);
            drop(loan.work.take());
            // A real original claim/backing return, or genuine obsolete desire
            // retirement, permits the next latest preview. No callback ACK.
            loan.state.notice.request();
        }
    });
}
fn service(state: &State, work: &mut Work) -> Result<(), Failure>
{
    let valid = in_desktop(|toolbox| Ok(validate(toolbox, state, work).is_ok()))?;
    if !valid
       || work.owner.as_ref().is_some_and(|owner| owner.cancelled())
       || work.canvas.as_ref().is_some_and(|canvas| canvas.cancelled())
    {
        work.cancelled = true;
    }
    if work.cancelled
    {
        return Ok(());
    }
    work.wait = Wait::None;
    if work.stage == Stage::Start
    {
        in_desktop(|toolbox| admit(toolbox, state, work))?;
    }
    if let Some(owner) = work.owner.clone()
    {
        owner.take_ready();
        let mut author = owner.checkout()?;
        service_user(state, work, &mut author)?;
    }
    else
    {
        if work.stage == Stage::Calculate
        {
            let result = in_desktop(|toolbox| {
                validate(toolbox, state, work)?;
                Ok(toolbox.windows.calculate_trusted_tracking(&work.plan))
            })?;
            work.calculated = Some(result.map_err(Failure::Author)?);
            work.stage = Stage::Surface;
        }
        if work.stage == Stage::Surface
        {
            prepare_mask(work)?;
            work.stage = Stage::Replay;
        }
        if work.stage == Stage::Replay
        {
            let result = in_desktop(|toolbox| {
                validate(toolbox, state, work)?;
                let calculated = work.calculated.ok_or(BAD_ABI)?;
                let mask = Arc::get_mut(work.mask.as_mut().ok_or(BAD_ABI)?).ok_or(SERVICE_BUSY)?;
                let result =
                    toolbox.windows
                           .draw_trusted_tracking(&work.plan, calculated, &mut mask.canvas());
                if result.is_ok()
                {
                    work.stage = Stage::Publish;
                }
                Ok(result)
            })?;
            result.map_err(Failure::Author)?;
        }
        if work.stage == Stage::Publish
        {
            in_desktop(|toolbox| publish(toolbox, state, work))?;
            work.stage = Stage::Done;
        }
    }
    Ok(())
}
fn prepare_mask(work: &mut Work) -> Result<(), Status>
{
    let structure = work.calculated.ok_or(BAD_ABI)?.structure();
    let screen = work.plan.screen();
    // Clone genuine authority only; the mutable mask remains in this same Work
    // across refusal. No uncharged or protected fallback.
    let account = work.allocation.clone();
    if work.mask.as_ref().is_some_and(|mask| Arc::strong_count(mask) != 1)
    {
        work.wait = Wait::Erase;
        return Err(SERVICE_BUSY);
    }
    let storage = work.plan.storage_owner();
    let prepare = || GrowMask::prepare(&mut work.mask, storage, structure, screen);
    if let Some(account) = account
    {
        crate::allocator::with_target(crate::allocator::Target::Owner {account,
            class:mac_kernel_model::policy::MemoryClass::Kernel},prepare)
    }
    else
    {
        InvocationGate::with_system_allocation(prepare)
    }
}
fn service_user(state: &State, work: &mut Work, author: &mut PaintLoan) -> Result<(), Failure>
{
    if work.stage == Stage::Calculate
    {
        match author.advance().map_err(Failure::Paint)?
        {
            PaintProgress::Pending =>
            {
                work.wait = Wait::Callback;
                return Ok(());
            }
            PaintProgress::Cancelled =>
            {
                work.cancelled = true;
                return Ok(());
            }
            PaintProgress::Ready =>
            {
                if let Some(status) = author.author_failure()
                {
                    return Err(Failure::Author(status));
                }
                in_desktop(|toolbox| {
                    validate(toolbox, state, work)?;
                    toolbox.windows.accept_tracking_calculation(author)?;
                    work.calculated = Some(author.tracking_window()?);
                    work.stage = Stage::Surface;
                    Ok(())
                })?;
            }
        }
    }
    if work.stage == Stage::Surface
    {
        prepare_mask(work)?;
        work.stage = Stage::SetSurface;
    }
    if work.stage == Stage::SetSurface
    {
        in_desktop(|toolbox| {
            validate(toolbox, state, work)?;
            let mask = Arc::get_mut(work.mask.as_mut().ok_or(BAD_ABI)?).ok_or(SERVICE_BUSY)?;
            toolbox.windows.set_tracking_surface(author, &mask.canvas())?;
            work.stage = Stage::Prepare;
            Ok(())
        })?;
    }
    if work.stage == Stage::Prepare
    {
        author.prepare()?;
        work.stage = Stage::Capture;
    }
    if work.stage == Stage::Capture
    {
        in_desktop(|toolbox| {
            validate(toolbox, state, work)?;
            let mask = Arc::get_mut(work.mask.as_mut().ok_or(BAD_ABI)?).ok_or(SERVICE_BUSY)?;
            toolbox.windows.capture_paint(author, &mask.canvas())?;
            work.stage = Stage::Advance;
            Ok(())
        })?;
    }
    if work.stage == Stage::Advance
    {
        match author.advance().map_err(Failure::Paint)?
        {
            PaintProgress::Pending =>
            {
                work.wait = Wait::Callback;
                return Ok(());
            }
            PaintProgress::Cancelled =>
            {
                work.cancelled = true;
                return Ok(());
            }
            PaintProgress::Ready => work.stage = Stage::Replay
        }
    }
    if work.stage == Stage::Replay
    {
        if let Some(status) = author.author_failure()
        {
            return Err(Failure::Author(status));
        }
        in_desktop(|toolbox| {
            validate(toolbox, state, work)?;
            let mask = Arc::get_mut(work.mask.as_mut().ok_or(BAD_ABI)?).ok_or(SERVICE_BUSY)?;
            toolbox.windows.replay_paint(author, &mut mask.canvas())?;
            work.stage = Stage::Publish;
            Ok(())
        })?;
    }
    if work.stage == Stage::Publish
    {
        in_desktop(|toolbox| {
            publish(toolbox, state, work)?;
            work.stage = Stage::Retire;
            Ok(())
        })?;
    }
    if work.stage == Stage::Retire
    {
        author.finish()?;
        work.stage = Stage::Done;
    }
    Ok(())
}
fn publish(toolbox: &mut Toolbox, state: &State, work: &mut Work) -> Result<(), Status>
{
    validate(toolbox, state, work)?;
    // Reserve the old mask's actual custody slot before publishing any pixels.
    state.with(|cell| {
             if cell.accepted.is_some() && cell.spares.iter().all(Option::is_some)
             {
                 return Err(SERVICE_BUSY);
             }
             let mask = work.mask.take().ok_or(BAD_ABI)?;
             if let Some((_, old)) = cell.accepted.replace((work.plan, mask))
             {
                 if let Some(slot) = cell.spares.iter_mut().find(|slot| slot.is_none())
                 {
                     *slot = Some(old);
                 }
             }
             cell.completed = Some(work.plan);
             Ok(())
         })?;
    toolbox.presentation.feedback_dirty = true;
    toolbox.present();
    Ok(())
}

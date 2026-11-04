//! Physical input is observed once. Deferred Window work carries its original
//! scalar snapshot; routing never rewrites newer live pointer or modifier state.
#![forbid(unsafe_code)]
use spinning_top::Spinlock;

use super::*;
use crate::toolbox::{resources::definitions::callbacks::Notice,
                     windows::manager::operations::{HitProgress, OperationPlan, Progress,
                                                    TrackingProgress, WindowOperation}};


#[derive(Clone, Copy)]
pub(super) struct ObservedInput
{
    pub(super) input:     Input,
    pub(super) when:      u32,
    pub(super) modifiers: u16,
    pub(super) key:       Option<Event>
}
impl Toolbox
{
    pub(super) fn observe_physical_input(&mut self, input: Input, when: u32) -> ObservedInput
    {
        let (kind, a, b, c) = match input
        {
            Input::Move(point) => (1, point.h as u64, point.v as u64, 0),
            Input::Button { point,
                            down } => (2, point.h as u64, point.v as u64, down as u64),
            Input::Key { mac_code,
                         character,
                         down,
                         repeat,
                         modifiers } =>
            {
                (3,
                 ((mac_code as u64) << 8) | character as u64,
                 down as u64 | ((repeat as u64) << 1),
                 modifiers as u64)
            }
        };
        crate::trace::record(crate::trace::INPUT, self.owner, kind, a, b, c);
        let observed = self.events.observe_input(input, when);
        self.presentation.tracking_grow.observe(input);
        if matches!(input,
                    Input::Button { down: false,
                                    .. })
        {
            self.presentation.feedback_dirty = true;
        }
        if let Input::Button { down,
                               .. } = input
        {
            if down
            {
                // Modal/menu/control clicks invalidate the old observation too.
                self.drag.reset_pointer_press();
            }
        }
        if let Input::Move(point)
        | Input::Button { point,
                               .. } = input
        {
            // Clear obscuring at input time, even if several queued moves
            // bring the pointer back before the next framebuffer is painted.
            self.observe_cursor_motion(point);
        }
        self.activity = self.activity.wrapping_add(1);
        // Key delivery does not change window pixels. Repainting here stalls
        // dispatch on an obsolete frame before the application handles the key.
        // Pointer movement damages the cursor, not the retained scene. Tracking
        // overlays or a changed stacking plane have their own scene damage.
        if !matches!(input, Input::Key { .. })
        {
            self.presentation.cursor_dirty = true;
        }
        ObservedInput { input,
                        when,
                        modifiers: self.events.modifiers,
                        key: observed }
    }
}

pub(super) struct State
{
    value:  Spinlock<Cell>,
    notice: Arc<Notice>
}
impl State
{
    pub(super) fn cancelled(&self) -> bool
    {
        self.value.lock().cancelled
    }

    pub(super) fn new(notice: Arc<Notice>) -> Result<Arc<Self>, Status>
    {
        Arc::try_new(Self { value: Spinlock::new(Cell::default()),
                            notice }).map_err(|_| NO_MEMORY)
    }
}
#[derive(Default)]
struct Cell
{
    active:      Option<Gesture>,
    replacement: Option<Start>,
    tail:        Tail,
    busy:        bool,
    cancelled:   bool,
    waiting:     bool,
    current:     Option<(OperationPlan, Purpose)>
}
#[derive(Default)]
struct Tail
{
    motion:  Option<ObservedInput>,
    release: Option<ObservedInput>
}
#[derive(Clone, Copy)]
enum Purpose
{
    Press,
    DragRelease,
    Tracking
    {
        close: bool
    }
}
#[derive(Clone, Copy)]
struct Start
{
    observation: ObservedInput,
    foreground:  u64,
    plan:        OperationPlan,
    purpose:     Purpose
}
struct Gesture
{
    start:     Start,
    operation: Option<GestureOperation>
}
enum GestureOperation
{
    Hit(WindowOperation),
    Action(native_operations::UserWindowOperation)
}
impl GestureOperation
{
    fn ready(&self) -> bool
    {
        match self
        {
            Self::Hit(operation) => operation.job().ready(),
            Self::Action(operation) => operation.ready()
        }
    }
}
pub(super) enum HitAdmission
{
    Ready(Option<super::super::windows::Hit>),
    Deferred
}
impl Toolbox
{
    /// Mark only changed frontbuffer tracking feedback after actual routing.
    pub(super) fn invalidate_tracking_feedback(&mut self,
                                               before:crate::toolbox::windows::manager::TrackingVisual)
    {
        let after = self.windows.tracking_visual();
        self.sync_tracking_grow();
        if before.tracking != after.tracking || before.outline != after.outline
        {
            self.presentation.feedback_dirty = true;
        }
        if before.chrome != after.chrome
        {
            for chrome in [before.chrome, after.chrome].into_iter().flatten()
            {
                use crate::toolbox::windows::manager::TrackingChrome;
                let (id, zoom) = match chrome
                {
                    TrackingChrome::Close { window,
                                            .. } => (window, false),
                    TrackingChrome::Zoom { window,
                                           .. } => (window, true),
                    TrackingChrome::Grow { .. } => continue
                };
                let Some(window) = self.windows.window(id)
                else
                {
                    continue;
                };
                // These are the definition's own global hit/draw bounds, not
                // the whole title bar or the application's content rectangle.
                let mut local = if zoom { window.zoom_box() } else { window.close_box() };
                local.offset(-window.bounds.left, -window.bounds.top);
                self.presentation.ordinary_underlay.changed();
                self.queue_active_content_damage(id.into(),
                    super::super::quickdraw::region::Region::from_rect(local));
            }
        }
    }

    /// Cancellation does not Drop broker owners under Desktop. The next real
    /// notice detaches the original owner for guard-free disposal.
    pub(crate) fn cancel_window_input(&mut self)
    {
        let before = self.windows.tracking_visual();
        self.cancel_tracking_grow_input();

        let (wake, current, replacement) = {
            let mut cell = self.window_input.value.lock();
            let replacement = cell.replacement.take().map(|start| (start.plan, start.purpose));
            let current =
                cell.current
                    .or_else(|| cell.active.as_ref().map(|g| (g.start.plan, g.start.purpose)));
            let wake = cell.active.is_some() || cell.busy;
            if wake
            {
                cell.cancelled = true;
            }
            (wake, current, replacement)
        };
        for (plan, purpose) in [current, replacement].into_iter().flatten()
        {
            self.windows.cancel_tracking_operation(&plan);
            if matches!(purpose, Purpose::DragRelease)
            {
                let before = self.drag.feedback();
                if self.drag.is_tracking()
                   && let Err(status) = self.drag.escape(Self::when())
                {
                    serial!("DRAG_INPUT_CANCEL_RETAINED status={status}; original drag debt \
                             retained");
                }
                self.drag.reset_pointer_press();
                self.drag_effects(before);
            }
        }
        if wake
        {
            self.window_input.notice.request();
        }
        self.invalidate_tracking_feedback(before);
    }

    pub(super) fn observe_window_input_intent(&mut self, input: Input)
    {
        if matches!(input,
                    Input::Button { down: true,
                                    .. }
                    | Input::Key { down: true,
                                   repeat: false,
                                   character: 27,
                                   .. })
           || matches!(input, Input::Key {down:true,repeat:false,modifiers,mac_code,character}
                       if mac_code!=0xff && modifiers & CMD_KEY != 0 && self.menus.has_key(character))
        {
            self.cancel_window_input();
        }
    }

    fn stage_window_hit(&mut self, observation: ObservedInput, plan: OperationPlan,
                        purpose: Purpose)
    {
        let start = Start { observation,
                            foreground: self.foreground,
                            plan,
                            purpose };
        {
            let mut cell = self.window_input.value.lock();
            if cell.active.is_some() || cell.busy
            {
                cell.cancelled = true;
                cell.replacement = Some(start);
            }
            else
            {
                cell.active = Some(Gesture { start,
                                             operation: None });
                cell.cancelled = false;
            }
            cell.tail = Tail::default();
        }
        self.window_input.notice.request();
    }

    pub(super) fn resolve_observed_press(&mut self, observation: ObservedInput)
                                         -> Option<crate::toolbox::windows::Hit>
    {
        let Input::Button { point,
                            down: true } = observation.input
        else
        {
            return None;
        };
        match self.windows.plan_hit(point)
        {
            Ok(HitProgress::Done(hit)) => Some(hit),
            Ok(HitProgress::User(plan)) =>
            {
                self.stage_window_hit(observation, plan, Purpose::Press);
                None
            }
            Err(status) =>
            {
                note_recovery_input_refusal(self.owner, status);
                None
            }
        }
    }

    pub(super) fn plan_physical_hit(&mut self, input: Input, when: u32) -> HitAdmission
    {
        let Input::Button { point,
                            down: true } = input
        else
        {
            return HitAdmission::Ready(None);
        };
        if self.menus.is_tracking() || self.windows.is_tracking()
        {
            return HitAdmission::Ready(None);
        }
        match self.windows.plan_hit(point)
        {
            Ok(HitProgress::Done(hit)) => HitAdmission::Ready(Some(hit)),
            Ok(HitProgress::User(plan)) =>
            {
                let observation = self.observe_physical_input(input, when);
                self.stage_window_hit(observation, plan, Purpose::Press);
                HitAdmission::Deferred
            }
            Err(status) =>
            {
                let _ = self.observe_physical_input(input, when);
                note_recovery_input_refusal(self.owner, status);
                HitAdmission::Deferred
            }
        }
    }

    pub(super) fn capture_window_input_tail(&mut self, input: Input, when: u32) -> bool
    {
        let capture = {
            let cell = self.window_input.value.lock();
            (cell.replacement.is_some()
             || (!cell.cancelled && (cell.active.is_some() || cell.busy)))
            && cell.tail.release.is_none()
            && matches!(input,
                        Input::Move(_)
                        | Input::Button { down: false,
                                          .. })
        };
        if !capture
        {
            return false;
        }
        let observation = self.observe_physical_input(input, when);
        let mut cell = self.window_input.value.lock();
        if matches!(input, Input::Move(_))
        {
            cell.tail.motion = Some(observation);
        }
        else
        {
            cell.tail.release = Some(observation);
        }
        true
    }

    /// Returns false only while this exact release awaits a real User HIT.
    pub(super) fn observe_drag_release(&mut self, observation: ObservedInput,
                                       admitted: Option<super::super::windows::Hit>)
                                       -> bool
    {
        let Input::Button { point,
                            down: false } = observation.input
        else
        {
            return true;
        };
        if !self.drag.awaiting_pointer_release()
        {
            return true;
        }
        let hit = match admitted.map(HitProgress::Done)
                                .map(Ok)
                                .unwrap_or_else(|| self.windows.plan_hit(point))
        {
            Ok(HitProgress::Done(hit)) => hit,
            Ok(HitProgress::User(plan)) =>
            {
                self.stage_window_hit(observation, plan, Purpose::DragRelease);
                return false;
            }
            Err(status) =>
            {
                note_recovery_input_refusal(self.owner, status);
                return false;
            }
        };
        let hot = self.drag_hot_with_hit(hit);
        self.drag.observe_pointer_release(point, observation.modifiers, hot, observation.when);
        true
    }

    pub(super) fn stage_tracking_release(&mut self, observation: ObservedInput) -> bool
    {
        let Input::Button { point,
                            down: false } = observation.input
        else
        {
            return false;
        };
        if !self.windows.is_tracking()
        {
            return false;
        }
        match self.windows.plan_tracking_release(point)
        {
            Ok(TrackingProgress::Ordinary) => false,
            Ok(TrackingProgress::Cancelled(plan)) =>
            {
                self.windows.cancel_tracking_operation(&plan);
                self.post_window_release(observation);
                true
            }
            Ok(TrackingProgress::User(plan)) =>
            {
                self.stage_window_hit(observation, plan, Purpose::Tracking { close: false });
                true
            }
            Ok(TrackingProgress::Close(plan)) =>
            {
                self.stage_window_hit(observation, plan, Purpose::Tracking { close: true });
                true
            }
            Err(status) =>
            {
                note_recovery_input_refusal(self.owner, status);
                self.post_window_release(observation);
                true
            }
        }
    }

    pub(super) fn post_window_release(&mut self, observation: ObservedInput)
    {
        let Input::Button { point,
                            down: false } = observation.input
        else
        {
            return;
        };
        self.events.post_system(Event { kind:      EventKind::MouseUp,
                                        message:   0,
                                        window:    0,
                                        when:      observation.when,
                                        where_:    point,
                                        modifiers: observation.modifiers,
                                        payload:   0 });
    }

    fn fail_window_input(&mut self, gesture: &Gesture)
    {
        let before = self.windows.tracking_visual();

        self.windows.cancel_tracking_operation(&gesture.start.plan);
        if matches!(gesture.start.purpose, Purpose::DragRelease)
           && !self.window_input.cancelled()
           && self.foreground == gesture.start.foreground
        {
            let before = self.drag.feedback();
            if self.drag.is_tracking()
               && let Err(status) = self.drag.escape(gesture.start.observation.when)
            {
                serial!("DRAG_INPUT_CANCEL_RETAINED status={status}; original drag debt retained");
            }
            self.drag.reset_pointer_press();
            self.drag_effects(before);
        }
        if matches!(gesture.start.purpose, Purpose::Tracking { .. })
           && !self.window_input.cancelled()
           && self.foreground == gesture.start.foreground
        {
            let caller = self.owner;
            self.enter(self.foreground);
            self.post_window_release(gesture.start.observation);
            self.route_events();
            self.enter(caller);
        }
        self.invalidate_tracking_feedback(before);
    }

    fn finish_window_input_action(&mut self, gesture: &Gesture) -> Result<(), Status>
    {
        if self.window_input.cancelled() || self.foreground != gesture.start.foreground
        {
            return Err(OPERATION_CANCELLED);
        }
        let caller = self.owner;
        self.enter(self.foreground);
        self.post_window_release(gesture.start.observation);
        self.route_events();
        self.sync_input_state();
        self.enter(caller);
        Ok(())
    }

    fn checkout_window_input(&mut self) -> Option<Loan>
    {
        let mut cell = self.window_input.value.lock();
        if cell.busy
        {
            cell.waiting = true;
            return None;
        }
        if cell.active.is_none()
           && let Some(start) = cell.replacement.take()
        {
            cell.active = Some(Gesture { start,
                                         operation: None });
            cell.cancelled = false;
        }
        let gesture = cell.active.take()?;
        if self.foreground != gesture.start.foreground
        {
            cell.cancelled = true;
        }
        cell.busy = true;
        cell.current = Some((gesture.start.plan, gesture.start.purpose));
        Some(Loan { cell:    self.window_input.clone(),
                    gesture: Some(gesture) })
    }

    fn finish_window_input_hit(&mut self, gesture: &Gesture,
                               completion:&mut
                                crate::toolbox::windows::manager::operations::Completion)
                               -> Result<Option<OperationPlan>, Status>
    {
        if self.window_input.value.lock().cancelled || self.foreground != gesture.start.foreground
        {
            return Err(OPERATION_CANCELLED);
        }
        match self.windows.finish_hit(completion)?
        {
            HitProgress::User(plan) => Ok(Some(plan)),
            HitProgress::Done(hit) =>
            {
                let tail = core::mem::take(&mut self.window_input.value.lock().tail);
                let caller = self.owner;
                self.enter(self.foreground);
                let observation = gesture.start.observation;
                self.input_with_observation(observation.input,
                                            observation.when,
                                            Some(hit),
                                            Some(observation));
                if matches!(gesture.start.purpose, Purpose::Press)
                {
                    for observation in [tail.motion, tail.release].into_iter().flatten()
                    {
                        self.input_with_observation(observation.input,
                                                    observation.when,
                                                    None,
                                                    Some(observation));
                    }
                }
                self.route_events();
                self.sync_input_state();
                self.enter(caller);
                Ok(None)
            }
        }
    }
}


/// A single pre-admitted cell, not a global identity registry. This owner can
/// restore custody even if its final Desktop admission is refused.
struct Loan
{
    cell:    Arc<State>,
    gesture: Option<Gesture>
}
impl Drop for Loan
{
    fn drop(&mut self)
    {
        let returning = self.gesture.take();
        let (discarded, wake) = {
            let mut cell = self.cell.value.lock();
            cell.busy = false;
            let discarded = if cell.cancelled || cell.active.is_some()
            {
                returning
            }
            else
            {
                cell.active = returning;
                None
            };
            cell.cancelled = false;
            cell.current = cell.active.as_ref().map(|g| (g.start.plan, g.start.purpose));
            let wake = core::mem::take(&mut cell.waiting)
                       || (cell.active.is_none() && cell.replacement.is_some());
            (discarded, wake)
        };
        drop(discarded);
        if wake
        {
            self.cell.notice.request();
        }
    }
}

pub(super) fn with_window_toolbox<R>(run: impl FnOnce(&mut Toolbox) -> R)
                                     -> Result<R, crate::state::AccessError>
{
    crate::kernel::with_window_toolbox_owned((),|(),toolbox|run(toolbox))
        .map_err(|(cause,())|cause)
}

enum InputFailure
{
    Desktop(crate::state::AccessError),
    Operation(Status)
}
impl From<Status> for InputFailure
{
    fn from(status: Status) -> Self
    {
        Self::Operation(status)
    }
}
impl InputFailure
{
    fn status(&self) -> Status
    {
        match self
        {
            Self::Desktop(cause) => cause.status(),
            Self::Operation(status) => *status
        }
    }
}

/// Root calls this only after consuming the shared WM work notice once. No
/// all-window polling, author call or owner destructor occurs under Desktop.
pub(crate) fn pump_window_input()
{
    let selected = with_window_toolbox(|toolbox| toolbox.checkout_window_input());
    let Ok(Some(mut loan)) = selected
    else
    {
        return;
    };
    let result = (|| -> Result<bool, InputFailure> {
        if loan.cell.value.lock().cancelled
        {
            return Err(OPERATION_CANCELLED.into());
        }
        let gesture = loan.gesture.as_mut().ok_or(BAD_ABI)?;
        if gesture.operation.is_none()
        {
            let notice = with_window_toolbox(|toolbox| {
                             if toolbox.window_input.cancelled()
                                || toolbox.foreground != gesture.start.foreground
                             {
                                 return Err(OPERATION_CANCELLED);
                             }
                             Ok(toolbox.windows.operation_notice())
                         }).map_err(InputFailure::Desktop)??;
            let operation=match gesture.start.purpose
            {
                Purpose::Tracking {close}=>GestureOperation::Action(
                    native_operations::UserWindowOperation::from_input(gesture.start.plan,close,
                        gesture.start.observation,gesture.start.foreground,&loan.cell,notice)?),
                Purpose::Press|Purpose::DragRelease=>
                {
                    let operation=WindowOperation::prepare_system(gesture.start.plan,notice)?;
                    with_window_toolbox(|toolbox| {
                        if toolbox.window_input.cancelled() || toolbox.foreground!=gesture.start.foreground
                        {return Err(OPERATION_CANCELLED);}
                        toolbox.windows.admit_operation(operation.job())
                    }).map_err(InputFailure::Desktop)??;
                    GestureOperation::Hit(operation)
                }
            };
            gesture.operation = Some(operation);
        }
        if !gesture.operation.as_ref().ok_or(BAD_ABI)?.ready()
        {
            return Ok(true);
        }
        if let Some(GestureOperation::Action(operation)) = gesture.operation.as_mut()
        {
            return match operation.advance()?
            {
                None => Ok(true),
                Some(status) =>
                {
                    if !operation.release_posted()
                    {
                        with_window_toolbox(|toolbox| {
                            toolbox.finish_window_input_action(gesture)
                        }).map_err(InputFailure::Desktop)??;
                    }
                    if status != OK
                    {
                        serial!("WINDOW_TRACKING_REFUSED status={status}; original release \
                                 consumed");
                    }
                    Ok(false)
                }
            };
        }
        let Some(GestureOperation::Hit(operation)) = gesture.operation.as_ref()
        else
        {
            return Err(BAD_ABI.into());
        };
        match operation.advance()?
        {
            Progress::Pending => Ok(true),
            Progress::Complete(mut completion) =>
            {
                let next = with_window_toolbox(|toolbox| {
                               toolbox.finish_window_input_hit(gesture, &mut completion)
                           }).map_err(InputFailure::Desktop)??;
                drop(completion);
                if let Some(plan) = next
                {
                    gesture.start.plan = plan;
                    drop(gesture.operation.take());
                    loan.cell.notice.request();
                    Ok(true)
                }
                else
                {
                    Ok(false)
                }
            }
            Progress::Finished(_) => Err(BAD_ABI.into())
        }
    })();
    match result
    {
        Ok(true) | Err(InputFailure::Desktop(crate::state::AccessError::Busy)) => (),
        Ok(false) => drop(loan.gesture.take()),
        Err(failure) =>
        {
            let status = failure.status();
            if let Some(gesture) = loan.gesture.as_ref()
            {
                if with_window_toolbox(|toolbox| toolbox.fail_window_input(gesture)).is_err()
                {
                    return;
                }
            }
            if status != OPERATION_CANCELLED
            {
                serial!("WINDOW_INPUT_REFUSED status={status}; original gesture cancelled");
            }
            drop(loan.gesture.take());
        }
    }
}

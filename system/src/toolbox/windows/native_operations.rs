//! Owned Window geometry and User WDEF operations retain the original request.
//! Trusted CALCULATE is cached as scalar state; User authors use real callbacks.
//! The Window Manager owns close tickets and cached original author outcomes.
#![forbid(unsafe_code)]
use super::*;
use crate::toolbox::{controls::objects::{ControlLayout, LayoutProgress, PreparedWindowLayout},
                     resources::definitions::callbacks::{CompletionTarget, Notice, Pending},
                     windows::manager::{CloseError,
                                        operations::{Completion, Geometry, Outcome, Progress,
                                                     WindowOperation}}};

/// Desktop acquisition has no admitted Window effects and may retry the SAME
/// request after its actual availability edge. A real service refusal must not
/// be mistaken for Desktop contention (notably callback self-queue Busy).
pub(crate) enum BeginError
{
    Desktop
    {
        cause: crate::state::AccessError,
        epoch: crate::kernel::DesktopEpoch
    },
    Service(Status)
}
impl BeginError
{
    pub(crate) fn status(&self) -> Status
    {
        match self
        {
            Self::Desktop { cause,
                            .. } => cause.status(),
            Self::Service(status) => *status
        }
    }
}
impl From<Status> for BeginError
{
    fn from(status: Status) -> Self
    {
        Self::Service(status)
    }
}

/// Captured from the ORIGINAL native caller before guard-free preparation.
/// It limits waiting; it grants no target or definition authority.
#[derive(Clone, Copy)]
enum CallerRole
{
    NativeProcess,
    NativeCallback,
    SystemInput
}

pub(crate) struct UserWindowOperation
{
    operation:       WindowOperation,
    admitted:        bool,
    geometry_wait:   Option<crate::toolbox::windows::manager::canvas::GeometryAdmission>,
    caller_role:     CallerRole,
    kind:            u32,
    when:            u32,
    completion:      Option<Completion>,
    terminal:        Option<Status>,
    caller:          u64,
    waiter:          Option<crate::context::ThreadId>,
    notice:          Arc<Notice>,
    layout_checked:  bool,
    layout:          Option<ControlLayout>,
    layout_pending:  Option<Arc<Pending>>,
    prepared_layout: Option<PreparedWindowLayout>,
    input:           Option<alloc::sync::Weak<input_callbacks::State>>,
    pointer:         Option<input_callbacks::ObservedInput>,
    allocation:      Option<crate::machine::scheduler::accounting::AllocationAuthority>,
    desktop_wait:    Option<crate::kernel::DesktopEpoch>,
    release_posted:  bool,
    ui_wait:         bool
}
impl UserWindowOperation
{
    pub(crate) fn begin(kind: u32, request: &WindowRequest) -> Result<Option<Self>, BeginError>
    {
        if !matches!(kind, CLOSE_WINDOW | SET_WINDOW_BOUNDS | SET_STANDARD_BOUNDS)
        {
            return Ok(None);
        }
        let epoch = crate::kernel::window_desktop_epoch();
        let (plan, notice, when, caller, allocation) =
            input_callbacks::with_window_toolbox(|toolbox| {
                let id = WindowId(request.id);
                if !toolbox.owns(id)
                {
                    return Err(if toolbox.windows.window(id).is_some()
                               {
                                   ACCESS_DENIED
                               }
                               else
                               {
                                   NOT_FOUND
                               });
                }
                let plan = match kind
                {
                    CLOSE_WINDOW => toolbox.windows.plan_user_close(id)?,
                    SET_WINDOW_BOUNDS =>
                    {
                        toolbox.windows.plan_geometry(id, Geometry::Bounds(request.bounds))?
                    }
                    SET_STANDARD_BOUNDS =>
                    {
                        let bounds = match request.reserved
                        {
                            0 => Some(request.bounds),
                            1 => None,
                            _ => return Err(BAD_ARGUMENT)
                        };
                        toolbox.windows.plan_geometry(id, Geometry::Standard(bounds))?
                    }
                    _ => return Err(BAD_ARGUMENT)
                };
                if kind == CLOSE_WINDOW
                   && plan.is_some()
                   && let Ok(execution) =
                       crate::machine::scheduler::callbacks::authenticate_current()
                   && (toolbox.windows.close_waits_on(id, &execution)?
                       || toolbox.controls.window_disposal_waits_on(id.0, &execution))
                {
                    return Err(SERVICE_BUSY);
                }
                let allocation = if kind == CLOSE_WINDOW
                {
                    None
                }
                else
                {
                    plan.as_ref().map(|plan| Self::geometry_allocation(toolbox, plan)).transpose()?
                };
                Ok((plan,
                    toolbox.windows.operation_notice(),
                    Toolbox::when(),
                    toolbox.owner,
                    allocation))
            }).map_err(|cause| {
                  BeginError::Desktop { cause,
                                        epoch }
              })??;
        let Some(plan) = plan
        else
        {
            return Ok(None);
        };
        let thread = crate::context::current_thread_context().identity().thread;
        let caller_role = match crate::machine::scheduler::callbacks::authenticate_current()
        {
            Ok(execution) =>
            {
                if execution.thread_id() != thread
                {
                    return Err(ACCESS_DENIED.into());
                }
                CallerRole::NativeCallback
            }
            Err(_) => CallerRole::NativeProcess
        };
        let prepare = || WindowOperation::prepare(plan, thread, notice.clone());
        let operation = match &allocation
        {
            Some(allocation) => crate::allocator::with_target(crate::allocator::Target::Owner {
                account: allocation.clone(), class: mac_kernel_model::policy::MemoryClass::Kernel
            }, prepare)?,
            None => prepare()?
        };
        if kind == CLOSE_WINDOW
        {
            let epoch = crate::kernel::window_desktop_epoch();
            input_callbacks::with_window_toolbox(|toolbox| {
            if !toolbox.owns(plan.window_id())
            {
                return Err(ACCESS_DENIED);
            }
            if kind == CLOSE_WINDOW
            {
                toolbox.controls.prepare_remove_window(plan.window_id().0)?;
                toolbox.drag.prepare_window_close(plan.window_id().0)?;
                toolbox.windows.admit_user_close(operation.job(), &mut toolbox.events, when)?;
                toolbox.observe_dialog_window_close(super::DialogCloseObserver::User(operation.job().clone()));
            }
            Ok(())
        }).map_err(|cause| {
              BeginError::Desktop { cause,
                                    epoch }
          })??;
        }
        Ok(Some(Self { operation,
                       admitted: kind == CLOSE_WINDOW,
                       geometry_wait: None,
                       caller_role,
                       kind,
                       when,
                       completion: None,
                       terminal: None,
                       caller,
                       waiter: Some(thread),
                       notice,
                       layout_checked: false,
                       layout: None,
                       layout_pending: None,
                       prepared_layout: None,
                       input: None,
                       pointer: None,
                       allocation,
                       desktop_wait: None,
                       release_posted: false,
                       ui_wait: false }))
    }

    fn cleanup(&mut self) -> Result<(), Status>
    {
        let Some(receipt) = self.operation.take_close_cleanup()
        else
        {
            return if self.terminal.is_some() && !self.operation.job().wait_ready()
            {
                self.ui_wait = true;
                Err(SERVICE_BUSY)
            }
            else
            {
                Ok(())
            };
        };
        receipt.arm_wait();
        let epoch = crate::kernel::window_desktop_epoch();
        let attempt = input_callbacks::with_window_toolbox(|toolbox| {
            toolbox.cleanup_user_window_close(&receipt)
        });
        let result = self.desktop_result(epoch, attempt);
        match result
        {
            Ok(Ok(removed)) =>
            {
                drop(removed);
                receipt.finish();
                Ok(())
            }
            Ok(Err(status)) =>
            {
                receipt.defer();
                self.ui_wait = status == SERVICE_BUSY;
                Err(status)
            }
            Err(status) =>
            {
                receipt.defer();
                Err(status)
            }
        }
    }

    fn close_status(outcome: Outcome) -> Status
    {
        match outcome
        {
            Outcome::Closed(Ok(_)) => OK,
            Outcome::Closed(Err(CloseError::NotFound(_))) => NOT_FOUND,
            Outcome::Closed(Err(CloseError::Cleanup(cause))) => close_ui::cleanup_status(cause),
            Outcome::Quiesced(_) => OPERATION_CANCELLED
        }
    }

    /// None retains this same operation for its real readiness capability.
    pub(crate) fn advance(&mut self) -> Result<Option<Status>, Status>
    {
        self.desktop_wait = None;
        self.ui_wait = false;
        let allocation = self.allocation.clone();
        let result = match allocation
        {
            Some(allocation) => crate::allocator::with_target(crate::allocator::Target::Owner {
                account: allocation, class: mac_kernel_model::policy::MemoryClass::Kernel
            }, || self.advance_inner()),
            None => self.advance_inner()
        };
        if result.is_err() && (self.desktop_wait.is_some() || self.ui_wait)
        {
            Ok(None)
        }
        else
        {
            result
        }
    }

    fn advance_inner(&mut self) -> Result<Option<Status>, Status>
    {
        if !self.admitted
        {
            let epoch = crate::kernel::window_desktop_epoch();
            let job = self.operation.job();
            let caller = self.caller;
            let callback_caller = matches!(self.caller_role, CallerRole::NativeCallback);
            let admission = &mut self.geometry_wait;
            let attempt = input_callbacks::with_window_toolbox(|toolbox| {
                if toolbox.owner != caller || !toolbox.owns(job.window_id())
                {
                    return Err(ACCESS_DENIED);
                }
                toolbox.windows.admit_geometry_wait(job, admission, callback_caller)
            });
            if !self.desktop_result(epoch, attempt)??
            {
                return Ok(None);
            }
            self.admitted = true;
            // Original registration/header pins return only outside Desktop.
            drop(self.geometry_wait.take());
        }
        if let Some(status) = self.terminal
        {
            self.cleanup()?;
            return Ok(Some(status));
        }
        if self.completion.is_none()
        {
            match self.operation.advance()?
            {
                Progress::Pending => return Ok(None),
                Progress::Complete(completion) => self.completion = Some(completion),
                Progress::Finished(outcome) =>
                {
                    let status = Self::close_status(outcome);
                    self.terminal = Some(status);
                    self.cleanup()?;
                    return Ok(Some(status));
                }
            }
        }
        if self.kind == CLOSE_WINDOW
        {
            let completion = self.completion.as_mut().ok_or(BAD_ABI)?;
            // Canonical debt retains its ticket on refusal. Observe only the
            // job's genuine cached terminal outcome after the detached owner
            // returns its original reply; never infer success from NotFound.
            let job = self.operation.job();
            let epoch = crate::kernel::window_desktop_epoch();
            let attempt = input_callbacks::with_window_toolbox(|toolbox| {
                toolbox.prepare_user_window_removal(job)?;
                Ok(toolbox.windows.finish_user_close(completion))
            });
            let result = match self.desktop_result(epoch, attempt)?
            {
                Ok(result) => result,
                Err(SERVICE_BUSY) => return Ok(None),
                Err(status) => return Err(status)
            };
            drop(self.completion.take());
            self.cleanup()?;
            match self.operation.advance()?
            {
                Progress::Finished(outcome) =>
                {
                    let status = Self::close_status(outcome);
                    self.terminal = Some(status);
                    Ok(Some(status))
                }
                Progress::Complete(completion) =>
                {
                    drop(completion);
                    match result
                    {
                        Err(CloseError::Cleanup(cause))
                            if close_ui::cleanup_status(cause) == SERVICE_BUSY =>
                        {
                            Ok(None)
                        }
                        Err(CloseError::NotFound(_)) => Err(NOT_FOUND),
                        Err(CloseError::Cleanup(cause)) => Err(close_ui::cleanup_status(cause)),
                        Ok(_) => Err(BAD_ABI)
                    }
                }
                Progress::Pending => Ok(None)
            }
        }
        else
        {
            self.advance_geometry()
        }
    }

    fn check_input(&self, toolbox: &Toolbox) -> Result<(), Status>
    {
        if let Some(input) = self.input.as_ref()
           && (input.upgrade().is_none_or(|state| state.cancelled())
               || toolbox.foreground != self.caller)
        {
            return Err(OPERATION_CANCELLED);
        }
        Ok(())
    }

    pub(super) fn from_input(plan: crate::toolbox::windows::manager::operations::OperationPlan,
                             close: bool, observation: input_callbacks::ObservedInput,
                             foreground: u64, state: &Arc<input_callbacks::State>,
                             notice: Arc<Notice>)
                             -> Result<Self, Status>
    {
        if !matches!(observation.input,
                     Input::Button { down: false,
                                     .. })
        {
            return Err(BAD_ARGUMENT);
        }

        let allocation = if close
        {
            None
        }
        else
        {
            Some(input_callbacks::with_window_toolbox(|toolbox| {
                     Self::geometry_allocation(toolbox, &plan)
                 }).map_err(|cause| cause.status())??)
        };
        let prepare = || WindowOperation::prepare_system(plan, notice.clone());
        let operation = match &allocation
        {
            Some(allocation) => crate::allocator::with_target(crate::allocator::Target::Owner {
                account: allocation.clone(), class: mac_kernel_model::policy::MemoryClass::Kernel
            }, prepare)?,
            None => prepare()?
        };
        input_callbacks::with_window_toolbox(|toolbox| {
            if state.cancelled() || toolbox.foreground != foreground
            {
                return Err(OPERATION_CANCELLED);
            }
            if close
            {
                toolbox.controls.prepare_remove_window(plan.window_id().0)?;
                toolbox.drag.prepare_window_close(plan.window_id().0)?;
                toolbox.windows
                       .admit_user_close(operation.job(), &mut toolbox.events, observation.when)?;
            }
            else
            {
                toolbox.windows.admit_operation(operation.job())?;
            }
            Ok(())
        }).map_err(|cause| cause.status())??;
        Ok(Self { operation,
                  admitted: true,
                  geometry_wait: None,
                  caller_role: CallerRole::SystemInput,
                  kind: if close { CLOSE_WINDOW } else { SET_WINDOW_BOUNDS },
                  when: observation.when,
                  completion: None,
                  terminal: None,
                  caller: foreground,
                  waiter: None,
                  notice,
                  layout_checked: false,
                  layout: None,
                  layout_pending: None,
                  prepared_layout: None,
                  input: Some(Arc::downgrade(state)),
                  pointer: Some(observation),
                  allocation,
                  desktop_wait: None,
                  release_posted: false,
                  ui_wait: false })
    }

    fn desktop_result<R>(&mut self, epoch: crate::kernel::DesktopEpoch,
                         result: Result<R, crate::state::AccessError>)
                         -> Result<R, Status>
    {
        match result
        {
            Ok(value) =>
            {
                self.desktop_wait = None;
                Ok(value)
            }
            Err(cause) =>
            {
                self.desktop_wait = if matches!(cause, crate::state::AccessError::Busy)
                {
                    Some(epoch)
                }
                else
                {
                    None
                };
                Err(cause.status())
            }
        }
    }

    /// Genuine original WIND storage seal -> same full Process account. This
    /// is not the WDEF source account or the ambient System input account.
    fn geometry_allocation(
        toolbox: &Toolbox, plan: &crate::toolbox::windows::manager::operations::OperationPlan)
        -> Result<crate::machine::scheduler::accounting::AllocationAuthority, Status>
    {
        let storage = plan.storage_owner();
        if storage.kind() != mac_object_table::domain::DomainKind::Process
        {
            return Err(NOT_SUPPORTED);
        }
        let allocation = toolbox.processes.allocation(storage.incarnation())?;
        if allocation.owner() != storage
        {
            return Err(ACCESS_DENIED);
        }
        Ok(allocation)
    }

    fn completion_target(&self) -> CompletionTarget
    {
        match self.waiter
        {
            Some(thread) =>
            {
                CompletionTarget::ThreadAndSystem { thread,
                                                    notice: self.notice.clone() }
            }
            None => CompletionTarget::System(self.notice.clone())
        }
    }

    fn advance_geometry(&mut self) -> Result<Option<Status>, Status>
    {
        if !self.layout_checked
        {
            let completion = self.completion.as_ref().ok_or(BAD_ABI)?;
            let epoch = crate::kernel::window_desktop_epoch();
            let attempt = input_callbacks::with_window_toolbox(|toolbox| {
                self.check_input(toolbox)?;
                let Some(viewport) = toolbox.windows.proposed_viewport(completion)?
                else
                {
                    return Ok(None);
                };
                let window = completion.window_id().0;
                let paint_pending =
                    toolbox.presentation
                           .pending_content
                           .iter()
                           .any(|content| content.id == SceneTarget::from(WindowId(window)));
                let allocation = self.allocation.as_ref().ok_or(BAD_ABI)?;
                if allocation.owner() != completion.storage_owner()
                {
                    return Err(ACCESS_DENIED);
                }
                let plan =
                    toolbox.controls
                           .begin_window_layout(window, viewport, paint_pending, &allocation)?;
                Ok(Some((plan, allocation.clone())))
            });
            let plan = self.desktop_result(epoch, attempt)??;
            if let Some((plan, allocation)) = plan
            {
                self.layout = Some(plan.prepare(self.completion_target(), &allocation)
                                       .map_err(|(status, plan)| {
                                           drop(plan);
                                           status
                                       })?);
                self.allocation = Some(allocation);
            }
            self.layout_checked = true;
        }
        if self.prepared_layout.is_none()
           && let Some(layout) = self.layout.as_mut()
        {
            drop(self.layout_pending.take());
            match layout.advance()?
            {
                LayoutProgress::Wait(pending) =>
                {
                    self.layout_pending = Some(pending);
                    return Ok(None);
                }
                LayoutProgress::Ready(prepared) => self.prepared_layout = Some(prepared)
            }
        }
        let input = self.input.clone();
        let original_caller = self.caller;
        let when = self.when;
        let pointer = self.pointer;
        let completion = self.completion.as_mut().ok_or(BAD_ABI)?;
        let prepared = &mut self.prepared_layout;
        let epoch = crate::kernel::window_desktop_epoch();
        let attempt = input_callbacks::with_window_toolbox(|toolbox| {
            let caller = toolbox.owner;
            toolbox.enter(original_caller);
            let cancelled = input.as_ref().is_some_and(|input| {
                                              input.upgrade().is_none_or(|state| state.cancelled())
                                              || toolbox.foreground != original_caller
                                          });
            let result = if cancelled
            {
                Err(OPERATION_CANCELLED)
            }
            else if toolbox.owner != original_caller
            {
                Err(ACCESS_DENIED)
            }
            else
            {
                toolbox.finish_user_window_geometry(completion, prepared.as_mut(), when, pointer)
            };
            toolbox.enter(caller);
            result
        });
        self.desktop_result(epoch, attempt)??;
        self.release_posted = self.pointer.is_some();
        drop(self.completion.take());
        drop(self.prepared_layout.take());
        drop(self.layout.take());
        self.terminal = Some(OK);
        Ok(Some(OK))
    }

    pub(super) fn release_posted(&self) -> bool
    {
        self.release_posted
    }

    pub(super) fn ready(&self) -> bool
    {
        if !self.admitted && self.desktop_wait.is_none()
        {
            return self.operation.job().wait_ready();
        }
        if self.ui_wait
        {
            return self.operation.job().wait_ready();
        }

        if let Some(epoch) = self.desktop_wait
        {
            return crate::kernel::window_desktop_epoch() != epoch;
        }

        if let Some(pending) = self.layout_pending.as_ref()
        {
            return pending.ready();
        }
        self.completion.is_some() || self.terminal.is_some() || self.operation.job().ready()
    }

    pub(crate) fn wait(&self) -> Result<(), Status>
    {
        if let Some(epoch) = self.desktop_wait
        {
            return crate::machine::scheduler::callbacks::wait_desktop(epoch).map(|_| ());
        }

        if let Some(pending) = self.layout_pending.as_ref()
        {
            return crate::machine::scheduler::callbacks::wait_call(pending).map(|_| ());
        }
        crate::machine::scheduler::callbacks::wait_window_call(self.operation.job()).map(|_| ())
    }
}

impl Toolbox
{
    /// The resident S path cannot suspend an app author. Reject before any
    /// WDEF/scene effects rather than running immediate User layout or turning
    /// a refused layout into successful native WINDOW status.
    pub(super) fn preflight_resident_window_geometry(&self, id: WindowId, intent: Geometry)
                                                     -> Result<(), Status>
    {
        let Some(plan) = self.windows.plan_geometry(id, intent)?
        else
        {
            return Ok(());
        };
        if plan.geometry_needs_user()
        {
            return Err(NOT_SUPPORTED);
        }
        let current = self.windows.window(id).ok_or(NOT_FOUND)?;
        if matches!(intent, Geometry::Bounds(_))
           && current.local_bounds() != plan.viewport()
           && self.controls.window_geometry_needs_staging(id.0)?
        {
            return Err(NOT_SUPPORTED);
        }
        Ok(())
    }

    fn finish_user_window_geometry(&mut self, completion: &mut Completion,
                                   mut prepared: Option<&mut PreparedWindowLayout>, when: u32,
                                   pointer: Option<input_callbacks::ObservedInput>)
                                   -> Result<(), Status>
    {
        let id = completion.window_id();
        let (used, limit) = window_layout::scene_admission(&self.windows,
                                                           &self.presentation.content,
                                                           &self.presentation.pending_content,
                                                           SceneTarget::from(id));
        let content = &mut self.presentation.content;
        let controls = &mut self.controls;
        let mut damage = None;
        let action=self.windows.finish_geometry(completion,|_,actual,viewport| {
            if actual!=id {return Err(BAD_ABI);}
            let ready=prepared.as_deref_mut().ok_or(BAD_ABI)?;
            if ready.window()!=id.0 || ready.viewport()!=viewport {return Err(BAD_ABI);}
            damage=Some(window_layout::publish_prepared_window_layout(content,controls,ready,used,limit)?);
            Ok(())
        },&mut self.events,when)?;
        if let Some(damage) = damage
        {
            for rect in damage.into_iter().filter(|rect| !rect.is_empty())
            {
                self.content_changed_rect(id, rect);
            }
        }
        if let Some(action) = action
        {
            if let Some(observation) = pointer
            {
                let point = match observation.input
                {
                    Input::Move(point)
                    | Input::Button { point,
                                    .. } => point,
                    Input::Key { .. } => return Err(BAD_ABI)
                };
                self.action_at_input(action, when, point, observation.modifiers);
            }
            else
            {
                self.action(action, when);
            }
        }
        if let Some(observation) = pointer
        {
            self.post_window_release(observation);
            self.route_events();
            self.sync_input_state();
        }

        Ok(())
    }
}

/// The canonical WM owner retains ticket/control/UI work. This continuation
/// observes only its authenticated final result, including after cancellation.
pub(crate) struct TrustedWindowClose
{
    handle: crate::toolbox::windows::manager::trusted::TrustedCloseHandle
}
impl TrustedWindowClose
{
    pub(crate) fn begin(kind: u32, request: &WindowRequest) -> Result<Option<Self>, BeginError>
    {
        if kind != CLOSE_WINDOW
        {
            return Ok(None);
        }
        let thread = crate::context::current_thread_context().identity().thread;
        let epoch = crate::kernel::window_desktop_epoch();
        input_callbacks::with_window_toolbox(|toolbox| {
            let id = WindowId(request.id);
            if toolbox.windows.plan_trusted_close(id)?.is_none()
            {
                return Ok(None);
            }
            let handle = toolbox.request_trusted_window_close_for(id, Some(thread))?;
            Ok(Some(Self { handle }))
        }).map_err(|cause| {
              BeginError::Desktop { cause,
                                    epoch }
          })?
          .map_err(BeginError::Service)
    }

    pub(crate) fn advance(&mut self) -> Result<Option<Status>, Status>
    {
        Ok(self.handle.poll_result().map(|result| {
                                        match result
                                        {
                                            Ok(_) => OK,
                                            Err(CloseError::NotFound(_)) => NOT_FOUND,
                                            Err(CloseError::Cleanup(cause)) =>
                                            {
                                                close_ui::cleanup_status(cause)
                                            }
                                        }
                                    }))
    }

    pub(crate) fn wait(&self) -> Result<(), Status>
    {
        crate::machine::scheduler::callbacks::wait_trusted_window_call(&self.handle).map(|_| ())
    }
}

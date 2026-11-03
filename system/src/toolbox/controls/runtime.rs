//! Native managed-control admission. CDEF validation stays in its original
//! Desktop invocation; owned title, binding and publication storage prepare
//! outside that guard, retaining the exact validated request through retries.
#![forbid(unsafe_code)]

use core::fmt;

use mac_abi::controls::{CREATE_SCROLLBAR, CREATE_STANDARD, ControlCall};

use super::*;
use crate::toolbox::controls::objects::{ControlCommit, ControlPlan, CreationError};

#[derive(Debug)]
enum ControlAdmissionError
{
    Toolbox(crate::state::AccessError),
    Service
    {
        stage:  &'static str,
        status: Status
    },
    Control(CreationError),
    Definition(crate::loader::LoadError)
}
impl ControlAdmissionError
{
    fn status(self) -> Status
    {
        match self
        {
            Self::Toolbox(cause) => cause.status(),
            Self::Service { status,
                            .. } => status,
            Self::Control(cause) => cause.status(),
            Self::Definition(cause) => crate::loader::status(cause)
        }
    }
}
impl fmt::Display for ControlAdmissionError
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Toolbox(cause) => write!(out, "control Desktop access refused: {cause:?}"),
            Self::Service { stage,
                            status } => write!(out, "control {stage} refused with status {status}"),
            Self::Control(cause) => cause.fmt(out),
            Self::Definition(cause) => cause.fmt(out)
        }
    }
}
fn toolbox<T>(run: impl FnOnce(&mut Toolbox) -> Result<T, ControlAdmissionError>)
              -> Result<T, ControlAdmissionError>
{
    crate::kernel::with_toolbox(run).map_err(ControlAdmissionError::Toolbox)?
}
fn service(stage: &'static str, status: Status) -> ControlAdmissionError
{
    ControlAdmissionError::Service { stage,
                                     status }
}
enum Turn
{
    Cold(i16),
    Internal,
    Process(ControlPlan)
}
/// Shared retained admission for one managed object. Caller retains staging and
/// the returned old command until its own safe destruction boundary.
pub(super) fn publish_drawing(
    content: &mut Content, drawing: &mut Option<crate::toolbox::controls::Drawing>, used: usize,
    limit: usize)
    -> Result<((Rect, [Rect; 2], Option<Rect>), Option<RenderCommand>), Status>
{
    let next = drawing.as_ref().ok_or(BAD_ARGUMENT)?;
    let bounds = next.bounds();
    let damage = next.damage();
    let previous =
        content.iter_commands()
               .find(|command| matches!(command, RenderCommand::Control(old) if old.id == next.id));
    let old_bounds = previous.and_then(|command| {
                                 if let RenderCommand::Control(old) = command
                                 {
                                     Some(old.bounds())
                                 }
                                 else
                                 {
                                     None
                                 }
                             });
    let next_bytes = used.checked_sub(previous.map_or(0, RenderCommand::storage))
                         .ok_or(CORRUPT_DATA)?
                         .checked_add(next.storage())
                         .ok_or(NO_MEMORY)?;
    if next_bytes > limit || (previous.is_none() && content.commands.len() >= 512)
    {
        return Err(NO_MEMORY);
    }
    let retired = content.replace_control(drawing)?;
    Ok(((bounds, damage, old_bounds), retired))
}

impl Toolbox
{
    fn control_parent(&self, call: &ControlCall) -> Result<(), Status>
    {
        if call.window != 0 && !self.owns(WindowId(call.window))
        {
            return Err(if self.windows.window(WindowId(call.window)).is_some()
                       {
                           ACCESS_DENIED
                       }
                       else
                       {
                           NOT_FOUND
                       });
        }
        Ok(())
    }

    fn plan_native_control(&mut self, call: &ControlCall) -> Result<Turn, ControlAdmissionError>
    {
        if let Some(id) =
            self.preflight_control(call).map_err(|status| service("request preflight", status))?
        {
            return Ok(Turn::Cold(id));
        }
        // System modal controls use their real reserved publication and Activity.
        // This is a distinct owner contract, never a fallback after refusal.
        if crate::context::recovery_activity().is_some()
        {
            return Ok(Turn::Internal);
        }
        let (recipient, storage) =
            self.service_owners().map_err(|status| service("actual service owners", status))?;
        self.control_parent(call).map_err(|status| service("parent window", status))?;
        let (definition, _) = self.controls
                                  .preflight_definition(&recipient, &storage, call)
                                  .map_err(|status| service("definition intent", status))?;
        let entry = crate::toolbox::runtime::Toolbox::control_definition(definition)
                        .map_err(|status| service("prepared definition", status))?;
        let allocation =
            self.processes
                .allocation(self.owner)
                .map_err(|status| service("original Process account", status))?;
        self.controls
            .plan_process_create(&recipient,
                                 &storage,
                                 &allocation,
                                 call,
                                 entry,
                                 crate::machine::traps::gateway)
            .map(Turn::Process)
            .map_err(ControlAdmissionError::Control)
    }

    fn validate_native_control_owner(&self, call: &ControlCall,
                                     recipient: &super::super::handles::OwnerRef,
                                     storage: &super::super::handles::OwnerRef)
                                     -> Result<(), ControlAdmissionError>
    {
        let (current_recipient, current_storage) =
            self.service_owners().map_err(|status| service("current service owners", status))?;
        if recipient.owner() != current_recipient.owner()
           || storage.owner() != current_storage.owner()
           || recipient.owner() != storage.owner()
        {
            return Err(service("original Process owner changed", ACCESS_DENIED));
        }
        self.control_parent(call).map_err(|status| service("current parent window", status))
    }
}

/// Native Process-owned creation only. Queries, tracking and existing object
/// updates retain the original CONTROL trap path.
pub(crate) fn native_control(call: &mut ControlCall) -> Status
{
    if !matches!(call.operation, CREATE_STANDARD | CREATE_SCROLLBAR)
    {
        return BAD_ARGUMENT;
    }
    match create_native(call)
    {
        Ok(status) => status,
        Err(cause) =>
        {
            serial!("CONTROL_ADMISSION_REFUSED: {cause}");
            cause.status()
        }
    }
}

pub(crate) struct UserBirth
{
    entry:   DefinitionRef,
    pending: crate::toolbox::resources::definitions::callbacks::Pending,
    cleanup: Option<super::super::controls::objects::ControlCleanup>
}
impl UserBirth
{
    pub(crate) fn begin(call: &ControlCall) -> Result<Self, Status>
    {
        if !matches!(call.operation, CREATE_STANDARD | CREATE_SCROLLBAR)
        {
            return Err(BAD_ARGUMENT);
        }
        let id = super::super::controls::creation_definition_id(call)?;
        // Owner/parent and structural checks precede cold executable work.
        let (storage, allocation, notice) =
            toolbox(|runtime| {
                runtime.preflight_control(call)
                       .map_err(|s| service("user request preflight", s))?;
                let (recipient, storage) =
                    runtime.service_owners().map_err(|s| service("user owner", s))?;
                runtime.validate_native_control_owner(call, &recipient, &storage)?;
                let allocation = runtime.processes
                                        .allocation(runtime.owner)
                                        .map_err(|s| service("user storage account", s))?;
                if allocation.owner() != storage.owner()
                {
                    return Err(service("user storage account owner", ACCESS_DENIED));
                }
                Ok((storage, allocation, runtime.controls.disposal_notice()))
            }).map_err(ControlAdmissionError::status)?;
        let entry = crate::loader::prepare_definition(*b"CDEF", id).map_err(crate::loader::status)?;
        if !crate::toolbox::resources::definitions::is_user(entry)?
        {
            return Err(ACCESS_DENIED);
        }
        // Admit the actual fixed disposal/RETURN carrier before this first
        // constructor author call. No live CTRL identity or Activity is parked.
        let cleanup = super::super::controls::objects::ControlCleanup::prepare(entry, call,
                                                                               &storage, notice,
                                                                               &allocation)?;
        let request = super::super::controls::user_validation_request(call)?;
        let waiter = crate::context::current_thread_context().identity().thread;
        let pending =
            crate::toolbox::resources::definitions::callbacks::enqueue_for_object(entry,
                                                                                  request,
                                                                                  waiter.into(),
                                                                                  &allocation).map_err(crate::toolbox::resources::definitions::callbacks::EnqueueError::status)?;
        Ok(Self { entry,
                  pending,
                  cleanup: Some(cleanup) })
    }

    pub(crate) fn pending(&self) -> &crate::toolbox::resources::definitions::callbacks::Pending
    {
        &self.pending
    }

    pub(crate) fn advance(&mut self, call: &mut ControlCall) -> Result<Option<Status>, Status>
    {
        let Some(reply) = self.pending.poll()?
        else
        {
            return Ok(None);
        };
        let validated = super::super::controls::apply_user_validation(call, &reply)?;
        let mut plan =
            toolbox(|runtime| {
                let (recipient, storage) =
                    runtime.service_owners().map_err(|s| service("user owner", s))?;
                runtime.validate_native_control_owner(call, &recipient, &storage)?;
                let (id, _) = runtime.controls
                                     .preflight_definition(&recipient, &storage, call)
                                     .map_err(|s| service("user definition intent", s))?;
                let entry = Toolbox::control_definition(id).map_err(|s| {
                                                               service("user definition source", s)
                                                           })?;
                if !entry.same(self.entry)
                {
                    return Err(service("user definition changed", SERVICE_BUSY));
                }
                let allocation = runtime.processes
                                        .allocation(runtime.owner)
                                        .map_err(|s| service("user original Process account", s))?;
                runtime.controls
                       .plan_process_user_create(&recipient,
                                                 &storage,
                                                 &allocation,
                                                 call,
                                                 &validated,
                                                 entry,
                                                 crate::machine::traps::gateway)
                       .map_err(ControlAdmissionError::Control)
            }).map_err(ControlAdmissionError::status)?;
        let cleanup = self.cleanup.take().ok_or(BAD_ABI)?;
        if let Err((status, cleanup)) = plan.attach_cleanup(cleanup)
        {
            self.cleanup = Some(cleanup);
            return Err(status);
        }
        publish_native(plan, call).map(Some).map_err(ControlAdmissionError::status)
    }
}
fn create_native(call: &mut ControlCall) -> Result<Status, ControlAdmissionError>
{
    let mut cold = false;
    let plan = loop
    {
        match toolbox(|runtime| runtime.plan_native_control(call))?
        {
            Turn::Cold(id) if !cold =>
            {
                crate::loader::prepare_definition(*b"CDEF", id)
                    .map_err(ControlAdmissionError::Definition)?;
                cold = true;
            }
            Turn::Cold(_) =>
            {
                return Err(service("definition source changed during preparation", SERVICE_BUSY));
            }
            Turn::Internal => return toolbox(|runtime| Ok(runtime.control(call))),
            Turn::Process(plan) => break plan
        }
    };
    publish_native(plan, call)
}
fn publish_native(plan: ControlPlan, call: &mut ControlCall)
                  -> Result<Status, ControlAdmissionError>
{
    let mut input = Some(plan.prepare().map_err(ControlAdmissionError::Control)?);
    let mut prepared = loop
    {
        let admission = toolbox(|runtime| {
            let value =
                input.as_ref().ok_or_else(|| service("unpublished preparation", BAD_ABI))?;
            runtime.validate_native_control_owner(call, value.recipient(), value.storage())?;
            if !value.matches_request(call)
            {
                return Err(service("original validated intent changed", ACCESS_DENIED));
            }
            runtime.controls
                   .admit_process_create(&mut input)
                   .map_err(ControlAdmissionError::Control)
        });
        match admission
        {
            Ok(value) => break Some(value),
            Err(ControlAdmissionError::Control(cause)) if cause.is_capacity_changed() =>
            {
                // Do not invoke foreign VALIDATE again, nor discard its title
                // or definition when only the metadata snapshot is stale.
                let plan = toolbox(|runtime| {
                    let value =
                        input.as_ref().ok_or_else(|| service("retained preparation", BAD_ABI))?;
                    runtime.validate_native_control_owner(call,
                                                          value.recipient(),
                                                          value.storage())?;
                    if !value.matches_request(call)
                    {
                        return Err(service("retained validated intent changed", ACCESS_DENIED));
                    }
                    value.plan_capacity(&runtime.controls).map_err(ControlAdmissionError::Control)
                })?;
                let capacity = plan.prepare().map_err(ControlAdmissionError::Control)?;
                let value = input.as_mut().ok_or_else(|| service("retained payload", BAD_ABI))?;
                value.replace_capacity(capacity).dispose();
            }
            Err(cause) => return Err(cause)
        }
    };
    if let Some(input) = input
    {
        input.dispose();
    }
    let mut retired = None;
    let result = toolbox(|runtime| {
        let value = prepared.as_ref().ok_or_else(|| service("canonical permit", BAD_ABI))?;
        runtime.validate_native_control_owner(call, value.recipient(), value.storage())?;
        if !value.matches_request(call)
        {
            return Err(service("admitted validated intent changed", ACCESS_DENIED));
        }
        let (definition, _) =
            runtime.controls
                   .preflight_definition(value.recipient(), value.storage(), call)
                   .map_err(|status| service("current definition intent", status))?;
        let entry = crate::toolbox::runtime::Toolbox::control_definition(definition)
                           .map_err(|status| service("current definition source", status))?;
        let complete = runtime.controls
                              .commit_process_create(&mut prepared, call, entry)
                              .map_err(ControlAdmissionError::Control)?;
        let ControlCommit { result,
                            retired: old } = complete;
        retired = Some(old);
        let drawing = result.map_err(|status| service("postpublication control request", status))?;
        runtime.finish_managed_control(call, call.window, drawing)
               .map(|()| OK)
               .map_err(|status| service("retained control drawing", status))
    });
    if let Some(retired) = retired
    {
        retired.dispose();
    }
    if let Some(prepared) = prepared
    {
        prepared.dispose();
    }
    result
}

pub(crate) enum UserOperationProgress
{
    DisposalWait(alloc::sync::Arc<super::super::controls::objects::ControlDisposal>),
    Wait(Arc<crate::toolbox::resources::definitions::callbacks::Pending>),
    Complete(Status)
}
pub(crate) struct UserOperation
{
    state:     UserOperationState,
    recipient: super::super::handles::OwnerRef,
    storage:   super::super::handles::OwnerRef,
    id:        u64,
    operation: u32
}
enum UserOperationState
{
    Disposal(super::super::controls::objects::DisposalObserver),
    Working(super::super::controls::objects::ControlWork),
    Ready(super::super::controls::objects::PreparedControlOperation),
    Layout(super::super::controls::objects::ControlLayout),
    LayoutReady(super::super::controls::objects::PreparedWindowLayout),
    CancelTracking,
    Complete(Status)
}
impl UserOperation
{
    /// Elect a managed leaf request without changing manager state on None.
    /// Original owner/parent checks precede all cold executable preparation.
    pub(crate) fn begin(call: &ControlCall, suspension: Result<(), Status>)
                        -> Result<Option<Self>, Status>
    {
        use mac_abi::controls::*;
        if call.operation == DISPOSE_OBJECT
        {
            return Self::begin_disposal(call, suspension);
        }
        if !matches!(call.operation,
                     CONFIGURE_OBJECT
                     | REFLOW_OBJECT
                     | DRAW_OBJECT
                     | SET_STATIC_TEXT_OBJECT
                     | SET_TABLE_LINES_OBJECT
                     | BEGIN_TRACKING
                     | POLL_TRACKING
                     | HIT_CHILDREN_OBJECT)
        {
            return Ok(None);
        }
        if matches!(call.operation, BEGIN_TRACKING | POLL_TRACKING)
        {
            let active = toolbox(|runtime| {
                             let (recipient, _) =
                                 runtime.service_owners()
                                        .map_err(|status| service("tracking owners", status))?;
                             let window =
                                 runtime.controls
                                        .window(call.id, recipient.owner().incarnation())
                                        .map_err(|status| service("tracking Window", status))?;
                             Ok(window == 0
                                || (runtime.owner == runtime.foreground
                                    && runtime.windows.active_window() == Some(WindowId(window))))
                         }).map_err(ControlAdmissionError::status)?;
            // Original resident path performs real cancellation, without any
            // author effects or implicit foreground change, when inactive.
            if !active
            {
                return Ok(None);
            }
        }
        if let Some(operation) = Self::begin_layout(call, suspension)?
        {
            return Ok(Some(operation));
        }
        let source =
            toolbox(|runtime| {
                let (recipient, storage) =
                    runtime.service_owners()
                           .map_err(|status| service("operation owners", status))?;
                let Some(normalized) =
                    runtime.controls
                           .classify_control_operation(call, recipient.owner().incarnation())
                           .map_err(|status| service("operation class", status))?
                else
                {
                    return Ok(None);
                };
                runtime.preflight_control(&normalized)
                       .map_err(|status| service("operation preflight", status))?;
                runtime.control_parent(&normalized)
                       .map_err(|status| service("operation parent", status))?;
                let (id, bound) =
                    runtime.controls
                           .preflight_definition(&recipient, &storage, &normalized)
                           .map_err(|status| service("operation definition", status))?;
                let old_process =
                    call.operation == CONFIGURE_OBJECT
                    && runtime.controls
                              .process_control_source(call.id, recipient.owner().incarnation())
                              .map_err(|status| service("operation original source", status))?;
                Ok(Some((recipient, storage, id, bound, old_process)))
            }).map_err(ControlAdmissionError::status)?;
        let Some((recipient, storage, id, bound, old_process)) = source
        else
        {
            return Ok(None);
        };
        let process_source = match bound
        {
            Some(entry) => crate::toolbox::resources::definitions::is_user(entry)?,
            None => crate::loader::process_definition(*b"CDEF", id).map_err(crate::loader::status)?
        };
        if !process_source && !old_process
        {
            return Ok(None);
        }
        suspension?;
        // Trusted System/Module and protected modal fast paths never admit a
        // Process AccountUse. Only elected user callbacks require this proof.
        let allocation = toolbox(|runtime| {
                             runtime.validate_native_control_owner(call, &recipient, &storage)?;
                             let allocation = runtime.processes
                                                     .allocation(runtime.owner)
                                                     .map_err(|status| {
                                                         service("operation original account",
                                                                 status)
                                                     })?;
                             if allocation.owner() != storage.owner()
                             {
                                 return Err(service("operation account owner", ACCESS_DENIED));
                             }
                             Ok(allocation)
                         }).map_err(ControlAdmissionError::status)?;
        let cold = bound.is_none();
        let entry = match bound
        {
            Some(entry) => entry,
            None =>
            {
                let entry =
                    crate::loader::prepare_definition(*b"CDEF", id).map_err(crate::loader::status)?;
                if crate::toolbox::resources::definitions::is_user(entry)? != process_source
                {
                    return Err(SERVICE_BUSY);
                }
                entry
            }
        };
        let plan =
            toolbox(|runtime| {
                runtime.validate_native_control_owner(call, &recipient, &storage)?;
                let window = runtime.controls
                                    .window(call.id, recipient.owner().incarnation())
                                    .map_err(|status| service("operation live window", status))?;
                if runtime.presentation
                          .pending_content
                          .iter()
                          .any(|content| content.id == SceneTarget::from(window))
                   && runtime.controls
                             .changes_tree_geometry(call, recipient.owner().incarnation())
                             .map_err(|status| service("operation geometry", status))?
                {
                    return Err(service("operation pending paint", SERVICE_BUSY));
                }
                runtime.controls
                       .plan_control_operation(&recipient, &storage, call, entry, &allocation)
                       .map_err(|status| service("operation claim", status))
            }).map_err(ControlAdmissionError::status)?;
        let Some(plan) = plan
        else
        {
            return if cold { Err(SERVICE_BUSY) } else { Ok(None) };
        };
        let waiter = crate::context::current_thread_context().identity().thread;
        let work = match plan.prepare(waiter.into(), &allocation)
        {
            Ok(work) => work,
            Err((status, plan)) =>
            {
                drop(plan);
                return Err(status);
            }
        };
        Ok(Some(Self { state: UserOperationState::Working(work),
                       recipient,
                       storage,
                       id: call.id,
                       operation: call.operation }))
    }

    fn begin_layout(call: &ControlCall, suspension: Result<(), Status>)
                    -> Result<Option<Self>, Status>
    {
        use mac_abi::controls::reflow as wire;
        if !matches!(call.operation,
                     mac_abi::controls::HIT_CHILDREN_OBJECT
                     | mac_abi::controls::BEGIN_TRACKING
                     | mac_abi::controls::POLL_TRACKING)
           && (call.operation != mac_abi::controls::REFLOW_OBJECT
               || !matches!(call.layout.message,
                            wire::ASK_SIZE
                            | wire::ASSIGN_RECT
                            | wire::SET_ANCHORS
                            | wire::SET_WINDOW_BORDER
                            | wire::SET_SCROLLING
                            | wire::SCROLL_TO
                            | wire::REVEAL_CHILD))
        {
            return Ok(None);
        }
        let source =
            toolbox(|runtime| {
                let (recipient, storage) =
                    runtime.service_owners().map_err(|status| service("layout owners", status))?;
                let owner = recipient.owner().incarnation();
                let window =
                    runtime.controls
                           .window(call.id, owner)
                           .map_err(|status| service("layout control window", status))?;
                if window != 0 && !runtime.owns(WindowId(window))
                {
                    return Err(service("layout window owner", ACCESS_DENIED));
                }
                let tracking = if matches!(call.operation,
                                           mac_abi::controls::BEGIN_TRACKING
                                           | mac_abi::controls::POLL_TRACKING)
                {
                    let needed =
                        runtime.controls
                               .tracking_layout_operation(call, owner)
                               .map_err(|status| service("tracking callback election", status))?;
                    if !needed
                    {
                        return Ok(None);
                    }
                    Some(needed)
                }
                else
                {
                    None
                };
                let viewport = if window == 0
                                  && (matches!(call.operation,
                                               mac_abi::controls::HIT_CHILDREN_OBJECT
                                               | mac_abi::controls::BEGIN_TRACKING
                                               | mac_abi::controls::POLL_TRACKING)
                                      || matches!(call.layout.message,
                                                  wire::ASK_SIZE | wire::ASSIGN_RECT))
                {
                    Rect::ZERO
                }
                else
                {
                    runtime.windows
                           .window(WindowId(window))
                           .ok_or(service("layout live window", NOT_FOUND))?
                           .local_bounds()
                };
                let needs_author = if let Some(needed) = tracking
                {
                    needed
                }
                else
                {
                    runtime.controls
                           .user_layout_operation(call, owner, viewport)
                           .map_err(|status| service("layout callback election", status))?
                };
                let prepare_bar =
                    runtime.controls
                           .needs_scrollbar_definition(call, owner)
                           .map_err(|status| service("scrollbar dependency", status))?;
                Ok(Some((recipient, storage, window, viewport, needs_author, prepare_bar)))
            }).map_err(ControlAdmissionError::status)?;
        let Some((recipient, storage, window, viewport, needs_author, prepare_bar)) = source
        else
        {
            return Ok(None);
        };
        let bar_author = if prepare_bar
        {
            crate::loader::process_definition(*b"CDEF", 1).map_err(crate::loader::status)?
        }
        else
        {
            false
        };
        if !needs_author && !bar_author
        {
            return Ok(None);
        }
        suspension?;
        if prepare_bar
        {
            // Genuine current source preparation is guard-free and precedes
            // snapshot/claim publication or author effects.
            crate::loader::prepare_definition(*b"CDEF", 1).map_err(crate::loader::status)?;
        }
        let (plan, allocation) =
            toolbox(|runtime| {
                runtime.validate_native_control_owner(call, &recipient, &storage)?;
                if window != 0
                   && runtime.windows
                             .window(WindowId(window))
                             .is_none_or(|window| window.local_bounds() != viewport)
                {
                    return Err(service("layout viewport changed", SERVICE_BUSY));
                }
                let allocation =
                    runtime.processes.allocation(runtime.owner).map_err(|status| {
                                                                    service("layout original \
                                                                             account",
                                                                            status)
                                                                })?;
                if allocation.owner() != storage.owner()
                {
                    return Err(service("layout account owner", ACCESS_DENIED));
                }
                let pending = runtime.presentation
                                     .pending_content
                                     .iter()
                                     .any(|content| content.id == SceneTarget::from(window));
                let plan = runtime.controls
                                  .begin_control_layout(call,
                                                        recipient.owner().incarnation(),
                                                        viewport,
                                                        pending,
                                                        &allocation)
                                  .map_err(|status| service("layout claim", status))?;
                Ok((plan, allocation))
            }).map_err(ControlAdmissionError::status)?;
        let waiter = crate::context::current_thread_context().identity().thread;
        let work = match plan.prepare(waiter.into(), &allocation)
        {
            Ok(work) => work,
            Err((status, plan)) =>
            {
                drop(plan);
                return Err(status);
            }
        };
        Ok(Some(Self { state: UserOperationState::Layout(work),
                       recipient,
                       storage,
                       id: call.id,
                       operation: call.operation }))
    }

    /// Elect an app-owned control disposal. Trusted definitions (and a
    /// non-root refusal) stay on the synchronous path by returning None.
    fn begin_disposal(call: &ControlCall, suspension: Result<(), Status>)
                      -> Result<Option<Self>, Status>
    {
        let source = toolbox(|runtime| {
                         let (recipient, storage) =
                             runtime.service_owners()
                                    .map_err(|status| service("disposal owners", status))?;
                         if !runtime.controls.disposal_observed(call.id, storage.owner())
                            && !runtime.controls
                                       .disposal_needs_author(call, recipient.owner().incarnation())
                                       .map_err(|status| service("disposal class", status))?
                         {
                             return Ok(None);
                         }
                         if !runtime.controls.disposal_observed(call.id, storage.owner())
                         {
                             runtime.control_parent(call)
                                    .map_err(|status| service("disposal parent", status))?;
                         }
                         Ok(Some((recipient, storage)))
                     }).map_err(ControlAdmissionError::status)?;
        let Some((recipient, storage)) = source
        else
        {
            return Ok(None);
        };
        suspension?;
        let allocation = toolbox(|runtime| {
                             runtime.validate_native_control_owner(call, &recipient, &storage)?;
                             let allocation = runtime.processes
                                                     .allocation(runtime.owner)
                                                     .map_err(|status| {
                                                         service("disposal original account",
                                                                 status)
                                                     })?;
                             if allocation.owner() != storage.owner()
                             {
                                 return Err(service("disposal account owner", ACCESS_DENIED));
                             }
                             Ok(allocation)
                         }).map_err(ControlAdmissionError::status)?;
        let observer = toolbox(|runtime| {
                           runtime.validate_native_control_owner(call, &recipient, &storage)?;
                           crate::allocator::with_target(crate::allocator::Target::Owner {
                account: allocation.clone(), class: mac_kernel_model::policy::MemoryClass::Kernel
            }, || runtime.controls.begin_control_disposal(call, &recipient))
                .map_err(|status| service("canonical disposal",status))
                       }).map_err(ControlAdmissionError::status)?;
        let Some(observer) = observer
        else
        {
            return Ok(None);
        };
        Ok(Some(Self { state: UserOperationState::Disposal(observer),
                       recipient,
                       storage,
                       id: call.id,
                       operation: call.operation }))
    }

    pub(crate) fn advance(&mut self, call: &mut ControlCall)
                          -> Result<UserOperationProgress, Status>
    {
        if let UserOperationState::Complete(status) = &self.state
        {
            // The SAME stable transport snapshot already holds final output.
            return Ok(UserOperationProgress::Complete(*status));
        }
        if call.id != self.id || call.operation != self.operation
        {
            return Err(BAD_ARGUMENT);
        }
        loop
        {
            match &mut self.state
            {
                UserOperationState::Disposal(observer) =>
                {
                    if observer.advance()?
                    {
                        *call = observer.output();
                        self.state = UserOperationState::Complete(OK);
                        return Ok(UserOperationProgress::Complete(OK));
                    }
                    return Ok(UserOperationProgress::DisposalWait(observer.wait()));
                }
                UserOperationState::Working(work) =>
                {
                    match work.advance()?
                    {
                        super::super::controls::objects::ControlProgress::Wait(pending) =>
                        {
                            return Ok(UserOperationProgress::Wait(pending));
                        }
                        super::super::controls::objects::ControlProgress::Ready(ready) =>
                        {
                            self.state = UserOperationState::Ready(ready)
                        }
                    }
                }
                UserOperationState::Ready(ready) =>
                {
                    let result =
                        toolbox(|runtime| {
                            runtime.validate_native_control_owner(&ready.output(),
                                                                  &self.recipient,
                                                                  &self.storage)?;
                            if matches!(self.operation,
                                        mac_abi::controls::BEGIN_TRACKING
                                        | mac_abi::controls::POLL_TRACKING)
                               && ready.window() != 0
                               && (runtime.owner != runtime.foreground
                                   || runtime.windows.active_window()
                                      != Some(WindowId(ready.window())))
                            {
                                return Ok(None);
                            }
                            runtime.commit_user_control_operation(ready)
                                   .map(Some)
                                   .map_err(|status| service("operation publication", status))
                        }).map_err(ControlAdmissionError::status)?;
                    let Some(result) = result
                    else
                    {
                        self.state = UserOperationState::CancelTracking;
                        continue;
                    };
                    *call = ready.output();
                    // The entire old reader/claim carrier leaves Desktop before
                    // replacement by this scalar cached terminal result.
                    self.state = UserOperationState::Complete(result);
                    return Ok(UserOperationProgress::Complete(result));
                }
                UserOperationState::Layout(work) =>
                {
                    match work.advance()?
                    {
                        super::super::controls::objects::LayoutProgress::Wait(pending) =>
                        {
                            return Ok(UserOperationProgress::Wait(pending));
                        }
                        super::super::controls::objects::LayoutProgress::Ready(ready) =>
                        {
                            self.state = UserOperationState::LayoutReady(ready)
                        }
                    }
                }
                UserOperationState::LayoutReady(ready) =>
                {
                    let result = toolbox(|runtime| {
                                     runtime.validate_native_control_owner(call, &self.recipient,
                                                                           &self.storage)?;
                                     if matches!(self.operation,
                                                 mac_abi::controls::BEGIN_TRACKING
                                                 | mac_abi::controls::POLL_TRACKING)
                                        && ready.window() != 0
                                        && (runtime.owner != runtime.foreground
                                            || runtime.windows.active_window()
                                               != Some(WindowId(ready.window())))
                                     {
                                         return Ok(None);
                                     }
                                     runtime.commit_user_control_layout(ready)
                                            .map(Some)
                                            .map_err(|status| service("layout publication", status))
                                 }).map_err(ControlAdmissionError::status)?;
                    let Some(result) = result
                    else
                    {
                        self.state = UserOperationState::CancelTracking;
                        continue;
                    };
                    if let Some(output) = ready.output()
                    {
                        *call = output;
                    }
                    if call.operation == mac_abi::controls::REFLOW_OBJECT
                       && matches!(call.layout.message,
                                   mac_abi::controls::reflow::ASK_SIZE
                                   | mac_abi::controls::reflow::ASSIGN_RECT)
                    {
                        call.layout.measurement = ready.measurement();
                    }
                    self.state = UserOperationState::Complete(result);
                    return Ok(UserOperationProgress::Complete(result));
                }
                UserOperationState::CancelTracking =>
                {
                    let mut cancel = *call;
                    cancel.operation = mac_abi::controls::CANCEL_TRACKING;
                    let result =
                        toolbox(|runtime| {
                            runtime.validate_native_control_owner(&cancel, &self.recipient,
                                                                  &self.storage)?;
                            let window =
                                runtime.controls
                                       .window(cancel.id, self.recipient.owner().incarnation())
                                       .map_err(|status| service("cancellation Window", status))?;
                            runtime.cancel_control_tracking(&mut cancel,
                                                            self.recipient.owner().incarnation(),
                                                            window)
                                   .map_err(|status| service("tracking cancellation", status))?;
                            Ok(OK)
                        }).map_err(ControlAdmissionError::status)?;
                    *call = cancel;
                    self.state = UserOperationState::Complete(result);
                    return Ok(UserOperationProgress::Complete(result));
                }
                UserOperationState::Complete(status) =>
                {
                    return Ok(UserOperationProgress::Complete(*status));
                }
            }
        }
    }
}

impl Toolbox
{
    /// Original native cancellation, with all fallible outline admission before
    /// the scalar capture change. Both resident and completed owned calls use it.
    pub(super) fn cancel_control_tracking(&mut self, call: &mut ControlCall, owner: u64,
                                          window: u64)
                                          -> Result<(), Status>
    {
        let split = self.controls.splitter_capture(call.id, owner)?;
        let outline = if split
        {
            Some(self.windows.prepare_control_outline(WindowId(window), None)?)
        }
        else
        {
            None
        };
        self.controls.cancel(call, owner)?;
        let changed = outline.is_some_and(|permit| permit.commit());
        self.presentation.feedback_dirty |= changed;
        if changed
        {
            self.present();
        }
        self.clear_control_highlight(window, call.id, false);
        Ok(())
    }

    fn commit_user_control_layout(&mut self,
                                  ready: &mut super::super::controls::objects::PreparedWindowLayout)
                                  -> Result<Status, Status>
    {
        let window = ready.window();
        if window != 0
        {
            if !self.owns(WindowId(window))
            {
                return Err(ACCESS_DENIED);
            }
            if self.windows
                   .window(WindowId(window))
                   .is_none_or(|live| live.local_bounds() != ready.viewport())
            {
                return Err(SERVICE_BUSY);
            }
        }
        if ready.changes_geometry()
           && self.presentation
                  .pending_content
                  .iter()
                  .any(|content| content.id == SceneTarget::from(window))
        {
            return Err(SERVICE_BUSY);
        }
        let (used, limit) = self.retained_admission(SceneTarget::from(window));
        let outline = ready.feedback()
                           .map(|next| self.windows.prepare_control_outline(WindowId(window), next))
                           .transpose()?;
        let damage =
            super::window_layout::publish_prepared_window_layout(&mut self.presentation.content,
                                                                 &mut self.controls, ready, used,
                                                                 limit)?;
        let outline_changed = outline.is_some_and(|permit| permit.commit());
        self.presentation.feedback_dirty |= outline_changed;
        let changed = !damage.is_empty() || outline_changed;
        for rect in damage.into_iter().filter(|rect| !rect.is_empty())
        {
            self.content_changed_rect(window, rect);
        }
        if changed
        {
            self.present();
        }
        Ok(OK)
    }

    fn commit_user_control_operation(&mut self,
                                     ready: &mut super::super::controls::objects::PreparedControlOperation)
                                     -> Result<Status, Status>
    {
        let window = ready.window();
        if window != 0 && !self.owns(WindowId(window))
        {
            return Err(ACCESS_DENIED);
        }
        if ready.changes_tree_geometry()
           && self.presentation
                  .pending_content
                  .iter()
                  .any(|content| content.id == SceneTarget::from(window))
        {
            return Err(SERVICE_BUSY);
        }
        let (used, limit) = self.retained_admission(SceneTarget::from(window));
        let content = self.presentation
                          .pending_content
                          .iter_mut()
                          .chain(&mut self.presentation.content)
                          .find(|content| content.id == SceneTarget::from(window));
        let damage = ready.publish(&mut self.controls, |drawing| {
                              if window == 0
                              {
                                  return Err(BAD_ARGUMENT);
                              }
                              publish_drawing(content.ok_or(NOT_FOUND)?, drawing, used, limit)
                          })?;
        let call = ready.output();
        if self.controls.in_tree(call.id)
           && (damage.as_ref().is_some_and(|(bounds, _, old)| *old != Some(*bounds))
               || (call.operation == mac_abi::controls::REFLOW_OBJECT
                   && call.layout.message == mac_abi::controls::reflow::SET_VISIBLE_RECT))
        {
            self.sync_control_tree(window);
        }
        if let Some((bounds, damage, old_bounds)) = damage
        {
            if let Some(old) = old_bounds
               && old != bounds
            {
                self.content_changed_rect(window, old);
            }
            for rect in damage.into_iter().filter(|rect| !rect.is_empty())
            {
                self.content_changed_rect(window, rect);
            }
            if damage.iter().any(|rect| !rect.is_empty())
               && !self.presentation
                       .pending_content
                       .iter()
                       .any(|content| content.id == SceneTarget::from(window))
            {
                self.present();
            }
        }
        Ok(OK)
    }
}

impl Toolbox
{
    /// Canonical retirement and UI suffix share the actual original controls;
    /// detached payloads, sources and tickets are drained by the outer pump.
    pub(crate) fn retire_user_controls(&mut self) -> Result<(), Status>
    {
        let mut current = self.controls.disposal_work();
        while let Some(slot) = current
        {
            current = slot.next();
            if !slot.ui_pending()
            {
                continue;
            }
            let window = slot.window().ok_or(BAD_ABI)?;
            if slot.clear_outline()
               && !self.controls.window_splitter_capture(window)
               && self.windows.window(WindowId(window)).is_some()
            {
                if let Err(status) = self.apply_control_outline(window, None)
                {
                    slot.ui_refused();
                    return Err(status);
                }
                self.present();
            }
            self.remove_dead_control_drawings(window);
            self.controls.finish_disposal_ui(window);
        }
        Ok(())
    }
}

#[path = "automatic_runtime.rs"]
mod automatic;
pub(crate) use automatic::{AutomaticPrelude, AutomaticProgress};

/// Elect only actual User offscreen definitions. Original owner/account lookup
/// precedes guard-free loader/packet allocation; no ambient System substitution.
pub(crate) fn admit_offscreen(
    input: super::super::controls::offscreen::Input)
    -> Result<Option<super::super::controls::offscreen::Admission>, WindowBeginError>
{
    let epoch = crate::kernel::window_desktop_epoch();
    let allocation =
        input_callbacks::with_window_toolbox(|toolbox| {
            let (recipient, storage) = toolbox.service_owners()?;
            if recipient.owner() != storage.owner()
               || storage.owner().kind() != mac_object_table::domain::DomainKind::Process
               || crate::context::recovery_activity().is_some()
            {
                return Err(NOT_SUPPORTED);
            }
            let allocation = toolbox.processes.allocation(storage.owner().incarnation())?;
            if allocation.owner() != storage.owner()
            {
                return Err(ACCESS_DENIED);
            }
            Ok(allocation)
        }).map_err(|cause| {
              WindowBeginError::Desktop { cause,
                                          epoch }
          })??;
    if let Some(call) = input.control_call()
    {
        let id = super::super::controls::preflight(&call)?;
        let target =
            crate::allocator::Target::Owner { account: allocation.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        let source = crate::allocator::with_target(target, || {
            let entry =
                crate::loader::prepare_definition(*b"CDEF", id).map_err(crate::loader::status)?;
            if !super::super::resources::definitions::is_user(entry)?
            {
                return Ok(None);
            }
            super::super::resources::definitions::bind(entry).map(|bound| {
                                                                 Some(bound.into_retained_source())
                                                             })
        })?;
        let Some(source) = source
        else
        {
            return Ok(None);
        };
        return input.admit_control(source, allocation).map(Some).map_err(Into::into);
    }
    let call = input.scroll_box_call().ok_or(BAD_ARGUMENT)?;
    let plan = input_callbacks::with_window_toolbox(|toolbox| {
                   let (recipient, storage) = toolbox.service_owners()?;
                   if storage.owner() != allocation.owner() || recipient.owner() != storage.owner()
                   {
                       return Err(ACCESS_DENIED);
                   }
                   let window =
                       toolbox.scroll_boxes.window(call.id, recipient.owner().incarnation())?;
                   if window != 0 && !toolbox.owns(WindowId(window))
                   {
                       return Err(ACCESS_DENIED);
                   }
                   let fonts = crate::kernel::fonts().ok_or(NOT_SUPPORTED)?;
                   toolbox.scroll_boxes.plan_user_operation(&recipient, &storage, &call, &fonts)
               }).map_err(|cause| {
                     WindowBeginError::Desktop { cause,
                                                 epoch }
                 })??;
    match plan
    {
        Some(plan) => input.admit_scroll_box(plan, allocation).map(Some).map_err(Into::into),
        None => Ok(None)
    }
}
impl Toolbox
{
    pub(crate) fn offscreen_stage_scroll_box(
        &mut self, prepared: &mut super::super::controls::scrollbox::callbacks::Prepared,
        call: &mac_abi::scrollbox::Call)
        -> Result<super::super::controls::scrollbox::Drawing, Status>
    {
        self.scroll_boxes.stage_user_drawing(prepared, call)?;
        prepared.drawing().take().ok_or(BAD_ABI)
    }

    pub(crate) fn offscreen_validate_scroll_box(&self,
                                                prepared:&super::super::controls::scrollbox::callbacks::Prepared)
                                                -> Result<(), Status>
    {
        self.scroll_boxes.validate_user_prepared(prepared)
    }

    pub(crate) fn offscreen_paint_claim(
        &self, plan: &super::super::controls::paint::PaintPlan)
        -> Result<Option<super::super::controls::paint::producer::PaintClaim>, Status>
    {
        use super::super::controls::paint::{PaintOrigin, producer::PaintClaim};
        match plan.origin()
        {
            PaintOrigin::Stateless => Ok(None),
            PaintOrigin::ScrollBox(origin) =>
            {
                self.scroll_boxes
                    .claim_scroll_box_paint(&origin, plan.source())
                    .map(PaintClaim::ScrollBox)
                    .map(Some)
            }
            PaintOrigin::Managed(_) => Err(ACCESS_DENIED)
        }
    }

    pub(crate) fn offscreen_capture(&self,
                                    loan: &mut super::super::controls::paint::producer::PaintLoan,
                                    canvas: &mut Canvas<'_>)
                                    -> Result<(), Status>
    {
        loan.capture(&self.windows, &self.controls, &self.scroll_boxes, canvas)
            .map_err(|failure| failure.status())
    }

    pub(crate) fn offscreen_replay(&self,
                                   loan: &mut super::super::controls::paint::producer::PaintLoan,
                                   canvas: &mut Canvas<'_>)
                                   -> Result<(), Status>
    {
        loan.replay(&self.windows, &self.controls, &self.scroll_boxes, canvas)
            .map(|_| ())
            .map_err(|failure| failure.status())
    }
}

pub(crate) fn offscreen_toolbox<R>(run: impl FnOnce(&mut Toolbox) -> R)
                                   -> Result<R, crate::state::AccessError>
{
    input_callbacks::with_window_toolbox(run)
}

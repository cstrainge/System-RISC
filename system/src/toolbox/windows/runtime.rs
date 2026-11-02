//! Native window admission. Shared scene storage is detached from Desktop;
//! immutable pixels and queued damage remain owned by the normal compositor.
#![forbid(unsafe_code)]

use core::{alloc::Layout, fmt};

use super::{super::windows::manager::{CreationError, PreparedWindow, WindowCommit, WindowPlan},
            *};
use crate::{machine::scheduler::accounting::AllocationAuthority,
            toolbox::drag::{PreparedReceiver, ReceiverError, ReceiverPlan, ReceiverRetired,
                            ReceiverTarget}};

pub(super) struct NativeWindowPublication<'a>
{
    prepared:         &'a mut Option<PreparedWindow>,
    retired:          &'a mut Option<WindowCommit>,
    refusal:          &'a mut Option<CreationError>,
    receiver:         &'a mut Option<PreparedReceiver>,
    receiver_retired: &'a mut Option<ReceiverRetired>
}

#[derive(Debug)]
enum AdmissionError
{
    Desktop(crate::state::AccessError),
    Service
    {
        stage:  &'static str,
        status: Status
    },
    Scene(SceneCapacityError),
    Window(CreationError),
    Receiver(ReceiverError),
    Definition(crate::loader::LoadError)
}
impl AdmissionError
{
    fn status(self) -> Status
    {
        match self
        {
            Self::Desktop(cause) => cause.status(),
            Self::Service { status,
                            .. } => status,
            Self::Scene(cause) => cause.status(),
            Self::Window(cause) => window_creation_status(cause),
            Self::Receiver(cause) => cause.status(),
            Self::Definition(cause) => crate::loader::status(cause)
        }
    }
}
impl fmt::Display for AdmissionError
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Desktop(cause) =>
            {
                match cause
                {
                    crate::state::AccessError::Unavailable =>
                    {
                        out.write_str("resident Desktop is unavailable")
                    }
                    crate::state::AccessError::Busy =>
                    {
                        out.write_str("resident Desktop is busy; no window admission was lost")
                    }
                    crate::state::AccessError::WrongOwner =>
                    {
                        out.write_str("window service entered from the wrong execution owner")
                    }
                }
            }
            Self::Service { stage,
                            status } => write!(out, "window {stage} refused with status {status}"),
            Self::Scene(cause) => cause.fmt(out),
            Self::Window(cause) => cause.fmt(out),
            Self::Receiver(cause) => cause.fmt(out),
            Self::Definition(cause) => cause.fmt(out)
        }
    }
}
fn desktop<T>(run: impl FnOnce(&mut Desktop) -> Result<T, AdmissionError>)
              -> Result<T, AdmissionError>
{
    crate::kernel::with_desktop(run).map_err(AdmissionError::Desktop)?
}
fn capacity_changed(cause: CreationError) -> bool
{
    matches!(cause,
             CreationError::CapacityChanged
             | CreationError::Publication(mac_object_table::store::PreparedError::CapacityChanged))
}
struct ProcessPlan
{
    window:     WindowPlan,
    receiver:   ReceiverPlan,
    scene:      SceneCapacityPlan,
    allocation: AllocationAuthority
}
enum Turn
{
    Cold(i16),
    Internal,
    Process(ProcessPlan)
}
impl Desktop
{
    fn plan_native_window(&self, request: &WindowRequest) -> Result<Turn, AdmissionError>
    {
        let id = self.preflight_window(NEW_WINDOW, request)
                     .map_err(|status| {
                         AdmissionError::Service { stage: "request preflight",
                                                   status }
                     })?
                     .ok_or(AdmissionError::Service { stage:  "definition intent",
                                                      status: BAD_ABI })?;
        let Some(entry) =
            crate::loader::cached_definition(*b"WDEF", id).map_err(AdmissionError::Definition)?
        else
        {
            return Ok(Turn::Cold(id));
        };
        if crate::context::recovery_activity().is_some()
        {
            return Ok(Turn::Internal);
        }
        let (recipient, storage) =
            self.service_owners().map_err(|status| {
                                      AdmissionError::Service { stage: "actual service owner",
                                                                status }
                                  })?;
        if recipient.owner() != storage.owner()
        {
            return Err(AdmissionError::Service { stage:  "Process storage owner",
                                                 status: ACCESS_DENIED });
        }
        let allocation = self.processes.allocation(self.owner)
            .map_err(|status| AdmissionError::Service { stage: "original Process account", status })?;
        let window = self.windows
                         .plan_process_window(&storage, request, entry)
                         .map_err(AdmissionError::Window)?;
        let receiver =
            self.drag
                .plan_receiver(&recipient,
                               crate::context::current_thread_context().identity().thread,
                               &allocation)
                .map_err(AdmissionError::Receiver)?;
        Ok(Turn::Process(ProcessPlan { window,
                                       receiver,
                                       scene: self.plan_window_scene()
                                                  .map_err(AdmissionError::Scene)?,
                                       allocation }))
    }
}

/// The normal native WINDOW trap. All phases use the actual current Process;
/// no host service substitute, public token or temporary global staging slot.
pub(crate) fn native_window(operation: u32, request: &mut WindowRequest) -> Status
{
    if operation != NEW_WINDOW
    {
        // Preserve the direct command path and its existing diagnostics. A
        // transient busy query is not a window-admission failure or a reason
        // to write UART on each poll/input event.
        return crate::kernel::with_desktop(|desktop| desktop.window(operation, request))
            .unwrap_or_else(|cause| cause.status());
    }
    match create_native(request)
    {
        Ok(status) => status,
        Err(cause) =>
        {
            serial!("WINDOW_ADMISSION_REFUSED: {cause}");
            cause.status()
        }
    }
}
fn create_native(request: &mut WindowRequest) -> Result<Status, AdmissionError>
{
    let mut cold_prepared = false;
    // Before an actual permit is issued, contention returns the complete
    // unadmitted preparation for disposal and a fresh snapshot outside Desktop.
    let (mut prepared, mut scene, mut receiver) = loop
    {
        let plan = match desktop(|desktop| desktop.plan_native_window(request))?
        {
            Turn::Cold(id) if !cold_prepared =>
            {
                crate::loader::prepare_definition(*b"WDEF", id)
                    .map_err(AdmissionError::Definition)?;
                cold_prepared = true;
                continue;
            }
            Turn::Cold(_) =>
            {
                return Err(AdmissionError::Service { stage:  "definition source changed during \
                                                              preparation",
                                                     status: SERVICE_BUSY });
            }
            Turn::Internal => return desktop(|desktop| Ok(desktop.window(NEW_WINDOW, request))),
            Turn::Process(plan) => plan
        };
        let mut input =
            Some(plan.window.prepare(&plan.allocation).map_err(AdmissionError::Window)?);
        let scene = plan.scene.prepare().map_err(AdmissionError::Scene)?;
        let admitted = desktop(|desktop| {
            let value = input.as_ref()
                             .ok_or(AdmissionError::Service { stage:  "unpublished window \
                                                                       preparation",
                                                              status: BAD_ABI })?;
            if value.owner().owner().incarnation() != desktop.owner
               || !value.matches_request(request)
            {
                return Err(AdmissionError::Service { stage:  "original Process intent changed",
                                                     status: ACCESS_DENIED });
            }
            desktop.windows.admit_process_window(&mut input).map_err(AdmissionError::Window)
        });
        if let Some(input) = input
        {
            input.dispose();
        }
        match admitted
        {
            Ok(value) =>
            {
                let receiver = super::drag_runtime::admit_receiver_plan(plan.receiver)
                    .map_err(AdmissionError::Receiver)?;
                break (Some(value), scene, Some(receiver));
            }
            Err(AdmissionError::Window(cause)) if capacity_changed(cause) => continue,
            Err(cause) => return Err(cause)
        }
    };
    loop
    {
        let mut retired = None;
        let mut receiver_retired = None;
        let mut refusal = None;
        let result = desktop(|desktop| {
            let value = prepared.as_ref()
                                .ok_or(AdmissionError::Service { stage:  "canonical window permit",
                                                                 status: BAD_ABI })?;
            if value.owner().owner().incarnation() != desktop.owner
               || !value.matches_request(request)
            {
                return Err(AdmissionError::Service { stage:  "admitted Process intent changed",
                                                     status: ACCESS_DENIED });
            }
            desktop.install_window_scene(&mut scene).map_err(AdmissionError::Scene)?;
            Ok(desktop.create_window(request,
                                     Some(NativeWindowPublication { prepared:         &mut prepared,
                                                                    retired:          &mut retired,
                                                                    refusal:          &mut refusal,
                                                                    receiver:         &mut receiver,
                                                                    receiver_retired:
                                                                        &mut receiver_retired })))
        });
        if let Some(retired) = retired
        {
            retired.dispose();
        }
        if let Some(retired) = receiver_retired
        {
            retired.dispose();
        }
        let refresh = match result
        {
            Err(AdmissionError::Scene(SceneCapacityError::Changed { .. })) => true,
            Ok(_) if refusal.is_some_and(capacity_changed) => true,
            result => return result
        };
        if refresh
        {
            // Preserve the exact genuine permit and payload. Reprepare only
            // fresh capacity; no second ID or fallback publisher is admitted.
            let (window_plan, scene_plan) = desktop(|desktop| {
                let value = prepared.as_ref()
                                    .ok_or(AdmissionError::Service { stage:  "retained window \
                                                                              permit",
                                                                     status: BAD_ABI })?;
                if value.owner().owner().incarnation() != desktop.owner
                   || !value.matches_request(request)
                {
                    return Err(AdmissionError::Service { stage:  "retained Process intent changed",
                                                         status: ACCESS_DENIED });
                }
                Ok((value.plan_capacity(&desktop.windows).map_err(AdmissionError::Window)?,
                    desktop.plan_window_scene().map_err(AdmissionError::Scene)?))
            })?;
            let capacity = window_plan.prepare().map_err(AdmissionError::Window)?;
            let next_scene = scene_plan.prepare().map_err(AdmissionError::Scene)?;
            let value = prepared.as_mut()
                                .ok_or(AdmissionError::Service { stage:  "retained window backing",
                                                                 status: BAD_ABI })?;
            value.replace_capacity(capacity).dispose();
            scene = next_scene;
        }
    }
}

#[derive(Debug)]
enum SceneCapacityError
{
    Geometry,
    Storage
    {
        stage:   &'static str,
        entries: usize
    },
    Changed
    {
        stage:     &'static str,
        required:  usize,
        available: usize
    }
}
impl SceneCapacityError
{
    fn status(&self) -> Status
    {
        match self
        {
            Self::Geometry => BAD_ARGUMENT,
            Self::Storage { .. } => NO_MEMORY,
            Self::Changed { .. } => SERVICE_BUSY
        }
    }
}
impl fmt::Display for SceneCapacityError
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Geometry =>
            {
                out.write_str("window scene capacity geometry overflows; original scene retained")
            }
            Self::Storage { stage,
                            entries } =>
            {
                write!(out,
                       "window {stage} storage refused for {entries} entries; no window published")
            }
            Self::Changed { stage,
                            required,
                            available } =>
            {
                write!(out,
                       "window {stage} now needs {required} entries but prepared storage holds \
                        {available}; retain original preparation and retry")
            }
        }
    }
}

struct SceneCapacityPlan
{
    content:            usize,
    pending:            usize,
    damage:             usize,
    deferred:           usize,
    deferred_available: usize
}
/// Fresh or displaced arrays only. Dispose after leaving Desktop; queued
/// Content/Region values move once rather than being cloned or discarded.
struct SceneCapacity
{
    content:  Vec<Content>,
    pending:  Vec<Content>,
    damage:   Vec<(SceneTarget, super::super::quickdraw::region::Region)>,
    deferred: Vec<(SceneTarget, super::super::quickdraw::region::Region)>
}
fn empty_storage<T>(entries: usize, stage: &'static str) -> Result<Vec<T>, SceneCapacityError>
{
    Layout::array::<T>(entries).map_err(|_| SceneCapacityError::Geometry)?;
    let mut values = Vec::new();
    values.try_reserve_exact(entries).map_err(|_| {
                                          SceneCapacityError::Storage { stage,
                                                                        entries }
                                      })?;
    Ok(values)
}
impl SceneCapacityPlan
{
    fn prepare(self) -> Result<SceneCapacity, SceneCapacityError>
    {
        // The actual caller's source is retained. A normal app cannot borrow
        // protected storage; a recovery Finder's genuine default Permit can.
        // Fresh arrays avoid reallocating an old ordinary/protected header.
        InvocationGate::with_system_allocation(|| {
            let content = empty_storage(self.content, "content directory")?;
            // Vec may provide more capacity than reserve_exact requested.
            // Account for the actual fresh directory before preparing its
            // dependent queue, rather than retrying the same undersized plan.
            let deferred =
                self.deferred
                    .max(content.capacity().checked_add(1).ok_or(SceneCapacityError::Geometry)?);
            Ok(SceneCapacity { content,
                               pending: empty_storage(self.pending,
                                                      "pending content directory")?,
                               damage: empty_storage(self.damage, "damage directory")?,
                               deferred: empty_storage(if self.deferred_available < deferred
                                                       {
                                                           deferred
                                                       }
                                                       else
                                                       {
                                                           0
                                                       },
                                                       "deferred damage directory")? })
        })
    }
}
fn needs_storage<T>(values: &Vec<T>, required: usize) -> usize
{
    if values.capacity() < required { required } else { 0 }
}
fn sufficient<T>(live: &Vec<T>, prepared: &Vec<T>, required: usize, stage: &'static str)
                 -> Result<(), SceneCapacityError>
{
    if live.capacity() < required && prepared.capacity() < required
    {
        return Err(SceneCapacityError::Changed { stage,
                                                 required,
                                                 available: prepared.capacity() });
    }
    Ok(())
}
fn install_storage<T>(live: &mut Vec<T>, prepared: &mut Vec<T>, required: usize)
{
    if live.capacity() < required
    {
        // The complete set of capacities was checked before the first move.
        // Required includes every existing entry. Neither operation allocates
        // or destroys the real queued content/damage values.
        prepared.extend(live.drain(..));
        core::mem::swap(live, prepared);
    }
}
impl Desktop
{
    fn scene_requirements(&self) -> Result<(usize, usize), SceneCapacityError>
    {
        let content = self.content
                          .len()
                          .checked_add(1)
                          .and_then(|n| n.checked_add(self.scene_spare()))
                          .ok_or(SceneCapacityError::Geometry)?;
        let damage = content.checked_add(1)
                            .ok_or(SceneCapacityError::Geometry)?
                            .max(self.content_damage.len());
        Ok((content, damage))
    }

    fn plan_window_scene(&self) -> Result<SceneCapacityPlan, SceneCapacityError>
    {
        let (content, damage) = self.scene_requirements()?;
        let deferred = self.content
                           .capacity()
                           .max(content)
                           .checked_add(1)
                           .ok_or(SceneCapacityError::Geometry)?
                           .max(self.ordinary_underlay.deferred.len());
        Ok(SceneCapacityPlan { content: needs_storage(&self.content, content),
                               // BEGIN_UPDATE admits at most one pending scene per live window.
                               // Reserve their directory at window birth, including modal spare,
                               // rather than asking its retained old header to grow during
                               // paint.
                               pending: needs_storage(&self.pending_content,
                                                      content.max(self.pending_content.len())),
                               damage: needs_storage(&self.content_damage, damage),
                               deferred,
                               deferred_available: self.ordinary_underlay.deferred.capacity() })
    }

    fn install_window_scene(&mut self, prepared: &mut SceneCapacity)
                            -> Result<(), SceneCapacityError>
    {
        let (content, damage) = self.scene_requirements()?;
        sufficient(&self.content, &prepared.content, content, "content directory")?;
        let pending = content.max(self.pending_content.len());
        sufficient(&self.pending_content, &prepared.pending, pending, "pending content directory")?;
        sufficient(&self.content_damage, &prepared.damage, damage, "damage directory")?;
        let content_capacity = if self.content.capacity() >= content
        {
            self.content.capacity()
        }
        else
        {
            prepared.content.capacity()
        };
        let deferred = content_capacity.checked_add(1)
                                       .ok_or(SceneCapacityError::Geometry)?
                                       .max(self.ordinary_underlay.deferred.len());
        sufficient(&self.ordinary_underlay.deferred, &prepared.deferred, deferred,
                   "deferred damage directory")?;
        // No identity is published here. Validate all capacities first so a
        // competing publisher cannot make a partially installed scene visible.
        install_storage(&mut self.content, &mut prepared.content, content);
        install_storage(&mut self.pending_content, &mut prepared.pending, pending);
        install_storage(&mut self.content_damage, &mut prepared.damage, damage);
        install_storage(&mut self.ordinary_underlay.deferred, &mut prepared.deferred, deferred);
        Ok(())
    }
}

impl Desktop
{
    /// One initialization path for prepared application windows and the real
    /// internal dialog callers. A preparation refusal never selects another
    /// publisher. Retired publication owners travel back outside Desktop.
    pub(super) fn create_window(&mut self, request: &mut WindowRequest,
                                mut publication: Option<NativeWindowPublication<'_>>)
                                -> Status
    {
        crate::trace::record(crate::trace::WINDOW, self.owner, NEW_WINDOW as u64, request.id, 0, 0);
        let when = Self::when();
        let definition = match self.preflight_window(NEW_WINDOW, request)
        {
            Ok(Some(id)) => id,
            Ok(None) => return BAD_ABI,
            Err(status) => return status
        };
        let title = match core::str::from_utf8(&request.title[..request.title_length as usize])
        {
            Ok(title) => title,
            Err(_) => return BAD_ARGUMENT
        };
        let entry = match crate::loader::cached_definition(*b"WDEF", definition).and_then(|entry| {
                              entry.ok_or_else(|| {
                                       mac_formats::Error::Missing("window definition was not \
                                                                    prepared before object \
                                                                    mutation").into()
                                   })
                          })
        {
            Ok(entry) => entry,
            Err(error) => return crate::loader::status(error)
        };
        // Capture logical reservation use before the shared allocator
        // suppresses recovery authority. The real modal may consume its
        // admitted slots; ordinary publishers must preserve that spare.
        let spare = self.scene_spare();
        if publication.is_none()
           && shared_scene_allocation(|| {
                  self.content.try_reserve(1 + spare).map_err(|_| NO_MEMORY)?;
                  self.ordinary_underlay
                      .prepare_scenes(self.content.capacity().saturating_add(1))?;
                  self.content_damage
                      .try_reserve(self.content
                                       .len()
                                       .saturating_add(2 + spare)
                                       .saturating_sub(self.content_damage.len()))
                      .map_err(|_| NO_MEMORY)
              }).is_err()
        {
            return NO_MEMORY;
        }
        let (recipient, storage) = match self.service_owners()
        {
            Ok(owners) => owners,
            Err(status) => return status
        };
        let creation = if let Some(publication) = publication.as_mut()
        {
            if publication.retired.is_some()
            {
                return BAD_ABI;
            }
            match self.windows
                      .commit_process_window(publication.prepared, &mut self.events, when, false)
            {
                Err(cause) =>
                {
                    *publication.refusal = Some(cause);
                    Err(cause)
                }
                Ok(complete) =>
                {
                    let result = complete.result;
                    *publication.retired = Some(complete);
                    result
                }
            }
        }
        else
        {
            self.windows.new_window_with_activation(request.bounds,
                                                    title,
                                                    &storage,
                                                    (request.minimum_width, request.minimum_height),
                                                    request.procedure,
                                                    entry,
                                                    &mut self.events,
                                                    when,
                                                    false)
        };
        let id = match creation
        {
            Ok(id) => id,
            Err(error) =>
            {
                if !capacity_changed(error)
                {
                    serial!("WINDOW_INITIALIZATION_REFUSED: owner={} {error}", self.owner);
                }
                return window_creation_status(error);
            }
        };
        if crate::context::recovery_activity().is_none()
        {
            let receiver = if let Some(publication) = publication.as_mut()
            {
                if publication.receiver_retired.is_some()
                {
                    Err(BAD_ABI)
                }
                else
                {
                    // WIND has actually been born and initialized. Only this
                    // real ID may become the prepared receiver's target.
                    match self.drag
                              .commit_receiver(publication.receiver, ReceiverTarget::Window(id))
                    {
                        Ok(retired) =>
                        {
                            *publication.receiver_retired = retired;
                            Ok(())
                        }
                        Err(cause) =>
                        {
                            serial!("WINDOW_RECEIVER_BINDING_REFUSED: owner={} {cause}",
                                    self.owner);
                            Err(cause.status())
                        }
                    }
                }
            }
            else
            {
                self.drag
                    .ensure_receiver(&recipient,
                                     crate::context::current_thread_context().identity().thread,
                                     id.0)
            };
            if let Err(status) = receiver
            {
                if let Err(error) = self.windows.dispose_window(id, &mut self.events, when)
                {
                    serial!("WINDOW_INITIALIZATION_ROLLBACK_REFUSED: owner={} {error}", self.owner);
                }
                return status;
            }
        }
        request.id = id.0;
        if self.first_window.0 == 0
        {
            self.first_window = id;
        }
        if self.owner == self.foreground
        {
            self.windows.select_window(id, &mut self.events, when);
        }
        if self.owner != self.foreground
        {
            if self.processes.get(self.owner).is_ok_and(|p| p.info.hidden != 0)
            {
                self.windows.set_hidden(id, true);
            }
            self.windows.restore_owner_group(self.foreground, &mut self.events, when);
        }
        self.content.push(Content::new(id));
        self.last_action = "NEW";
        self.route_events();
        self.redraw = true;
        OK
    }
}

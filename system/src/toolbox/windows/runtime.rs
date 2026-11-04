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
    Toolbox(crate::state::AccessError),
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
            Self::Toolbox(cause) => cause.status(),
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
            Self::Toolbox(cause) =>
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
fn toolbox<T>(run: impl FnOnce(&mut Toolbox) -> Result<T, AdmissionError>)
              -> Result<T, AdmissionError>
{
    crate::kernel::with_toolbox(run).map_err(AdmissionError::Toolbox)?
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
impl Toolbox
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
        return crate::kernel::with_toolbox(|toolbox| toolbox.window(operation, request))
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
struct PreparedNative
{
    prepared: Option<PreparedWindow>,
    scene:    SceneCapacity,
    receiver: Option<PreparedReceiver>
}
enum Preparation
{
    Internal,
    Process(PreparedNative)
}

/// The original request holds only a handle. Canonical unpublished custody was
/// admitted before author NEW; Drop cannot lose its permit/Pending/author state.
pub(crate) struct UserBirth
{
    value: PreparedNative,
    owner: alloc::sync::Arc<crate::toolbox::windows::manager::birth::BirthOwner>
}
pub(crate) enum BirthProgress
{
    Wait,
    Complete(Status)
}
impl Drop for UserBirth
{
    fn drop(&mut self)
    {
        self.owner.abandon();
    }
}
impl UserBirth
{
    pub(crate) fn begin(request: &WindowRequest) -> Result<Self, Status>
    {
        let Preparation::Process(mut value) =
            prepare_native(request).map_err(AdmissionError::status)?
        else
        {
            return Err(ACCESS_DENIED);
        };
        let notice = toolbox(|toolbox| Ok(toolbox.windows.operation_notice())).map_err(AdmissionError::status)?;
        let prepared = value.prepared.take().ok_or(BAD_ABI)?;
        let owner = crate::toolbox::windows::manager::birth::BirthOwner::prepare(prepared,
            Some(crate::context::current_thread_context().identity().thread), notice)?;
        loop
        {
            let admitted = toolbox(|toolbox| {
                if owner.storage_owner().incarnation() != toolbox.owner
                {
                    return Err(AdmissionError::Service { stage:  "original birth recipient",
                                                         status: ACCESS_DENIED });
                }
                toolbox.windows.admit_birth_owner(&owner, request).map_err(AdmissionError::Window)
            });
            match admitted
            {
                Ok(()) => break,
                Err(AdmissionError::Window(cause)) if capacity_changed(cause) =>
                {
                    let mut loan = owner.preparation()?;
                    let plan = toolbox(|toolbox| {
                                   loan.input()
                                       .as_ref()
                                       .ok_or(AdmissionError::Service { stage:  "original birth \
                                                                                 preparation",
                                                                        status: BAD_ABI })?
                                       .plan_capacity(&toolbox.windows)
                                       .map_err(AdmissionError::Window)
                               }).map_err(AdmissionError::status)?;
                    let capacity = plan.prepare().map_err(window_creation_status)?;
                    loan.input().as_mut().ok_or(BAD_ABI)?.replace_capacity(capacity).dispose();
                }
                Err(cause) => return Err(cause.status())
            }
        }
        Ok(Self { value,
                  owner })
    }

    pub(crate) fn current_job(
        &self)
        -> Option<alloc::sync::Arc<crate::toolbox::windows::manager::operations::WindowJob>>
    {
        self.owner.current_job()
    }

    pub(crate) fn advance(&mut self, request: &mut WindowRequest) -> Result<BirthProgress, Status>
    {
        use crate::toolbox::windows::manager::birth::CreationProgress;
        loop
        {
            match self.owner.advance_creation().map_err(window_creation_status)?
            {
                CreationProgress::Continue => continue,
                CreationProgress::Wait => return Ok(BirthProgress::Wait),
                CreationProgress::Ready =>
                {
                    let mut loan = self.owner.publication()?;
                    core::mem::swap(loan.input(), &mut self.value.prepared);
                    let result = commit_native(&mut self.value, request);
                    core::mem::swap(loan.input(), &mut self.value.prepared);
                    if loan.input().is_none()
                    {
                        loan.published()?;
                    }
                    return result.map(BirthProgress::Complete).map_err(AdmissionError::status);
                }
            }
        }
    }
}
fn create_native(request: &mut WindowRequest) -> Result<Status, AdmissionError>
{
    match prepare_native(request)?
    {
        Preparation::Internal => toolbox(|toolbox| Ok(toolbox.window(NEW_WINDOW, request))),
        Preparation::Process(mut prepared) => commit_native(&mut prepared, request)
    }
}
fn prepare_native(request: &WindowRequest) -> Result<Preparation, AdmissionError>
{
    let mut cold_prepared = false;
    // Before an actual permit is issued, contention returns the complete
    // unadmitted preparation for disposal and a fresh snapshot outside Desktop.
    loop
    {
        let plan = match toolbox(|toolbox| toolbox.plan_native_window(request))?
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
            Turn::Internal => return Ok(Preparation::Internal),
            Turn::Process(plan) => plan
        };
        let mut input =
            Some(plan.window.prepare(&plan.allocation).map_err(AdmissionError::Window)?);
        let scene = plan.scene.prepare().map_err(AdmissionError::Scene)?;
        let admitted = toolbox(|toolbox| {
            let value = input.as_ref()
                             .ok_or(AdmissionError::Service { stage:  "unpublished window \
                                                                       preparation",
                                                              status: BAD_ABI })?;
            if value.owner().owner().incarnation() != toolbox.owner
               || !value.matches_request(request)
            {
                return Err(AdmissionError::Service { stage:  "original Process intent changed",
                                                     status: ACCESS_DENIED });
            }
            toolbox.windows.admit_process_window(&mut input).map_err(AdmissionError::Window)
        });
        if let Some(input) = input
        {
            input.dispose();
        }
        match admitted
        {
            Ok(mut value) =>
            {
                let notice = toolbox(|toolbox| Ok(toolbox.windows.operation_notice()))?;
                value.prepare_trusted_cleanup(notice).map_err(|status| {
                                                          AdmissionError::Service {
                    stage: "trusted close custody", status }
                                                      })?;
                let receiver = super::drag_runtime::admit_receiver_plan(plan.receiver)
                    .map_err(AdmissionError::Receiver)?;
                return Ok(Preparation::Process(PreparedNative { prepared: Some(value),
                                                                scene,
                                                                receiver: Some(receiver) }));
            }
            Err(AdmissionError::Window(cause)) if capacity_changed(cause) => continue,
            Err(cause) => return Err(cause)
        }
    }
}
fn commit_native(value: &mut PreparedNative, request: &mut WindowRequest)
                 -> Result<Status, AdmissionError>
{
    let PreparedNative { prepared,
                         scene,
                         receiver } = value;
    loop
    {
        let mut retired = None;
        let mut receiver_retired = None;
        let mut refusal = None;
        let result = toolbox(|toolbox| {
            let value = prepared.as_ref()
                                .ok_or(AdmissionError::Service { stage:  "canonical window permit",
                                                                 status: BAD_ABI })?;
            if value.owner().owner().incarnation() != toolbox.owner
               || !value.matches_request(request)
            {
                return Err(AdmissionError::Service { stage:  "admitted Process intent changed",
                                                     status: ACCESS_DENIED });
            }
            toolbox.install_window_scene(scene).map_err(AdmissionError::Scene)?;
            Ok(toolbox.create_window(request,
                                     Some(NativeWindowPublication { prepared,
                                                                    retired: &mut retired,
                                                                    refusal: &mut refusal,
                                                                    receiver,
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
            let (window_plan, scene_plan) = toolbox(|toolbox| {
                let value = prepared.as_ref()
                                    .ok_or(AdmissionError::Service { stage:  "retained window \
                                                                              permit",
                                                                     status: BAD_ABI })?;
                if value.owner().owner().incarnation() != toolbox.owner
                   || !value.matches_request(request)
                {
                    return Err(AdmissionError::Service { stage:  "retained Process intent changed",
                                                         status: ACCESS_DENIED });
                }
                Ok((value.plan_capacity(&toolbox.windows).map_err(AdmissionError::Window)?,
                    toolbox.plan_window_scene().map_err(AdmissionError::Scene)?))
            })?;
            let capacity = window_plan.prepare().map_err(AdmissionError::Window)?;
            let next_scene = scene_plan.prepare().map_err(AdmissionError::Scene)?;
            let value = prepared.as_mut()
                                .ok_or(AdmissionError::Service { stage:  "retained window backing",
                                                                 status: BAD_ABI })?;
            value.replace_capacity(capacity).dispose();
            *scene = next_scene;
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
impl Toolbox
{
    fn scene_requirements(&self) -> Result<(usize, usize), SceneCapacityError>
    {
        let content = self.presentation
                          .content
                          .len()
                          .checked_add(1)
                          .and_then(|n| n.checked_add(self.scene_spare()))
                          .ok_or(SceneCapacityError::Geometry)?;
        let damage = content.checked_add(1)
                            .ok_or(SceneCapacityError::Geometry)?
                            .max(self.presentation.content_damage.len());
        Ok((content, damage))
    }

    fn plan_window_scene(&self) -> Result<SceneCapacityPlan, SceneCapacityError>
    {
        let (content, damage) = self.scene_requirements()?;
        let deferred = self.presentation
                           .content
                           .capacity()
                           .max(content)
                           .checked_add(1)
                           .ok_or(SceneCapacityError::Geometry)?
                           .max(self.presentation.ordinary_underlay.deferred.len());
        Ok(SceneCapacityPlan { content: needs_storage(&self.presentation.content, content),
                               // BEGIN_UPDATE admits at most one pending scene per live window.
                               // Reserve their directory at window birth, including modal spare,
                               // rather than asking its retained old header to grow during
                               // paint.
                               pending: needs_storage(&self.presentation.pending_content,
                                                      content.max(self.presentation
                                                                      .pending_content
                                                                      .len())),
                               damage: needs_storage(&self.presentation.content_damage, damage),
                               deferred,
                               deferred_available: self.presentation
                                                       .ordinary_underlay
                                                       .deferred
                                                       .capacity() })
    }

    fn install_window_scene(&mut self, prepared: &mut SceneCapacity)
                            -> Result<(), SceneCapacityError>
    {
        let (content, damage) = self.scene_requirements()?;
        sufficient(&self.presentation.content, &prepared.content, content, "content directory")?;
        let pending = content.max(self.presentation.pending_content.len());
        sufficient(&self.presentation.pending_content, &prepared.pending, pending,
                   "pending content directory")?;
        sufficient(&self.presentation.content_damage, &prepared.damage, damage,
                   "damage directory")?;
        let content_capacity = if self.presentation.content.capacity() >= content
        {
            self.presentation.content.capacity()
        }
        else
        {
            prepared.content.capacity()
        };
        let deferred = content_capacity.checked_add(1)
                                       .ok_or(SceneCapacityError::Geometry)?
                                       .max(self.presentation.ordinary_underlay.deferred.len());
        sufficient(&self.presentation.ordinary_underlay.deferred, &prepared.deferred, deferred,
                   "deferred damage directory")?;
        // No identity is published here. Validate all capacities first so a
        // competing publisher cannot make a partially installed scene visible.
        install_storage(&mut self.presentation.content, &mut prepared.content, content);
        install_storage(&mut self.presentation.pending_content, &mut prepared.pending, pending);
        install_storage(&mut self.presentation.content_damage, &mut prepared.damage, damage);
        install_storage(&mut self.presentation.ordinary_underlay.deferred, &mut prepared.deferred,
                        deferred);
        Ok(())
    }
}

impl Toolbox
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
                  self.presentation.content.try_reserve(1 + spare).map_err(|_| NO_MEMORY)?;
                  self.presentation
                      .ordinary_underlay
                      .prepare_scenes(self.presentation.content.capacity().saturating_add(1))?;
                  self.presentation
                      .content_damage
                      .try_reserve(self.presentation
                                       .content
                                       .len()
                                       .saturating_add(2 + spare)
                                       .saturating_sub(self.presentation.content_damage.len()))
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
        let owner_close = if publication.is_none()
                             && storage.owner().kind()
                                != mac_object_table::domain::DomainKind::System
                             && !match crate::toolbox::resources::definitions::is_user(entry)
                             {
                                 Ok(user) => user,
                                 Err(status) => return status
                             }
        {
            let allocation = match crate::context::capture_allocation()
            {
                Ok(crate::allocation::Target::Owner { account,
                                                      .. })
                    if account.owner() == storage.owner() =>
                {
                    account
                }
                _ =>
                {
                    match self.processes.allocation(storage.owner().incarnation())
                    {
                        Ok(account) if account.owner() == storage.owner() => account,
                        Ok(_) => return ACCESS_DENIED,
                        Err(status) => return status
                    }
                }
            };
            let header = match crate::toolbox::windows::manager::trusted::TrustedCloseHeader::prepare_owner(
                &storage, &allocation, self.windows.operation_notice())
            { Ok(header) => header, Err(status) => return status };
            match header.prepare_binding()
            {
                Ok(binding) => Some(binding),
                Err(status) => return status
            }
        }
        else
        {
            None
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
                                                    false,
                                                    request.reserved == WINDOW_STAY_IN_BACK,
                                                    owner_close)
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
        self.presentation.content.push(Content::new(id));
        self.last_action = "NEW";
        self.route_events();
        self.presentation.redraw = true;
        OK
    }
}

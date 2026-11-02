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

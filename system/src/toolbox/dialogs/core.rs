//! Immutable, resource-backed templates for the guaranteed System alert set.
#![forbid(unsafe_code)]
use alloc::{string::String, sync::Arc, vec::Vec};
use core::fmt;

use mac_abi::{dialogs::*, geometry::Rect, resources, *};
use mac_object_table::store::PublicationError;

use super::{Alert, Item, modal::DialogState};
use crate::{recovery::Activity,
            toolbox::{handles::OwnerRef,
                      quickdraw::{icons_service, service}}};

const IDS: [i16; 3] = [CORE_ERROR_ALERT, CORE_OUT_OF_MEMORY_ALERT, CORE_SAVE_CHANGES_ALERT];
struct Templates
{
    alerts: [Alert; IDS.len()],
    icons:  Vec<(i16, icons_service::Drawing)>
}
static TEMPLATES: spin::Once<Templates> = spin::Once::new();

pub(crate) fn supported(scope: resources::Scope, id: i16) -> bool
{
    scope == resources::SYSTEM && IDS.contains(&id)
}

pub(crate) fn required_control_count() -> Result<usize, Status>
{
    Ok(TEMPLATES.get()
                .ok_or(NOT_SUPPORTED)?
                .alerts
                .iter()
                .map(|alert| alert.items.iter().filter(|item| item.kind == 4).count())
                .max()
                .unwrap_or(0))
}

pub(crate) fn prepare() -> Result<(), Status>
{
    TEMPLATES.try_call_once(|| {
                 let load = |id| {
                     let mut call = DialogCall { scope: resources::SYSTEM,
                                                 resource: id,
                                                 text_length: 2,
                                                 ..Default::default() };
                     call.text[..2].copy_from_slice(b"^0");
                     crate::kernel::with_resources(|resources| super::load(resources, &call))
                .ok_or(NOT_SUPPORTED)?
                 };
                 let alerts = [load(IDS[0])?, load(IDS[1])?, load(IDS[2])?];
                 let mut icons = Vec::new();
                 for item in
                     alerts.iter().flat_map(|alert| &alert.items).filter(|item| item.kind == 32)
                 {
                     if icons.iter().any(|(id, _)| *id == item.icon)
                     {
                         continue;
                     }
                     icons.try_reserve(1).map_err(|_| NO_MEMORY)?;
                     let call = mac_abi::quickdraw::IconCall { scope: resources::SYSTEM,
                                                               id: item.icon,
                                                               destination: item.bounds,
                                                               ..Default::default() };
                     let drawing =
                         crate::kernel::with_resources(|resources| {
                             icons_service::Drawing::new(resources, &call, service::Port::default())
                         }).ok_or(NOT_SUPPORTED)??;
                     icons.push((item.icon, drawing));
                 }
                 Ok::<_, Status>(Templates { alerts,
                                             icons })
             })?;
    Ok(())
}

/// Clone the actual boot-admitted immutable family, never decode during paint.
/// Missing stock is an internal template-admission failure, not a late-load hint.
pub(crate) fn pinned_icon(id: i16, destination: Rect) -> Result<icons_service::Drawing, Status>
{
    let templates = TEMPLATES.get().ok_or(BAD_ABI)?;
    let (_, drawing) = templates.icons.iter().find(|(held, _)| *held == id).ok_or(BAD_ABI)?;
    let mut drawing = drawing.clone();
    drawing.destination = destination;
    Ok(drawing)
}

fn substitute(template: &str, value: &str) -> Result<String, Status>
{
    let mut length = 0usize;
    for (at, part) in template.split("^0").enumerate()
    {
        if at != 0
        {
            length = length.checked_add(value.len()).ok_or(BAD_ARGUMENT)?;
        }
        length = length.checked_add(part.len()).ok_or(BAD_ARGUMENT)?;
    }
    if length > INLINE_STRING_BYTES
    {
        return Err(BAD_ARGUMENT);
    }
    let mut output = String::new();
    output.try_reserve_exact(length).map_err(|_| NO_MEMORY)?;
    for (at, part) in template.split("^0").enumerate()
    {
        if at != 0
        {
            output.push_str(value);
        }
        output.push_str(part);
    }
    Ok(output)
}

pub(crate) fn instantiate(call: &DialogCall) -> Result<Alert, Status>
{
    if call.reserved != 0
    {
        return Err(BAD_ARGUMENT);
    }
    let index = IDS.iter().position(|&id| id == call.resource).ok_or(NOT_SUPPORTED)?;
    let template = &TEMPLATES.get().ok_or(NOT_SUPPORTED)?.alerts[index];
    let value = core::str::from_utf8(call.text
                                         .get(..call.text_length as usize)
                                         .ok_or(BAD_ARGUMENT)?).map_err(|_| BAD_ARGUMENT)?;
    let mut items = Vec::new();
    items.try_reserve_exact(template.items.len()).map_err(|_| NO_MEMORY)?;
    for item in &template.items
    {
        items.push(Item { bounds:  item.bounds,
                          kind:    item.kind,
                          enabled: item.enabled,
                          text:    substitute(&item.text, value)?,
                          icon:    item.icon });
    }
    Ok(Alert { bounds: template.bounds,
               position: template.position,
               items })
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum OpenError
{
    Service
    {
        stage:  &'static str,
        status: Status
    },
    Publication(PublicationError)
}
impl OpenError
{
    pub(crate) fn service(stage: &'static str, status: Status) -> Self
    {
        Self::Service { stage,
                        status }
    }

    pub(crate) fn status(self) -> Status
    {
        match self
        {
            Self::Service { status,
                            .. } => status,
            Self::Publication(PublicationError::Busy) => SERVICE_BUSY,
            Self::Publication(PublicationError::Pool(cause)) => super::modal::pool_status(cause)
        }
    }
}
impl fmt::Display for OpenError
{
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Service { stage,
                            status } =>
            {
                write!(output, "Core Alert {stage} refused with status {status}")
            }
            Self::Publication(cause) =>
            {
                write!(output, "Core Alert reserved publication refused: {cause:?}")
            }
        }
    }
}

/// The request's substitution cannot be reconstructed from substituted items.
/// Retain its bounded ABI bytes without another allocated string or quota.
struct Intent
{
    resource: i16,
    length:   usize,
    text:     [u8; INLINE_STRING_BYTES]
}

/// One actual unshared Alert Arc and DialogState, prepared before Desktop.
/// Refused canonical publication restores this same state for outside disposal.
#[must_use = "retain and dispose Core Alert preparation outside Desktop"]
pub(crate) struct PreparedAlert
{
    pub(super) state:                  Option<DialogState>,
    pub(in crate::toolbox) positioned: bool,
    recipient:                         OwnerRef,
    storage:                           OwnerRef,
    intent:                            Intent
}

fn open_text(call: &DialogCall) -> Result<&[u8], Status>
{
    if call.operation != OPEN_ALERT || call.reserved != 0 || !supported(call.scope, call.resource)
    {
        return Err(BAD_ARGUMENT);
    }
    call.text.get(..call.text_length as usize).ok_or(BAD_ARGUMENT)
}

fn validate_scope(activity: &Activity, recipient: &OwnerRef, storage: &OwnerRef)
                  -> Result<(), Status>
{
    activity.validate_core_alert_open(recipient, storage)?;
    if !crate::context::recovery_activity().is_some_and(|actual| actual.same_session(activity))
       || !crate::context::io_subject().is_some_and(|subject| {
                                           subject.scope()
                                                  .same_scope(activity.root_subject().scope())
                                       })
    {
        return Err(ACCESS_DENIED);
    }
    Ok(())
}

impl PreparedAlert
{
    /// Run under the same Opening's System Subject and protected Permit, with
    /// no Desktop/Store guard. Ordinary validation does not claim this protected scope.
    pub(crate) fn prepare(activity: &Activity, call: &mut DialogCall) -> Result<Self, OpenError>
    {
        if call.reserved != 0
        {
            return Err(OpenError::service("request", BAD_ARGUMENT));
        }
        call.item = 0;
        let text = open_text(call).map_err(|status| OpenError::service("request", status))?;
        let mut intent = Intent { resource: call.resource,
                                  length:   text.len(),
                                  text:     [0; INLINE_STRING_BYTES] };
        intent.text[..text.len()].copy_from_slice(text);
        let recipient = activity.recipient();
        let storage = activity.storage_owner();
        validate_scope(activity, &recipient, &storage).map_err(|status| {
                                                          OpenError::service("service admission",
                                                                             status)
                                                      })?;
        let alert = instantiate(call).map_err(|status| OpenError::service("template", status))?;
        call.item = alert.items.len() as u16;
        let state = DialogState::prepare(alert, Some(activity.clone()))
            .map_err(|status| OpenError::service("state backing", status))?;
        Ok(Self { state: Some(state),
                  positioned: false,
                  recipient,
                  storage,
                  intent })
    }

    pub(in crate::toolbox) fn validate(&self, recipient: &OwnerRef, storage: &OwnerRef,
                                       call: &DialogCall)
                                       -> Result<(), Status>
    {
        let text = open_text(call)?;
        if self.intent.resource != call.resource
           || self.intent.length != text.len()
           || &self.intent.text[..self.intent.length] != text
        {
            return Err(BAD_ARGUMENT);
        }
        if self.recipient.owner() != recipient.owner() || self.storage.owner() != storage.owner()
        {
            return Err(ACCESS_DENIED);
        }
        let state = self.state.as_ref().ok_or(BAD_ABI)?;
        let activity = state.recovery_activity.as_ref().ok_or(ACCESS_DENIED)?;
        validate_scope(activity, recipient, storage)?;
        let alert = state.alert.as_ref().ok_or(BAD_ABI)?;
        if state.window != 0
           || state.tracking.is_some()
           || state.controls.len() != alert.items.len()
           || state.controls.iter().any(Option::is_some)
        {
            return Err(BAD_ABI);
        }
        Ok(())
    }

    /// Position the same unique payload before birth; do not clone Alert or
    /// allocate a replacement if another real read still pins this Arc.
    pub(in crate::toolbox) fn alert_mut(&mut self) -> Result<&mut Alert, OpenError>
    {
        let alert = self.state
                        .as_mut()
                        .and_then(|state| state.alert.as_mut())
                        .ok_or(OpenError::service("prepared Alert", BAD_ABI))?;
        Arc::get_mut(alert).ok_or(OpenError::service("exclusive Alert", SERVICE_BUSY))
    }
}

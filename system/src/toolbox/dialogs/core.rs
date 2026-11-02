//! Immutable, resource-backed templates for the guaranteed System alert set.
#![forbid(unsafe_code)]
use alloc::{string::String, sync::Arc, vec::Vec};
use core::fmt;

use mac_abi::{dialogs::*, resources, *};
use mac_object_table::{shared::InvocationGate as _, store::PublicationError};
use spinning_top::Spinlock;

use super::{Alert, Item, modal::DialogState};
use crate::{recovery::{Activity, CorePayloadLease, CorePayloadQuiescence},
            toolbox::{handles::OwnerRef,
                      quickdraw::{icons_service, service}}};

const IDS: [i16; 3] = [CORE_ERROR_ALERT, CORE_OUT_OF_MEMORY_ALERT, CORE_SAVE_CHANGES_ALERT];
struct Templates
{
    alerts: [Alert; IDS.len()],
    icons:  Vec<(i16, icons_service::Drawing)>,
    stock:  Spinlock<[Option<Payload>; IDS.len()]>
}
static TEMPLATES: spin::Once<Templates> = spin::Once::new();

/// Only allocation stock: no live identity, owner, scope or Activity survives
/// semantic DialogState destruction in this value.
struct Payload
{
    alert:    Option<Arc<Alert>>,
    controls: Vec<Option<u64>>
}
/// Parked in the exact original Session until its real completion/child drain.
pub(crate) struct PendingReturn
{
    template: usize,
    payload:  Payload
}
impl PendingReturn
{
    pub(crate) fn template(&self) -> usize
    {
        self.template
    }

    pub(crate) fn unique(&mut self) -> Result<(), Status>
    {
        let alert = self.payload.alert.as_mut().ok_or(BAD_ABI)?;
        Arc::get_mut(alert).ok_or(SERVICE_BUSY)?;
        Ok(())
    }
}

pub(super) fn park_payload(lease: CorePayloadLease, alert: Option<Arc<Alert>>,
                           mut controls: Vec<Option<u64>>)
{
    controls.fill(None);
    let pending = PendingReturn { template: lease.template(),
                                  payload:  Payload { alert,
                                                      controls } };
    lease.park(pending);
}

/// The opaque proof takes only its own original Session's pending return.
/// Refusal leaves that SAME return in the closing Session for a real retry.
pub(crate) fn return_quiesced(mut proof: CorePayloadQuiescence<'_>) -> Result<(), Status>
{
    let templates = TEMPLATES.get().ok_or(BAD_ABI)?;
    crate::toolbox::handles::InvocationGate::run(|| {
        let Some(template) = proof.template()?
        else
        {
            return Ok(());
        };
        let mut stock = templates.stock.lock();
        let slot = stock.get_mut(template).ok_or(BAD_ABI)?;
        if slot.is_some()
        {
            return Err(BAD_ABI);
        }
        if let Some(pending) = proof.take()?
        {
            *slot = Some(pending.payload);
        }
        Ok(())
    })
}

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
        for item in alerts.iter().flat_map(|alert| &alert.items).filter(|item| item.kind == 32)
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
        let stock = crate::context::with_recovery_allocation(crate::recovery::permit()?, || {
            crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
                let mut stock = core::array::from_fn(|_| None);
                for (slot, template) in stock.iter_mut().zip(&alerts)
                {
                    *slot = Some(Payload::prepare(template)?);
                }
                Ok::<_, Status>(stock)
            })
        })?;
        Ok::<_, Status>(Templates { alerts,
                                    icons,
                                    stock: Spinlock::new(stock) })
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

fn substituted_length(template: &str, value: &str) -> Result<usize, Status>
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
    Ok(length)
}

fn append_substitution(output: &mut String, template: &str, value: &str)
{
    for (at, part) in template.split("^0").enumerate()
    {
        if at != 0
        {
            output.push_str(value);
        }
        output.push_str(part);
    }
}

fn substitute(template: &str, value: &str) -> Result<String, Status>
{
    let length = substituted_length(template, value)?;
    let mut output = String::new();
    output.try_reserve_exact(length).map_err(|_| NO_MEMORY)?;
    append_substitution(&mut output, template, value);
    Ok(output)
}

impl Payload
{
    fn prepare(template: &Alert) -> Result<Self, Status>
    {
        let mut items = Vec::new();
        items.try_reserve_exact(template.items.len()).map_err(|_| NO_MEMORY)?;
        for item in &template.items
        {
            if item.text.len() > INLINE_STRING_BYTES
            {
                return Err(BAD_ABI);
            }
            let mut text = String::new();
            text.try_reserve_exact(INLINE_STRING_BYTES).map_err(|_| NO_MEMORY)?;
            text.push_str(&item.text);
            items.push(Item { bounds: item.bounds,
                              kind: item.kind,
                              enabled: item.enabled,
                              text,
                              icon: item.icon });
        }
        let mut controls = Vec::new();
        controls.try_reserve_exact(items.len()).map_err(|_| NO_MEMORY)?;
        controls.resize(items.len(), None);
        let alert = Arc::try_new(Alert { bounds: template.bounds,
                                         position: template.position,
                                         items }).map_err(|_| NO_MEMORY)?;
        Ok(Self { alert: Some(alert),
                  controls })
    }

    fn reset(&mut self, template: &Alert, value: &str) -> Result<(), Status>
    {
        let alert = self.alert.as_mut().ok_or(BAD_ABI)?;
        let alert = Arc::get_mut(alert).ok_or(SERVICE_BUSY)?;
        if alert.items.len() != template.items.len()
           || self.controls.len() != template.items.len()
           || self.controls.iter().any(Option::is_some)
        {
            return Err(BAD_ABI);
        }
        // Validate every length/capacity before changing the original payload.
        for (item, source) in alert.items.iter().zip(&template.items)
        {
            let length = substituted_length(&source.text, value)?;
            if item.text.capacity() < length
            {
                return Err(BAD_ABI);
            }
        }
        alert.bounds = template.bounds;
        alert.position = template.position;
        for (item, source) in alert.items.iter_mut().zip(&template.items)
        {
            item.bounds = source.bounds;
            item.kind = source.kind;
            item.enabled = source.enabled;
            item.icon = source.icon;
            item.text.clear();
            append_substitution(&mut item.text, &source.text, value);
        }
        Ok(())
    }
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
        let value = core::str::from_utf8(text).map_err(|_| {
                                                  OpenError::service("template text", BAD_ARGUMENT)
                                              })?;
        let templates =
            TEMPLATES.get().ok_or(OpenError::service("template stock", NOT_SUPPORTED))?;
        let index = IDS.iter()
                       .position(|&id| id == call.resource)
                       .ok_or(OpenError::service("template", NOT_SUPPORTED))?;
        let template = &templates.alerts[index];
        let state = crate::toolbox::handles::InvocationGate::run(|| {
                        let mut stock = templates.stock.lock();
                        let slot = stock.get_mut(index).ok_or(BAD_ABI)?;
                        slot.as_mut().ok_or(SERVICE_BUSY)?.reset(template, value)?;
                        let payload = slot.take().ok_or(BAD_ABI)?;
                        let lease = match activity.lease_core_payload(index)
                        {
                            Ok(lease) => lease,
                            Err(status) =>
                            {
                                *slot = Some(payload);
                                return Err(status);
                            }
                        };
                        Ok(DialogState { previous:          None,
                                         window:            0,
                                         alert:             payload.alert,
                                         controls:          payload.controls,
                                         tracking:          None,
                                         recovery_activity: Some(activity.clone()),
                                         core_payload:      Some(lease) })
                    }).map_err(|status| OpenError::service("payload stock", status))?;
        call.item = template.items.len() as u16;
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
        if !state.core_payload.as_ref().is_some_and(|lease| {
                                           lease.matches_activity(activity)
                                           && IDS.get(lease.template()).copied()
                                              == Some(self.intent.resource)
                                       })
        {
            return Err(ACCESS_DENIED);
        }
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

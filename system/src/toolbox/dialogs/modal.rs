//! Registered classic Alert protocol; the window and controls retain their own lifetimes.
#![forbid(unsafe_code)]
use alloc::{sync::Arc, vec::Vec};
use core::ops::Deref;

use mac_abi::*;
use mac_object_table::{domain::{CompletionError, ReleasedResource},
                       registry::{Release, Source, Specification},
                       shared::InvocationGate as _,
                       store::CompletionKey};
use spinning_top::Spinlock;

use super::{Alert,
            core_alerts::{OpenError, PreparedAlert}};
use crate::toolbox::handles::{CapacityPolicy, InvocationGate, KindCode, KindLease, OwnerRef,
                              Store, cleanup::Failure};

pub(crate) struct DialogState
{
    pub previous:                 Option<WindowId>,
    pub window:                   u64,
    pub alert:                    Option<Arc<Alert>>,
    pub controls:                 Vec<Option<u64>>,
    pub tracking:                 Option<usize>,
    pub system_menu:              bool,
    pub closing:                  bool,
    pub(super) close_observer:    Spinlock<Option<crate::toolbox::runtime::DialogCloseObserver>>,
    pub(super) recovery_activity: Option<crate::recovery::Activity>,
    pub(super) core_payload:      Option<crate::recovery::CorePayloadLease>
}
static EMPTY: DialogState = DialogState { previous:          None,
                                          window:            0,
                                          alert:             None,
                                          controls:          Vec::new(),
                                          tracking:          None,
                                          system_menu:       false,
                                          closing:           false,
                                          close_observer:    Spinlock::new(None),
                                          recovery_activity: None,
                                          core_payload:      None };
impl Drop for DialogState
{
    fn drop(&mut self)
    {
        if let Some(lease) = self.core_payload.take()
        {
            // Semantic IDs die here; independently registered children retain
            // their original cleanup and ACK pins. Only inert buffers park.
            self.previous = None;
            self.window = 0;
            self.tracking = None;
            super::core_alerts::park_payload(lease,
                                             self.alert.take(),
                                             core::mem::take(&mut self.controls));
        }
    }
}
impl DialogState
{
    /// The original ordinary Alert payload factory. Protected Core Alerts use
    /// their actual boot-stocked payload instead.
    pub(super) fn prepare(alert: Alert, activity: Option<crate::recovery::Activity>)
                          -> Result<Self, Status>
    {
        let mut controls = Vec::new();
        controls.try_reserve_exact(alert.items.len()).map_err(|_| NO_MEMORY)?;
        controls.resize(alert.items.len(), None);
        let alert = Arc::try_new(alert).map_err(|_| NO_MEMORY)?;
        Ok(Self { previous: None,
                  window: 0,
                  alert: Some(alert),
                  controls,
                  tracking: None,
                  system_menu: false,
                  closing: false,
                  close_observer: Spinlock::new(None),
                  recovery_activity: activity,
                  core_payload: None })
    }

    pub fn active(&self) -> bool
    {
        self.window != 0 && self.alert.is_some()
    }
}
pub(crate) type Released = mac_object_table::store::Removed<DialogState, InvocationGate>;
pub(crate) struct Manager
{
    owner:       OwnerRef,
    records:     Store<DialogState>,
    current:     Option<CompletionKey>,
    publication: Option<mac_object_table::store::PublicationReservation<InvocationGate>>
}
impl Deref for Manager
{
    type Target = DialogState;

    fn deref(&self) -> &DialogState
    {
        self.current.and_then(|key| self.records.get(key.id())).unwrap_or(&EMPTY)
    }
}
impl Manager
{
    pub(crate) fn new(kinds: (KindLease, KindLease), owner: OwnerRef) -> Result<Self, Status>
    {
        Ok(Self { owner,
                  records: Store::owned(kinds.0, kinds.1).map_err(|_| BAD_ARGUMENT)?,
                  current: None,
                  publication: None })
    }

    pub(crate) fn owner(&self) -> &OwnerRef
    {
        &self.owner
    }

    /// Boot-admit the core protocol's canonical slot, ID-index capacity and
    /// reusable completion endpoint in this exact permanent System store.
    pub(crate) fn prepare_recovery(&mut self) -> Result<(), Status>
    {
        if self.owner.owner().kind() != mac_object_table::domain::DomainKind::System
        {
            return Err(ACCESS_DENIED);
        }
        if self.publication.is_none()
        {
            self.publication =
                Some(self.records.reserve_publication(&self.owner).map_err(pool_status)?);
        }
        Ok(())
    }

    pub(crate) fn begin(&mut self, alert: Alert, previous: Option<WindowId>)
                        -> Result<u64, Status>
    {
        if !self.owner.accepting()
        {
            return Err(SERVICE_BUSY);
        }
        if self.current.is_some_and(|key| self.records.completing(&key).is_some())
        {
            return Err(SERVICE_BUSY);
        }
        // System core Alerts have one detached publication path below. A
        // protected caller never falls back to ordinary payload construction.
        if self.owner.owner().kind() == mac_object_table::domain::DomainKind::System
           || crate::context::recovery_activity().is_some()
        {
            return Err(ACCESS_DENIED);
        }
        let mut state = DialogState::prepare(alert, None)?;
        state.previous = previous;
        let (id, completion) = self.records
                                   .insert_with_receipt(&self.owner, |_| state)
                                   .map_err(pool_status)?
                                   .into_parts();
        self.current = Some(completion);
        Ok(id)
    }

    /// No allocation or definition callback occurs in this final native turn.
    /// Every prebirth refusal retains/restores the same state in its outer owner.
    pub(in crate::toolbox) fn begin_core_prepared(&mut self, recipient: &OwnerRef,
                                                  storage: &OwnerRef,
                                                  call: &mut mac_abi::dialogs::DialogCall,
                                                  previous: Option<WindowId>,
                                                  prepared: &mut PreparedAlert)
                                                  -> Result<u64, OpenError>
    {
        InvocationGate::run(|| {
            prepared.validate(recipient, storage, call)
                    .map_err(|status| OpenError::service("final admission", status))?;
            if !prepared.positioned
            {
                return Err(OpenError::service("positioning state", BAD_ABI));
            }
            if self.owner.owner() != storage.owner()
            {
                return Err(OpenError::service("storage owner", ACCESS_DENIED));
            }
            if !self.owner.accepting()
               || self.current.is_some_and(|key| self.records.completing(&key).is_some())
            {
                return Err(OpenError::service("protocol vacancy", SERVICE_BUSY));
            }
            let reservation = self.publication
                                  .as_mut()
                                  .ok_or(OpenError::service("reserved endpoint", NOT_SUPPORTED))?;
            if !reservation.ready()
            {
                return Err(OpenError::service("reserved completion", SERVICE_BUSY));
            }
            let mut state =
                prepared.state.take().ok_or(OpenError::service("prepared state", BAD_ABI))?;
            let Some(activity) = state.recovery_activity.as_ref()
            else
            {
                prepared.state = Some(state);
                return Err(OpenError::service("prepared activity", ACCESS_DENIED));
            };
            let subject = activity.root_subject();
            let pin = activity.acknowledgement_pin();
            state.previous = previous;
            match self.records.insert_reserved_in_scope_owned_with_acknowledgement(reservation,
                                                                                   subject.scope(),
                                                                                   Some(pin),
                                                                                   state)
            {
                Ok(publication) =>
                {
                    let (id, completion) = publication.into_parts();
                    self.current = Some(completion);
                    Ok(id)
                }
                Err((cause, state)) =>
                {
                    prepared.state = Some(state);
                    Err(OpenError::Publication(cause))
                }
            }
        })
    }

    pub(crate) fn state_mut(&mut self) -> Result<&mut DialogState, Status>
    {
        self.records.get_mut(self.current.ok_or(NOT_FOUND)?.id()).ok_or(NOT_FOUND)
    }

    pub(crate) fn take(&mut self) -> Option<Released>
    {
        let record = self.records.remove(self.current?.id())?;
        self.current = None;
        Some(record)
    }

    pub(crate) fn recovery_window(&self, activity: &crate::recovery::Activity)
                                  -> Result<Option<u64>, Status>
    {
        if self.owner.owner() != activity.storage_owner().owner()
        {
            return Err(ACCESS_DENIED);
        }
        let Some(state) = self.current.and_then(|key| self.records.get(key.id()))
        else
        {
            return Ok(None);
        };
        if !state.recovery_activity.as_ref().is_some_and(|held| held.same_session(activity))
        {
            return Err(ACCESS_DENIED);
        }
        Ok(Some(state.window))
    }

    /// Retain the original close endpoint in the actual ALRT payload, including
    /// after its public ID has been invalidated. No new owner or ticket is made.
    pub(crate) fn observe_window_close(&mut self,
                                       observer: crate::toolbox::runtime::DialogCloseObserver)
    {
        let Some(key) = self.current
        else
        {
            return;
        };
        if key.owner() != observer.storage_owner()
        {
            return;
        }
        let Some(state) = self.records.completing(&key)
        else
        {
            return;
        };
        if state.window != observer.window_id().0 || state.alert.is_none()
        {
            return;
        }
        let recipient =
            state.recovery_activity.as_ref().map_or(self.owner.owner().incarnation(), |activity| {
                                                activity.recipient().owner().incarnation()
                                            });
        if recipient != observer.owner()
        {
            return;
        }
        InvocationGate::run(|| {
            let mut held = state.close_observer.lock();
            if held.is_none()
            {
                *held = Some(observer);
            }
        });
        if let Some(state) = self.records.get_mut(key.id())
        {
            state.closing = true;
        }
    }

    pub(crate) fn window_close_observer(&self, window: u64)
                                        -> Option<crate::toolbox::runtime::DialogCloseObserver>
    {
        let state = self.records.completing(&self.current?)?;
        if state.window != window
        {
            return None;
        }
        InvocationGate::run(|| state.close_observer.lock().clone())
    }

    pub(crate) fn take_window(&mut self, window: u64) -> Option<Released>
    {
        if self.window == window && self.active() { self.take() } else { None }
    }

    /// This is the original protocol's retained Activity, not a session chosen
    /// from a numeric recipient. Call only after its actual Window UI tail is
    /// admitted; child and payload acknowledgements still govern kit reuse.
    pub(crate) fn close_window_activity(&self, window: u64) -> Result<(), Status>
    {
        if self.window != window || self.alert.is_none()
        {
            return Err(NOT_FOUND);
        }
        if let Some(activity) = &self.recovery_activity
        {
            activity.close();
        }
        Ok(())
    }

    pub(crate) fn release(&mut self, ticket: ReleasedResource)
                          -> Result<Released, CompletionError>
    {
        let key = ticket.key();
        let record = self.records.release(ticket)?;
        if self.current.is_some_and(|current| current.resource_key() == key)
        {
            self.current = None;
        }
        Ok(record)
    }

    pub(crate) fn release_storage(&mut self, ticket: ReleasedResource)
                                  -> Result<(), CompletionError>
    {
        self.records.release_storage(ticket)
    }
}
pub(crate) fn specification(storage: bool) -> Specification<'static>
{
    Specification { kind:     KindCode(if storage { *b"ALST" } else { *b"ALRT" }),
                    label:    if storage { "Alert backing" } else { "Resource alerts" },
                    provider: "System Dialog Manager",
                    source:   Source::Resident,
                    capacity: CapacityPolicy::HardBound { slots:  1,
                                                          reason: if storage
                                                          {
                                                              "One Alert Manager backing per domain"
                                                          }
                                                          else
                                                          {
                                                              "One active classic Alert protocol \
                                                               per owner; System uses one core \
                                                               store"
                                                          } } }
}
pub(crate) struct ReleaseAlerts;
pub(crate) struct ReleaseStorage;
impl Release<crate::toolbox::runtime::Toolbox, Failure> for ReleaseAlerts
{
    fn release(&self, context: &mut crate::toolbox::runtime::Toolbox, ticket: ReleasedResource)
               -> Result<(), Failure>
    {
        context.release_alert(ticket)
    }
}
impl Release<crate::toolbox::runtime::Toolbox, Failure> for ReleaseStorage
{
    fn release(&self, context: &mut crate::toolbox::runtime::Toolbox, ticket: ReleasedResource)
               -> Result<(), Failure>
    {
        context.release_alert_storage(ticket).map_err(Into::into)
    }
}

pub(super) fn pool_status(error: mac_object_table::domain::PoolError) -> Status
{
    crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(error))
}

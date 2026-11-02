//! Registered classic Alert protocol; the window and controls retain their own lifetimes.
#![forbid(unsafe_code)]
use alloc::{sync::Arc, vec::Vec};
use core::ops::Deref;

use mac_abi::*;
use mac_object_table::{domain::{CompletionError, ReleasedResource},
                       registry::{Release, Source, Specification},
                       shared::InvocationGate as _};

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
    pub(super) recovery_activity: Option<crate::recovery::Activity>
}
static EMPTY: DialogState = DialogState { previous:          None,
                                          window:            0,
                                          alert:             None,
                                          controls:          Vec::new(),
                                          tracking:          None,
                                          recovery_activity: None };
impl DialogState
{
    /// The original payload factory; a caller chooses its genuine scope before
    /// entering. Core Alert calls it outside Desktop, ordinary Alerts retain
    /// their existing allocation path.
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
                  recovery_activity: activity })
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
    current:     Option<u64>,
    publication: Option<mac_object_table::store::PublicationReservation<InvocationGate>>
}
impl Deref for Manager
{
    type Target = DialogState;

    fn deref(&self) -> &DialogState
    {
        self.current.and_then(|id| self.records.get(id)).unwrap_or(&EMPTY)
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
        if self.current.is_some_and(|id| self.records.get(id).is_some())
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
        let id = self.records.insert(&self.owner, state).map_err(pool_status)?;
        self.current = Some(id);
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
               || self.current.is_some_and(|id| self.records.get(id).is_some())
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
                    let id = publication.id();
                    self.current = Some(id);
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
        self.records.get_mut(self.current.ok_or(NOT_FOUND)?).ok_or(NOT_FOUND)
    }

    pub(crate) fn take(&mut self) -> Option<Released>
    {
        let record = self.records.remove(self.current?)?;
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
        let Some(state) = self.current.and_then(|id| self.records.get(id))
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

    pub(crate) fn take_recovery(&mut self, activity: &crate::recovery::Activity)
                                -> Result<Option<Released>, Status>
    {
        if self.recovery_window(activity)?.is_none()
        {
            return Ok(None);
        }
        self.take().map(Some).ok_or(NOT_FOUND)
    }

    pub(crate) fn take_window(&mut self, window: u64) -> Option<Released>
    {
        if self.window == window && self.active() { self.take() } else { None }
    }

    pub(crate) fn release(&mut self, ticket: ReleasedResource)
                          -> Result<Released, CompletionError>
    {
        let record = self.records.release(ticket)?;
        self.current = None;
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
impl Release<crate::toolbox::runtime::Desktop, Failure> for ReleaseAlerts
{
    fn release(&self, context: &mut crate::toolbox::runtime::Desktop, ticket: ReleasedResource)
               -> Result<(), Failure>
    {
        context.release_alert(ticket)
    }
}
impl Release<crate::toolbox::runtime::Desktop, Failure> for ReleaseStorage
{
    fn release(&self, context: &mut crate::toolbox::runtime::Desktop, ticket: ReleasedResource)
               -> Result<(), Failure>
    {
        context.release_alert_storage(ticket).map_err(Into::into)
    }
}

pub(super) fn pool_status(error: mac_object_table::domain::PoolError) -> Status
{
    crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(error))
}

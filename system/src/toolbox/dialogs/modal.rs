//! Registered classic Alert protocol; the window and controls retain their own lifetimes.
#![forbid(unsafe_code)]
use alloc::{sync::Arc, vec::Vec};
use core::ops::Deref;

use mac_abi::*;
use mac_object_table::{domain::{CompletionError, ReleasedResource},
                       registry::{Release, Source, Specification}};

use super::Alert;
use crate::toolbox::handles::{CapacityPolicy, InvocationGate, KindCode, KindLease, OwnerRef,
                              Store, cleanup::Failure};

pub(crate) struct DialogState
{
    pub previous:      Option<WindowId>,
    pub window:        u64,
    pub alert:         Option<Arc<Alert>>,
    pub controls:      Vec<Option<u64>>,
    pub tracking:      Option<usize>,
    recovery_activity: Option<crate::recovery::Activity>
}
static EMPTY: DialogState = DialogState { previous:          None,
                                          window:            0,
                                          alert:             None,
                                          controls:          Vec::new(),
                                          tracking:          None,
                                          recovery_activity: None };
impl DialogState
{
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
        let activity = crate::context::recovery_activity();
        let core_subject = if self.owner.owner().kind()
                              == mac_object_table::domain::DomainKind::System
        {
            let activity = activity.as_ref().ok_or(ACCESS_DENIED)?;
            if activity.storage_owner().owner() != self.owner.owner()
            {
                return Err(ACCESS_DENIED);
            }
            let subject = crate::context::io_subject().ok_or(ACCESS_DENIED)?;
            let root = activity.root_subject();
            if !subject.scope().same_scope(root.scope())
            {
                return Err(ACCESS_DENIED);
            }
            if !self.publication.as_ref().ok_or(NOT_SUPPORTED)?.ready()
            {
                return Err(SERVICE_BUSY);
            }
            Some(subject)
        }
        else
        {
            // A protected core alert must use the one System-owned manager.
            if activity.is_some()
            {
                return Err(ACCESS_DENIED);
            }
            None
        };
        let mut controls = Vec::new();
        controls.try_reserve_exact(alert.items.len()).map_err(|_| NO_MEMORY)?;
        controls.resize(alert.items.len(), None);
        let alert = Arc::try_new(alert).map_err(|_| NO_MEMORY)?;
        let state = DialogState { previous,
                                  window: 0,
                                  alert: Some(alert),
                                  controls,
                                  tracking: None,
                                  recovery_activity: activity.clone() };
        let id = if let Some(subject) = core_subject
        {
            let reservation = self.publication.as_mut().ok_or(NOT_SUPPORTED)?;
            let pin = activity.as_ref().ok_or(ACCESS_DENIED)?.acknowledgement_pin();
            self.records
                .insert_reserved_in_scope_with_acknowledgement(reservation,
                                                               subject.scope(),
                                                               Some(pin),
                                                               |_| state)
                .map_err(|error| {
                    match error
                    {
                        mac_object_table::store::PublicationError::Busy => SERVICE_BUSY,
                        mac_object_table::store::PublicationError::Pool(error) => pool_status(error)
                    }
                })?
                .id()
        }
        else
        {
            self.records.insert(&self.owner, state).map_err(pool_status)?
        };
        self.current = Some(id);
        Ok(id)
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

fn pool_status(error: mac_object_table::domain::PoolError) -> Status
{
    crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(error))
}

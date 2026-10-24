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
    pub previous: Option<WindowId>,
    pub window:   u64,
    pub alert:    Option<Arc<Alert>>,
    pub controls: Vec<Option<u64>>,
    pub tracking: Option<usize>
}
static EMPTY: DialogState = DialogState { previous: None,
                                          window:   0,
                                          alert:    None,
                                          controls: Vec::new(),
                                          tracking: None };
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
    owner:   OwnerRef,
    records: Store<DialogState>,
    current: Option<u64>
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
                  current: None })
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
        let mut controls = Vec::new();
        controls.try_reserve_exact(alert.items.len()).map_err(|_| NO_MEMORY)?;
        controls.resize(alert.items.len(), None);
        let alert = Arc::try_new(alert).map_err(|_| NO_MEMORY)?;
        let id = self.records
                     .insert(&self.owner,
                             DialogState { previous,
                                           window: 0,
                                           alert: Some(alert),
                                           controls,
                                           tracking: None })
                     .map_err(|error| {
                         match error
                    {
                        mac_object_table::domain::PoolError::Stopping { .. }
                        | mac_object_table::domain::PoolError::RegistrationUnavailable { .. } =>
                        {
                            SERVICE_BUSY
                        }
                        _ => NO_MEMORY
                    }
                     })?;
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
                                                               per application"
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

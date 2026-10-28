//! Admitted publication endpoints for one protected resident modal service.
//! These are unused canonical slots, never dummy jobs or reusable public IDs.
//! Counts describe the queued FSYS -> nested HDIR graph. Runtime selects
//! ordinary admission for graphs not covered by this protected kit. A covered
//! graph exceeding its admitted concurrency gets typed Busy before the next
//! callback, rather than stealing ordinary publication capacity.
#![forbid(unsafe_code)]
use mac_object_table::{domain::DomainKind,
                       shared::InvocationGate as _,
                       store::{PublicationError, PublicationReservation}};
use spinning_top::Spinlock;

use super::*;
use crate::toolbox::handles::{InvocationGate, OwnerRef};

type Reservation = PublicationReservation<InvocationGate>;
pub(crate) struct RecoveryEndpoints
{
    owner:    mac_object_table::domain::DomainId,
    job:      Spinlock<Reservation>,
    requests: Spinlock<[Reservation; 2]>
}
impl RecoveryEndpoints
{
    /// Boot admission runs with the genuine System original account, before
    /// ordinary pressure. No endpoint contains a Session/Activity backreference.
    pub(crate) fn prepare(owner: &OwnerRef) -> Result<Self, Status>
    {
        if owner.owner().kind() != DomainKind::System
        {
            return Err(ACCESS_DENIED);
        }
        let job = crate::drivers::prepare_recovery_job(owner)?;
        let requests = crate::drivers::prepare_recovery_requests(owner)?;
        Ok(Self { owner:    owner.owner(),
                  job:      Spinlock::new(job),
                  requests: Spinlock::new(requests) })
    }

    pub(crate) fn idle(&self) -> bool
    {
        InvocationGate::run(|| {
            self.job.lock().ready() && self.requests.lock().iter().all(Reservation::ready)
        })
    }

    pub(super) fn with_job<R>(&self, subject: &subjects::Subject,
                              operation: impl FnOnce(&mut Reservation) -> Result<R, Status>)
                              -> Result<R, Status>
    {
        if subject.scope().subject() != self.owner
        {
            return Err(ACCESS_DENIED);
        }
        InvocationGate::run(|| operation(&mut self.job.lock()))
    }

    pub(crate) fn with_request<R>(&self, subject: &subjects::Subject,
                                  operation: impl FnOnce(&mut Reservation)
                                         -> Result<R, crate::drivers::Error>)
                                  -> Result<R, crate::drivers::Error>
    {
        if subject.scope().subject() != self.owner
        {
            return Err(crate::drivers::Error::RequestSubject);
        }
        InvocationGate::run(|| {
            let mut requests = self.requests.lock();
            let reservation = requests.iter_mut()
                                      .find(|entry| entry.ready())
                                      .ok_or(crate::drivers::Error::Busy)?;
            operation(reservation)
        })
    }
}
pub(super) fn publication_status(error: PublicationError) -> Status
{
    match error
    {
        PublicationError::Busy => SERVICE_BUSY,
        PublicationError::Pool(error) => super::records::pool_status(error)
    }
}

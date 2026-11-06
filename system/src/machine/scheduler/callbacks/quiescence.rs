//! A closed genuine source cannot regain author execution after its last real
//! canonical thread has disappeared. This is not a Dispose callback receipt.
#![forbid(unsafe_code)]
use mac_abi::{ACCESS_DENIED, SERVICE_BUSY, Status};
use mac_object_table::domain::DomainKind;

use super::super::SCHEDULER;

pub(crate) struct SourceQuiescence<'a>
{
    source: &'a crate::images::Read
}
impl SourceQuiescence<'_>
{
    pub(crate) fn owner(&self) -> &crate::toolbox::handles::OwnerRef
    {
        self.source.owner()
    }

    pub(crate) fn image(&self) -> crate::images::Binding
    {
        self.source.binding()
    }
}

pub(crate) fn source_quiescence(source: &crate::images::Read)
                                -> Result<SourceQuiescence<'_>, Status>
{
    let owner = source.owner();
    if owner.owner().kind() != DomainKind::Process
       || source.binding().owner() != owner.owner().incarnation()
    {
        return Err(ACCESS_DENIED);
    }
    if owner.accepting()
    {
        return Err(SERVICE_BUSY);
    }
    let _interrupts = crate::machine::interrupts::mask();
    let scheduler = SCHEDULER.lock();
    // Include inert, retired, claimed and service-draining records. A semantic
    // stop or an empty callback queue cannot stand in for actual THRD teardown.
    // Owner closing is monotonic; admission cannot publish a new author after
    // this check. Preparations not yet published cannot run and must refuse.
    if scheduler.iter().any(|(_, thread)| {
                           thread.core.context.identity().app.0 == owner.owner().incarnation()
                           || thread.core.binding.owner() == owner.owner().incarnation()
                       })
    {
        return Err(SERVICE_BUSY);
    }
    Ok(SourceQuiescence { source })
}

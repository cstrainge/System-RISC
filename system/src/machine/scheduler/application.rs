//! Runtime failure admission and genuine safe-point observation. Child lifecycle
//! uses this module later; this slice exports no unsupported thread operations.
#![forbid(unsafe_code)]
use mac_abi::Status;
use mac_object_table::domain::DomainKind;

use super::*;
mod failure;
pub(crate) use failure::{fail_owner, failure_quiescent};

#[derive(Clone, Copy, Debug)]
pub(crate) enum RuntimeAdmissionError
{
    Closed
}
impl RuntimeAdmissionError
{
    pub(crate) fn status(&self) -> Status
    {
        mac_abi::OPERATION_CANCELLED
    }
}
impl core::fmt::Display for RuntimeAdmissionError
{
    fn fmt(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        out.write_str("Process runtime admission is closed; prepared key/author ownership was not \
                       published")
    }
}
/// No allocation, callback or owning disposal may run while this turn lives.
/// Declaration order releases SCHEDULER before restoring local interrupts.
pub(crate) struct RuntimeTurn
{
    _scheduler:  spinning_top::SpinlockGuard<'static, Scheduler>,
    _interrupts: super::super::interrupts::InterruptMask
}
pub(crate) fn runtime_turn(admission: &crate::toolbox::native_threads::RuntimeAdmission)
                           -> Result<RuntimeTurn, RuntimeAdmissionError>
{
    let interrupts = super::super::interrupts::mask();
    let scheduler = SCHEDULER.lock();
    if !admission.accepting()
    {
        return Err(RuntimeAdmissionError::Closed);
    }
    Ok(RuntimeTurn { _scheduler:  scheduler,
                     _interrupts: interrupts })
}

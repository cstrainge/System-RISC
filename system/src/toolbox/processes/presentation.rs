//! Running-process presentation policy uses the authentic lifetime ledger.
#![forbid(unsafe_code)]
use mac_abi::{processes::ProcessInfo, *};

use crate::toolbox::handles::OwnerRef;

pub(super) fn running(ledger: &OwnerRef, info: &ProcessInfo) -> bool
{
    ledger.accepting() && info.thread != 0
}

pub(super) fn require_foreground(ledger: &OwnerRef, info: &ProcessInfo) -> Result<(), Status>
{
    if !running(ledger, info)
    {
        return Err(SERVICE_BUSY);
    }
    if info.hidden != 0
    {
        return Err(ACCESS_DENIED);
    }
    Ok(())
}

pub(super) fn hide_other(ledger: &OwnerRef, info: &mut ProcessInfo, foreground: u64) -> bool
{
    if ledger.owner().incarnation() != foreground && running(ledger, info) && info.hidden == 0
    {
        info.hidden = 1;
        true
    }
    else
    {
        false
    }
}

#[cfg(all(test, feature = "test-support", not(target_arch = "riscv64")))]
#[path = "presentation/tests.rs"]
mod tests;

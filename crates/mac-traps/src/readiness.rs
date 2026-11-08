//! Native source readiness, separate from the classic EventRecord queue.
use mac_abi::readiness as wire;

use crate::{Client, Result, status};
impl Client<'_>
{
    pub fn readiness(self, call: &mut wire::Call) -> Result<()>
    {
        status(self.invoke(mac_abi::traps::READINESS, call))
    }
}

impl Client<'_>
{
    pub fn timers(self, call: &mut mac_abi::timers::Call) -> Result<()>
    {
        status(self.invoke(mac_abi::traps::TIMERS, call))
    }
}

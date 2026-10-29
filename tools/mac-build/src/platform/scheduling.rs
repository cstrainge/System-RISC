//! Platform-selected execution policy limits. A callback's elapsed-time
//! allowance is independent of the runnable thread's scheduling quantum.
use serde::Deserialize;

use crate::{Result, error};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Definition
{
    quantum_microseconds:         u64,
    callback_budget_microseconds: u64
}
pub struct Checked
{
    quantum:         u32,
    callback_budget: u64
}
impl Definition
{
    pub(super) fn check(&self) -> Result<Checked>
    {
        let quantum = u32::try_from(self.quantum_microseconds).ok()
                                                              .filter(|v| *v != 0)
                                                              .ok_or_else(|| {
                                                                  error("platform scheduling.\
                                                                         quantum_microseconds \
                                                                         must be nonzero and fit \
                                                                         the SCHD u32 quantum \
                                                                         field")
                                                              })?;
        if self.callback_budget_microseconds == 0
        {
            return Err(error("platform scheduling.callback_budget_microseconds must be \
                              nonzero; a callback budget is a late-return check, not \
                              non-return recovery"));
        }
        Ok(Checked { quantum,
                     callback_budget: self.callback_budget_microseconds })
    }
}
impl Checked
{
    pub fn rust_constants(&self) -> String
    {
        format!("// Generated from the checked platform scheduling policy.\nconst \
                 QUANTUM_MICROSECONDS: u64 = {};\nconst CALLBACK_BUDGET_MICROSECONDS: u64 = {};\n",
                self.quantum, self.callback_budget)
    }
}

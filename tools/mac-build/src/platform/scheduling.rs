//! Platform-selected execution policy limits. A callback's elapsed-time
//! allowance is independent of the runnable thread's scheduling quantum.
use serde::Deserialize;

use crate::{Result, error};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Definition
{
    maximum_active_harts:         u32,
    quantum_microseconds:         u64,
    callback_budget_microseconds: u64
}
pub struct Checked
{
    maximum_active_harts: u32,
    quantum:              u32,
    callback_budget:      u64
}
impl Definition
{
    pub(super) fn check(&self, qemu_harts: core::num::NonZeroUsize) -> Result<Checked>
    {
        if self.maximum_active_harts == 0
           || usize::try_from(self.maximum_active_harts).ok()
                                                      .is_none_or(|count| count > qemu_harts.get())
        {
            return Err(error("platform scheduling.maximum_active_harts must be nonzero and no \
                              greater than qemu_harts"));
        }
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
        Ok(Checked { maximum_active_harts: self.maximum_active_harts,
                     quantum,
                     callback_budget: self.callback_budget_microseconds })
    }
}
impl Checked
{
    pub fn rust_constants(&self) -> String
    {
        format!("// Generated from the checked platform scheduling policy.\npub(crate) const \
                 MAXIMUM_ACTIVE_HARTS: u32 = {};\nconst QUANTUM_MICROSECONDS: u64 = {};\nconst \
                 CALLBACK_BUDGET_MICROSECONDS: u64 = {};\n",
                self.maximum_active_harts, self.quantum, self.callback_budget)
    }
}

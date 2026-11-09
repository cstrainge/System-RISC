//! Build-selected stack policies. The firmware bootstrap stack is separate:
//! these bounds describe resident per-hart and application per-thread stacks.
use mac_kernel_model::{pages::PageGeometry, policy::ConfiguredRange, stack::Policy};
use serde::Deserialize;

use super::{address, error};
use crate::Result;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Range
{
    minimum: String,
    default: String,
    maximum: String
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Definition
{
    system:      Range,
    application: Range
}
#[derive(Debug)]
pub(super) struct Checked
{
    system:      ConfiguredRange,
    application: ConfiguredRange
}
impl Definition
{
    pub(super) fn check(&self, page: u64) -> Result<Checked>
    {
        let check = |name: &str, range: &Range| -> Result<ConfiguredRange> {
            let minimum = address(&format!("stacks.{name}.minimum"), &range.minimum)?;
            let default = address(&format!("stacks.{name}.default"), &range.default)?;
            let maximum = address(&format!("stacks.{name}.maximum"), &range.maximum)?;
            let geometry = PageGeometry::new(page).map_err(|cause| {
                                                      error(format!("platform memory \
                                                                     stacks.{name}: {cause}"))
                                                  })?;
            Policy::new(minimum, default, maximum, geometry).map_err(|cause| {
                                                                error(format!("platform memory \
                                                                               stacks.{name}: \
                                                                               {cause}"))
                                                            })?;
            let checked = ConfiguredRange::new(minimum, default, maximum).map_err(|cause| {
                              error(format!("platform memory stacks.{name}: {cause}"))
                          })?;
            Ok(checked)
        };
        Ok(Checked { system:      check("system", &self.system)?,
                     application: check("application", &self.application)? })
    }
}
impl Checked
{
    pub(super) fn system_default(&self) -> u64
    {
        self.system.default_value()
    }

    pub(super) fn system_maximum(&self) -> u64
    {
        self.system.maximum()
    }

    pub(super) fn application_default(&self) -> u64
    {
        self.application.default_value()
    }

    pub(super) fn constants(&self) -> [(&'static str, u64); 6]
    {
        [("SYSTEM_STACK_MIN_BYTES", self.system.minimum()),
         ("SYSTEM_STACK_BYTES", self.system.default_value()),
         ("SYSTEM_STACK_MAX_BYTES", self.system.maximum()),
         ("APPLICATION_STACK_MIN_BYTES", self.application.minimum()),
         ("APPLICATION_STACK_BYTES", self.application.default_value()),
         ("APPLICATION_STACK_MAX_BYTES", self.application.maximum())]
    }
}

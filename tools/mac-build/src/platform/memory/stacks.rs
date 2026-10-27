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
#[cfg(test)]
mod tests
{
    use super::*;
    fn definition() -> Definition
    {
        serde_json::from_str(r#"{"system":{"minimum":"0x40000","default":"0x400000","maximum":"0x800000"},"application":{"minimum":"0x10000","default":"0x400000","maximum":"0x800000"}}"#).unwrap()
    }
    #[test]
    fn configurable_defaults_refuse_invalid_ranges_and_alignment_before_emission()
    {
        let definition = definition();
        let values = definition.check(4096).unwrap().constants();
        assert_eq!(values[1], ("SYSTEM_STACK_BYTES", 4 << 20));
        assert_eq!(values[4], ("APPLICATION_STACK_BYTES", 4 << 20));
        for field in ["minimum", "default", "maximum"]
        {
            let mut bad = definition.clone();
            match field
            {
                "minimum" => bad.application.minimum = "0x10001".into(),
                "default" => bad.application.default = "0x400001".into(),
                _ => bad.application.maximum = "0x800001".into()
            }
            assert!(bad.check(4096)
                       .unwrap_err()
                       .to_string()
                       .contains("stacks.application: stack sizes must be nonzero and page \
                                  aligned"));
        }
        let mut bad = definition.clone();
        bad.system.minimum = "0x0".into();
        assert!(bad.check(4096)
                   .unwrap_err()
                   .to_string()
                   .contains("stacks.system: stack sizes must be nonzero"));
        let mut bad = definition.clone();
        bad.system.default = "0x1000000".into();
        assert!(bad.check(4096)
                   .unwrap_err()
                   .to_string()
                   .contains("with minimum <= default <= maximum"));
        let mut changed = definition;
        changed.system.default = "0x200000".into();
        changed.application.default = "0x300000".into();
        assert_eq!(changed.check(4096).unwrap().constants()[1].1, 2 << 20);
        assert_eq!(changed.check(4096).unwrap().constants()[4].1, 3 << 20);
    }
}

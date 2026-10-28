//! Build-selected process caps. These limits admit charges; they neither reserve
//! physical memory at boot nor guarantee that the selected capacity is free.
use mac_kernel_model::pages::PageGeometry;
use serde::Deserialize;

use super::{address, error};
use crate::Result;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Definition
{
    kernel_bytes:      String,
    application_bytes: String
}
#[derive(Debug)]
pub(super) struct Checked
{
    kernel:      u64,
    application: u64
}
impl Definition
{
    pub(super) fn check(&self, page: u64, application_stack: u64) -> Result<Checked>
    {
        let check = |name: &str, text: &str| -> Result<u64> {
            let value = address(&format!("process_accounts.{name}"), text)?;
            if value == 0 || !value.is_multiple_of(page)
            {
                return Err(error(format!("platform memory process_accounts.{name} must be \
                                          nonzero and page aligned ({page} bytes)")));
            }
            Ok(value)
        };
        let kernel = check("kernel_bytes", &self.kernel_bytes)?;
        let application = check("application_bytes", &self.application_bytes)?;
        kernel.checked_add(application)
              .ok_or_else(|| {
                  error("platform memory process_accounts combined caps overflow u64")
              })?;
        let stack_backing =
            PageGeometry::new(page).and_then(|geometry| geometry.buddy_bytes(application_stack))
                                   .map_err(|cause| {
                                       error(format!("platform memory default application stack \
                                                      ({application_stack} requested bytes) \
                                                      cannot determine buddy backing: {cause}"))
                                   })?;
        if application < stack_backing
        {
            return Err(error(format!("platform memory process_accounts.application_bytes \
                                      ({application} bytes) must cover the selected default \
                                      application stack ({application_stack} requested bytes, \
                                      {stack_backing} bytes of buddy backing); increase the \
                                      process application cap or reduce the stack default")));
        }
        Ok(Checked { kernel,
                     application })
    }
}
impl Checked
{
    pub(super) fn constants(&self) -> [(&'static str, u64); 2]
    {
        [("PROCESS_KERNEL_BUDGET_BYTES", self.kernel),
         ("PROCESS_APPLICATION_BUDGET_BYTES", self.application)]
    }
}
/// Module workers and their metadata share one kernel account. This is a cap,
/// not physical admission or permission to activate a worker backend.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ModuleDefinition
{
    kernel_bytes:       String,
    worker_stack_bytes: String
}
#[derive(Debug)]
pub(super) struct ModuleChecked
{
    kernel:       u64,
    worker_stack: u64
}
impl ModuleDefinition
{
    pub(super) fn check(&self, page: u64) -> Result<ModuleChecked>
    {
        let check = |name: &str, text: &str| -> Result<u64> {
            let value = address(&format!("module_accounts.{name}"), text)?;
            if value == 0 || !value.is_multiple_of(page)
            {
                return Err(error(format!("platform memory module_accounts.{name} must be \
                                          nonzero and page aligned ({page} bytes)")));
            }
            Ok(value)
        };
        let kernel = check("kernel_bytes", &self.kernel_bytes)?;
        let worker_stack = check("worker_stack_bytes", &self.worker_stack_bytes)?;
        let backing =
            PageGeometry::new(page).and_then(|geometry| geometry.buddy_bytes(worker_stack))
                                   .map_err(|cause| {
                                       error(format!("platform memory \
                                                      module_accounts.worker_stack_bytes cannot \
                                                      determine buddy backing: {cause}"))
                                   })?;
        if kernel < backing
        {
            return Err(error(format!("platform memory module_accounts.kernel_bytes ({kernel} \
                                      bytes) must cover the selected worker stack \
                                      ({worker_stack} requested bytes, {backing} bytes of \
                                      buddy backing); increase the module cap or reduce its \
                                      worker stack")));
        }
        Ok(ModuleChecked { kernel,
                           worker_stack })
    }
}
impl ModuleChecked
{
    pub(super) fn constants(&self) -> [(&'static str, u64); 2]
    {
        [("MODULE_KERNEL_BUDGET_BYTES", self.kernel),
         ("MODULE_WORKER_STACK_BYTES", self.worker_stack)]
    }
}

#[cfg(test)]
mod tests
{
    use super::*;

    fn definition() -> Definition
    {
        serde_json::from_str(r#"{"kernel_bytes":"0x1000000","application_bytes":"0x4000000"}"#).unwrap()
    }
    fn memory() -> serde_json::Value
    {
        serde_json::from_str::<serde_json::Value>(include_str!("../../../../../platforms/\
                                                                QemuRISC-V.json")).unwrap()
            ["memory"]
                      .clone()
    }
    #[test]
    fn selected_process_caps_are_configurable_without_minimum_or_maximum_policy()
    {
        let selected = definition();
        assert_eq!(selected.check(4096, 4 << 20).unwrap().constants(),
                   [("PROCESS_KERNEL_BUDGET_BYTES", 16 << 20),
                    ("PROCESS_APPLICATION_BUDGET_BYTES", 64 << 20)]);
        let mut changed = selected;
        changed.kernel_bytes = "0x1000".into();
        changed.application_bytes = "0x400000".into();
        assert_eq!(changed.check(4096, 4 << 20).unwrap().constants(),
                   [("PROCESS_KERNEL_BUDGET_BYTES", 4096),
                    ("PROCESS_APPLICATION_BUDGET_BYTES", 4 << 20)]);
    }
    #[test]
    fn invalid_caps_refuse_with_the_exact_field_and_recovery_information()
    {
        for field in ["kernel_bytes", "application_bytes"]
        {
            for value in ["0x0", "0x1001"]
            {
                let mut input = serde_json::to_value(serde_json::json!({"kernel_bytes":"0x1000000","application_bytes":"0x4000000"})).unwrap();
                input[field] = value.into();
                let definition: Definition = serde_json::from_value(input).unwrap();
                let message = definition.check(4096, 4 << 20).unwrap_err().to_string();
                assert!(message.contains(&format!("process_accounts.{field}")), "{message}");
                assert!(message.contains("nonzero and page aligned (4096 bytes)"), "{message}");
            }
            for value in ["16MiB", "0x10000000000000000"]
            {
                let mut input =
                    serde_json::json!({"kernel_bytes":"0x1000000","application_bytes":"0x4000000"});
                input[field] = value.into();
                let definition: Definition = serde_json::from_value(input).unwrap();
                let message = definition.check(4096, 4 << 20).unwrap_err().to_string();
                assert!(message.contains(&format!("process_accounts.{field}")), "{message}");
                assert!(message.contains("u64 0x hexadecimal value"), "{message}");
            }
        }
        let mut changed = definition();
        changed.kernel_bytes = "0xfffffffffffff000".into();
        assert!(changed.check(4096, 4 << 20)
                       .unwrap_err()
                       .to_string()
                       .contains("combined caps overflow u64"));
        let mut changed = definition();
        changed.application_bytes = "0x3ff000".into();
        let message = changed.check(4096, 4 << 20).unwrap_err().to_string();
        assert!(message.contains("must cover the selected default application stack (4194304 \
                                  requested bytes, 4194304 bytes of buddy backing)"),
                "{message}");
        assert!(message.contains("increase the process application cap or reduce the stack \
                                  default"),
                "{message}");
    }
    #[test]
    fn application_cap_covers_actual_buddy_backing_of_non_power_of_two_stack()
    {
        let mut changed = definition();
        changed.application_bytes = "0x300000".into();
        let message = changed.check(4096, 3 << 20).unwrap_err().to_string();
        assert!(message.contains("process_accounts.application_bytes (3145728 bytes)"),
                "{message}");
        assert!(message.contains("3145728 requested bytes, 4194304 bytes of buddy backing"),
                "{message}");
        assert!(message.contains("increase the process application cap or reduce the stack \
                                  default"),
                "{message}");
        changed.application_bytes = "0x400000".into();
        assert_eq!(changed.check(4096, 3 << 20).unwrap().constants()[1],
                   ("PROCESS_APPLICATION_BUDGET_BYTES", 4 << 20));
    }
    #[test]
    fn missing_fields_and_unselected_range_policy_are_not_silently_defaulted()
    {
        let missing=serde_json::from_value::<Definition>(serde_json::json!({"kernel_bytes":"0x1000000"})).err().unwrap().to_string();
        assert!(missing.contains("missing field `application_bytes`"), "{missing}");
        let unknown=serde_json::from_value::<Definition>(serde_json::json!({"kernel_bytes":"0x1000000","application_bytes":"0x4000000","maximum":"0x8000000"})).err().unwrap().to_string();
        assert!(unknown.contains("unknown field `maximum`"), "{unknown}");
        let mut input = memory();
        input.as_object_mut().unwrap().remove("process_accounts");
        let missing =
            serde_json::from_value::<super::super::Definition>(input).err().unwrap().to_string();
        assert!(missing.contains("missing field `process_accounts`"), "{missing}");
    }
    #[test]
    fn checked_process_caps_emit_resident_constants_without_reserving_boot_memory()
    {
        let input = memory();
        let original: super::super::Definition = serde_json::from_value(input.clone()).unwrap();
        let original = original.check().unwrap();
        let mut changed = input;
        changed["process_accounts"]["kernel_bytes"] = "0x1800000".into();
        changed["process_accounts"]["application_bytes"] = "0x8000000".into();
        let checked: super::super::Definition = serde_json::from_value(changed).unwrap();
        let checked = checked.check().unwrap();
        let constants = checked.system_constants();
        assert!(constants.contains("pub const PROCESS_KERNEL_BUDGET_BYTES: u64 = 0x1800000;"));
        assert!(constants.contains("pub const PROCESS_APPLICATION_BUDGET_BYTES: u64 = 0x8000000;"));
        assert_eq!(checked.reservation_constant(),
                   original.reservation_constant(),
                   "quota changes became physical boot reservations");
        assert_eq!(checked.linker_symbols(),
                   original.linker_symbols(),
                   "quota changes altered linker placement");
        assert_eq!(checked.boot_constants(),
                   original.boot_constants(),
                   "quota changes altered ROM placement");
    }
}

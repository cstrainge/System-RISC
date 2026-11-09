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


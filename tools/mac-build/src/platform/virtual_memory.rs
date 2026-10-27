//! One checked, architecture-aware virtual layout emits both build artifacts.
//! Physical RAM/topology remains a BootInfo input, not a size inferred here.
use std::fmt::Write;

use mac_kernel_model::{pages::{Architecture, PageGeometry},
                       policy::{AddressRange, UniformLayout}};
use serde::Deserialize;

use crate::{Result, error};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Region
{
    start: String,
    end:   String
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Mode
{
    Sv39
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Definition
{
    mode:                   Mode,
    application:            Region,
    process_kernel:         Region,
    modules:                Region,
    system:                 Region,
    physmap:                Region,
    application_image_base: String,
    system_image_base:      String
}
/// Build validation performs no runtime allocation or page-table access. This
/// explicitly chosen host gate is never the resident physical pool's gate.
struct HostValidation;
impl mac_object_table::shared::InvocationGate for HostValidation
{
    fn run<R>(operation: impl FnOnce() -> R) -> R
    {
        operation()
    }
}
struct LayoutOnlySv39;
impl Architecture for LayoutOnlySv39
{
    type InvocationGate = HostValidation;
    type PageTable = [u64; 512];
    type TrapFrame = ();

    fn geometry() -> PageGeometry
    {
        PageGeometry::new(mac_riscv_pages::Sv39::PAGE_BYTES).expect("Sv39 base pages are 4096 \
                                                                     bytes")
    }

    fn leaf_sizes() -> &'static [u64]
    {
        mac_riscv_pages::Sv39::LEAF_BYTES
    }

    fn valid_virtual_range(range: AddressRange) -> bool
    {
        mac_riscv_pages::Sv39::canonical_range(range)
    }
}
fn address(field: &str, value: &str) -> Result<u64>
{
    value.strip_prefix("0x").and_then(|v|u64::from_str_radix(v,16).ok())
        .ok_or_else(||error(format!("platform virtual_memory.{field} must be a u64 0x hexadecimal address")))
}
fn region(field: &str, value: &Region, page: u64) -> Result<AddressRange>
{
    let start = address(&format!("{field}.start"), &value.start)?;
    let end = address(&format!("{field}.end"), &value.end)?;
    let range = AddressRange::new(start, end).map_err(|cause| {
                                                 error(format!("platform virtual_memory.{field}: \
                                                                {cause}"))
                                             })?;
    if !start.is_multiple_of(page)
       || !end.is_multiple_of(page)
       || !LayoutOnlySv39::valid_virtual_range(range)
    {
        return Err(error(format!("platform virtual_memory.{field} must be page aligned and \
                                  stay inside one canonical Sv39 half; got \
                                  {start:#x}..{end:#x}")));
    }
    Ok(range)
}
pub struct Checked
{
    layout:       UniformLayout,
    system_image: u64
}
impl Definition
{
    pub(super) fn check(&self, target: &str, page: u64) -> Result<Checked>
    {
        match self.mode
        {
            Mode::Sv39 =>
            {
                if target != "riscv64imac-unknown-none-elf" || page != 4096
                {
                    return Err(error(format!("platform sv39 virtual_memory requires the RV64 \
                                              target and 4096-byte pages; got \
                                              {target},page_bytes={page}")));
                }
            }
        }
        let app = region("application", &self.application, page)?;
        let process = region("process_kernel", &self.process_kernel, page)?;
        let modules = region("modules", &self.modules, page)?;
        let system = region("system", &self.system, page)?;
        let physmap = region("physmap", &self.physmap, page)?;
        let image = address("application_image_base", &self.application_image_base)?;
        let system_image = address("system_image_base", &self.system_image_base)?;
        let layout=UniformLayout::new(app,process,modules,system,physmap,image)
            .map_err(|cause|error(format!("platform virtual_memory: {cause}")))?;
        layout.validate_architecture::<LayoutOnlySv39>()
              .map_err(|cause| error(format!("platform virtual_memory: {cause}")))?;
        if !(system.start()..system.end()).contains(&system_image)
           || (physmap.start()..physmap.end()).contains(&system_image)
           || !system_image.is_multiple_of(page)
        {
            return Err(error("platform virtual_memory.system_image_base must be page aligned, \
                              inside System and outside the physmap"));
        }
        if image == 0
        {
            return Err(error("platform virtual_memory.application_image_base must preserve the \
                              null guard"));
        }
        Ok(Checked { layout,
                     system_image })
    }
}
impl Checked
{
    fn symbols(&self) -> impl Iterator<Item = (&'static str, u64)>
    {
        let l = &self.layout;
        [("VM_ADDRESS_BITS", 39),
         ("VM_APPLICATION_START", l.application().start()),
         ("VM_APPLICATION_END", l.application().end()),
         ("VM_PROCESS_KERNEL_START", l.process_kernel().start()),
         ("VM_PROCESS_KERNEL_END", l.process_kernel().end()),
         ("VM_MODULE_START", l.modules().start()),
         ("VM_MODULE_END", l.modules().end()),
         ("VM_SYSTEM_START", l.kernel().start()),
         ("VM_SYSTEM_END", l.kernel().end()),
         ("VM_PHYSMAP_START", l.physmap().start()),
         ("VM_PHYSMAP_END", l.physmap().end()),
         ("VM_APPLICATION_IMAGE_BASE", l.image_base()),
         ("VM_SYSTEM_IMAGE_BASE", self.system_image)].into_iter()
    }

    pub fn linker_symbols(&self) -> String
    {
        let mut out = String::from("\n/* Generated from the checked virtual layout. */\n");
        for (name, value) in self.symbols()
        {
            writeln!(out, "__platform_{name} = {value:#x};").unwrap();
        }
        out
    }

    pub fn rust_constants(&self) -> String
    {
        let mut out = String::from("// Generated from the checked virtual layout.\n");
        for (name, value) in self.symbols()
        {
            writeln!(out, "pub const {name}: u64 = {value:#x};").unwrap();
        }
        out
    }
}
#[cfg(test)]
mod tests
{
    use super::*;
    fn definition() -> Definition
    {
        serde_json::from_value(serde_json::from_str::<serde_json::Value>(include_str!("../../../../platforms/QemuRISC-V.json")).unwrap()["virtual_memory"].clone()).unwrap()
    }
    #[test]
    fn approved_layout_emits_identical_rust_and_linker_values()
    {
        let checked = definition().check("riscv64imac-unknown-none-elf", 4096).unwrap();
        for (name, value) in checked.symbols()
        {
            assert!(checked.linker_symbols().contains(&format!("__platform_{name} = {value:#x};")));
            assert!(checked.rust_constants()
                           .contains(&format!("pub const {name}: u64 = {value:#x};")));
        }
        assert_eq!(checked.layout.image_base(), 4 * 1024 * 1024);
        assert_eq!(checked.layout.physmap().end() - checked.layout.physmap().start(), 64 << 30);
    }
    #[test]
    fn malformed_noncanonical_overlapping_and_foreign_arch_layouts_fail_informatively()
    {
        let target = "riscv64imac-unknown-none-elf";
        let check_error = |d: &Definition, word: &str| {
            let error = d.check(target, 4096).err().unwrap().to_string();
            assert!(error.contains(word), "{error}");
        };
        let mut d = definition();
        d.application.start = "0x400001".into();
        check_error(&d, "page aligned");
        let mut d = definition();
        d.process_kernel.start = "0x0000004000000000".into();
        check_error(&d, "canonical Sv39");
        let mut d = definition();
        d.application.end = "0xffffffc000000000".into();
        check_error(&d, "canonical Sv39");
        let mut d = definition();
        d.modules.start = "0xffffffc400000000".into();
        check_error(&d, "overlap");
        let mut d = definition();
        d.physmap.start = "0xffffffc000000000".into();
        check_error(&d, "physmap");
        let mut d = definition();
        d.system_image_base = d.physmap.start.clone();
        check_error(&d, "outside the physmap");
        let mut d = definition();
        d.application_image_base = "0x0".into();
        check_error(&d, "image base");
        let d = definition();
        assert!(d.check("aarch64-unknown-none", 4096)
                 .err()
                 .unwrap()
                 .to_string()
                 .contains("requires the RV64 target"));
        assert!(d.check(target, 65536).err().unwrap().to_string().contains("4096-byte pages"));
    }
}

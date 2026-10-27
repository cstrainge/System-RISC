//! One checked, architecture-aware virtual layout emits both build artifacts.
//! Physical RAM/topology remains a BootInfo input, not a size inferred here.
use std::fmt::Write;

use mac_kernel_model::{pages::PageLayout,
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
    system_image_base:      String,
    system_stacks:          Region,
    physmap_physical_base:  String
}
use mac_riscv_pages::Sv39;
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
    if !start.is_multiple_of(page) || !end.is_multiple_of(page) || !Sv39::valid_virtual_range(range)
    {
        return Err(error(format!("platform virtual_memory.{field} must be page aligned and \
                                  stay inside one canonical Sv39 half; got \
                                  {start:#x}..{end:#x}")));
    }
    Ok(range)
}
pub struct Checked
{
    layout:                UniformLayout,
    system_image:          u64,
    system_image_limit:    u64,
    system_stacks:         AddressRange,
    system_stack_stride:   u64,
    physmap_physical_base: u64
}
impl Definition
{
    pub(super) fn check(&self, target: &str, memory: &super::memory::Checked) -> Result<Checked>
    {
        let page = memory.page_bytes;
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
        let system_stacks = region("system_stacks", &self.system_stacks, page)?;
        let layout=UniformLayout::new(app,process,modules,system,physmap,image)
            .map_err(|cause|error(format!("platform virtual_memory: {cause}")))?;
        layout.validate_architecture::<Sv39>()
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
        let system_image_limit =
            system_image.checked_add(memory.application_primary_base - memory.images.start)
                        .ok_or_else(|| error("platform virtual System image extent overflows"))?;
        let system_stack_stride =
            memory.system_stack_maximum().checked_add(2 * page).ok_or_else(|| {
                                                                    error("platform virtual \
                                                                           System stack \
                                                                           slot/guards overflow")
                                                                })?;
        let overlaps =
            |a: AddressRange, b: AddressRange| a.start() < b.end() && b.start() < a.end();
        let image_range = AddressRange::new(system_image, system_image_limit).map_err(|cause| {
                              error(format!("platform virtual System image: {cause}"))
                          })?;
        if !Sv39::valid_virtual_range(image_range)
           || image_range.end() > system.end()
           || overlaps(image_range, physmap)
           || system_stacks.start() < system.start()
           || system_stacks.end() > system.end()
           || overlaps(system_stacks, physmap)
           || overlaps(system_stacks, image_range)
           || system_stacks.end() - system_stacks.start() < system_stack_stride
        {
            return Err(error("platform virtual System image and guarded stack window must be \
                              disjoint inside System, outside physmap; stack window must fit \
                              the maximum stack and two guards"));
        }
        let physmap_physical_base = address("physmap_physical_base", &self.physmap_physical_base)?;
        let physical_end = physmap_physical_base.checked_add(physmap.end() - physmap.start())
                                                .ok_or_else(|| {
                                                    error("platform virtual_memory physmap \
                                                           physical extent overflows")
                                                })?;
        if !physmap_physical_base.is_multiple_of(page) || physical_end > 1 << 56
        {
            return Err(error("platform virtual_memory physmap physical extent must be page \
                              aligned and fit Sv39 physical addresses"));
        }
        if physmap_physical_base > memory.ram.start || physical_end < memory.ram.end
        {
            return Err(error("platform physmap must cover the complete minimum RAM reservation"));
        }
        Ok(Checked { layout,
                     system_image,
                     system_image_limit,
                     system_stacks,
                     system_stack_stride,
                     physmap_physical_base })
    }
}
impl Checked
{
    pub fn system_image_range(&self) -> core::ops::Range<u64>
    {
        self.system_image..self.system_image_limit
    }

    fn symbols(&self) -> impl Iterator<Item = (&'static str, u64)>
    {
        let l = &self.layout;
        [("VM_ADDRESS_BITS", 39),
         ("VM_PHYSMAP_PHYSICAL_BASE", self.physmap_physical_base),
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
         ("VM_SYSTEM_IMAGE_BASE", self.system_image),
         ("VM_SYSTEM_IMAGE_LIMIT", self.system_image_limit),
         ("VM_SYSTEM_STACKS_START", self.system_stacks.start()),
         ("VM_SYSTEM_STACKS_END", self.system_stacks.end()),
         ("VM_SYSTEM_STACK_STRIDE", self.system_stack_stride)].into_iter()
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

    /// Emit only the facts a native consumer uses. This avoids duplicate
    /// handwritten values and unused placeholders in a private machine module.
    pub fn rust_constants_for(&self, names: &[&str]) -> Result<String>
    {
        let mut out =
            String::from("// Generated selected facts from the checked virtual layout.\n");
        for &name in names
        {
            let (_,value)=self.symbols().find(|(symbol,_)|*symbol==name)
                .ok_or_else(||error(format!("unknown checked virtual layout symbol {name}")))?;
            writeln!(out, "pub const {name}: u64 = {value:#x};").unwrap();
        }
        Ok(out)
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
        let checked = definition().check("riscv64imac-unknown-none-elf",
                                         &super::super::current().memory().unwrap())
                                  .unwrap();
        for (name, value) in checked.symbols()
        {
            assert!(checked.linker_symbols().contains(&format!("__platform_{name} = {value:#x};")));
            assert!(checked.rust_constants()
                           .contains(&format!("pub const {name}: u64 = {value:#x};")));
        }
        let selected =
            checked.rust_constants_for(&["VM_SYSTEM_IMAGE_BASE", "VM_PHYSMAP_START"]).unwrap();
        assert_eq!(selected.matches("pub const").count(), 2);
        assert!(selected.contains("VM_SYSTEM_IMAGE_BASE: u64 = 0xffffffd000200000"));
        assert!(checked.rust_constants_for(&["VM_UNKNOWN"])
                       .unwrap_err()
                       .to_string()
                       .contains("unknown checked virtual layout symbol VM_UNKNOWN"));
        assert_eq!(checked.layout.image_base(), 4 * 1024 * 1024);
        assert_eq!(checked.layout.physmap().end() - checked.layout.physmap().start(), 64 << 30);
        assert_eq!(checked.system_image_limit - checked.system_image, 14 << 20);
        assert_eq!(checked.system_stack_stride, (8 << 20) + 8192);
    }
    #[test]
    fn malformed_noncanonical_overlapping_and_foreign_arch_layouts_fail_informatively()
    {
        let target = "riscv64imac-unknown-none-elf";
        let check_error = |d: &Definition, word: &str| {
            let error = d.check(target, &super::super::current().memory().unwrap())
                         .err()
                         .unwrap()
                         .to_string();
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
        let mut stacks = definition();
        stacks.system_stacks.start = stacks.system_image_base.clone();
        check_error(&stacks, "guarded stack window");
        let mut stacks = definition();
        stacks.system_stacks.end = "0xffffffd010001000".into();
        check_error(&stacks, "maximum stack and two guards");
        let mut image = definition();
        image.system_image_base = "0xfffffffffff00000".into();
        check_error(&image, "overflows");
        let d = definition();
        assert!(d.check("aarch64-unknown-none", &super::super::current().memory().unwrap())
                 .err()
                 .unwrap()
                 .to_string()
                 .contains("requires the RV64 target"));
        assert!(d.check(target, &{
                     let mut memory = super::super::current().memory().unwrap();
                     memory.page_bytes = 65536;
                     memory
                 })
                 .err()
                 .unwrap()
                 .to_string()
                 .contains("4096-byte pages"));
    }
}

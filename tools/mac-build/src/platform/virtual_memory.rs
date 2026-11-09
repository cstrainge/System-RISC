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
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Mode
{
    Sv39,
    /// A 39-bit, four-level aarch64 layout. The page-table format is aarch64's;
    /// the virtual windows are the same OS policy the RISC-V record states.
    Aarch64
}

/// One supported virtual layout. Each implementation names the architecture it
/// belongs to and owns its canonical-range rules, so shared validation never
/// compares a literal target string.
trait Layout: PageLayout
{
    const NAME: &'static str;
    const TARGET: &'static str;
    const TARGET_DESCRIPTION: &'static str;
    const ROOT_BYTES: u64;
}

use mac_riscv_pages::Sv39;

struct Sv39Layout;
impl PageLayout for Sv39Layout
{
    fn geometry() -> mac_kernel_model::pages::PageGeometry
    {
        Sv39::geometry()
    }

    fn leaf_sizes() -> &'static [u64]
    {
        Sv39::leaf_sizes()
    }

    fn valid_virtual_range(range: AddressRange) -> bool
    {
        Sv39::valid_virtual_range(range)
    }

    fn lower_half_end() -> u64
    {
        1 << 38
    }

    fn upper_half_start() -> u64
    {
        u64::MAX << 38
    }
}
impl Layout for Sv39Layout
{
    const NAME: &'static str = "Sv39";
    const ROOT_BYTES: u64 = 1 << 30;
    const TARGET: &'static str = "riscv64imac-unknown-none-elf";
    const TARGET_DESCRIPTION: &'static str = "RV64";
}

/// The aarch64 39-bit canonical address space. With a 4 KiB granule the walk
/// starts at level 1, so the leaves are the 1 GiB block, 2 MiB block and 4 KiB
/// page of that three-level format.
struct Aarch64Layout;
impl PageLayout for Aarch64Layout
{
    fn geometry() -> mac_kernel_model::pages::PageGeometry
    {
        mac_kernel_model::pages::PageGeometry::new(4096).expect("the aarch64 base page geometry \
                                                                 is 4096 bytes")
    }

    fn leaf_sizes() -> &'static [u64]
    {
        &[1 << 30, 1 << 21, 1 << 12]
    }

    fn valid_virtual_range(range: AddressRange) -> bool
    {
        let (start, end) = (range.start(), range.end());
        (start < 0x0000_0080_0000_0000 && end <= 0x0000_0080_0000_0000)
        || start >= 0xffff_ff80_0000_0000
    }

    fn lower_half_end() -> u64
    {
        0x0000_0080_0000_0000
    }

    fn upper_half_start() -> u64
    {
        0xffff_ff80_0000_0000
    }
}
impl Layout for Aarch64Layout
{
    const NAME: &'static str = "aarch64";
    const ROOT_BYTES: u64 = 1 << 30;
    const TARGET: &'static str = "aarch64-unknown-none";
    const TARGET_DESCRIPTION: &'static str = "aarch64";
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
    system_heap_base:       String,
    system_stacks:          Region,
    physmap_physical_base:  String
}
fn address(field: &str, value: &str) -> Result<u64>
{
    value.strip_prefix("0x").and_then(|v|u64::from_str_radix(v,16).ok())
        .ok_or_else(||error(format!("platform virtual_memory.{field} must be a u64 0x hexadecimal address")))
}
fn region(field: &str, value: &Region, page: u64, valid: fn(AddressRange) -> bool, half: &str)
          -> Result<AddressRange>
{
    let start = address(&format!("{field}.start"), &value.start)?;
    let end = address(&format!("{field}.end"), &value.end)?;
    let range = AddressRange::new(start, end).map_err(|cause| {
                                                 error(format!("platform virtual_memory.{field}: \
                                                                {cause}"))
                                             })?;
    if !start.is_multiple_of(page) || !end.is_multiple_of(page) || !valid(range)
    {
        return Err(error(format!("platform virtual_memory.{field} must be page aligned and \
                                  stay inside one canonical {half} half; got \
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
    system_heap:           AddressRange,
    physmap_physical_base: u64
}
impl Definition
{
    pub(super) fn check(&self, target: &str, memory: &super::memory::Checked) -> Result<Checked>
    {
        match self.mode
        {
            Mode::Sv39 => self.check_layout::<Sv39Layout>(target, memory),
            Mode::Aarch64 => self.check_layout::<Aarch64Layout>(target, memory)
        }
    }

    fn check_layout<L: Layout>(&self, target: &str, memory: &super::memory::Checked)
                               -> Result<Checked>
    {
        let page = memory.page_bytes;
        if target != L::TARGET || page != 4096
        {
            return Err(error(format!("platform {} virtual_memory requires the {} target and \
                                      4096-byte pages; got {target},page_bytes={page}",
                                     L::NAME,
                                     L::TARGET_DESCRIPTION)));
        }
        let valid = L::valid_virtual_range as fn(AddressRange) -> bool;
        let half = L::NAME;
        let root_bytes = L::ROOT_BYTES;
        let app = region("application", &self.application, page, valid, half)?;
        let process = region("process_kernel", &self.process_kernel, page, valid, half)?;
        let modules = region("modules", &self.modules, page, valid, half)?;
        // The initial dynamic module mapper owns dedicated root slots in both
        // System views. It never replaces an existing System/physmap subtree.
        if !modules.start().is_multiple_of(root_bytes) || !modules.end().is_multiple_of(root_bytes)
        {
            return Err(error("platform virtual_memory.modules must occupy whole root slots; \
                              dynamic module branches cannot share a bootstrap System subtree"));
        }
        let system = region("system", &self.system, page, valid, half)?;
        let physmap = region("physmap", &self.physmap, page, valid, half)?;
        let image = address("application_image_base", &self.application_image_base)?;
        let system_image = address("system_image_base", &self.system_image_base)?;
        let system_stacks = region("system_stacks", &self.system_stacks, page, valid, half)?;
        let system_heap_base = address("system_heap_base", &self.system_heap_base)?;
        let system_heap_limit = system_heap_base.checked_add(memory.system_heap_bytes)
                                                .ok_or_else(|| {
                                                    error("platform virtual_memory.\
                                                           system_heap_base + system_heap_bytes \
                                                           overflows")
                                                })?;
        let system_heap = region("system_heap",
                                 &Region { start: format!("{system_heap_base:#x}"),
                                           end:   format!("{system_heap_limit:#x}") },
                                 page,
                                 valid,
                                 half)?;
        let layout=UniformLayout::new(app,process,modules,system,physmap,image)
            .map_err(|cause|error(format!("platform virtual_memory: {cause}")))?;
        layout.validate_architecture::<L>()
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
        if !valid(image_range)
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
        if system_heap.start() < system.start()
           || system_heap.end() > system.end()
           || overlaps(system_heap, physmap)
           || overlaps(system_heap, image_range)
           || overlaps(system_heap, system_stacks)
        {
            return Err(error("platform virtual System heap must be disjoint from the complete \
                              image, guarded stack window and physmap, and contained inside \
                              System"));
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
                              aligned and fit the platform's physical address space"));
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
                     system_heap,
                     physmap_physical_base })
    }
}
impl Checked
{
    /// Execution VA capacity only. Actual module pages are admitted against
    /// discovered RAM and their original module Kernel accounts at runtime.
    pub fn module_capacity(&self) -> u64
    {
        self.layout.modules().end() - self.layout.modules().start()
    }

    /// Shared link geometry for separate Process address spaces, never a
    /// physical reservation. Packaging retains only actual segment extent.
    pub fn application_image_range(&self) -> core::ops::Range<u64>
    {
        self.layout.image_base()..self.layout.application().end()
    }

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
         ("VM_SYSTEM_STACK_STRIDE", self.system_stack_stride),
         ("VM_SYSTEM_HEAP_BASE", self.system_heap.start()),
         ("VM_SYSTEM_HEAP_LIMIT", self.system_heap.end())].into_iter()
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

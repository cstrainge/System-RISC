//! Checked physical placement policy. Discovered RAM remains a boot-time input.
use std::{fmt::Write, ops::Range};

use serde::Deserialize;

use crate::{Result, error};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Region
{
    base:  String,
    bytes: String
}
/// Display size in pixels, from the platform record. This is the single
/// build-time source of display geometry; the guest reads it through the
/// generated DISPLAY_WIDTH/DISPLAY_HEIGHT constants. A future Display Control
/// Panel changes the size by backing those constants with a runtime
/// preference instead of this record; runtime switching also requires dynamic
/// framebuffers, caches and scratch, not just this value.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Display
{
    width:  u64,
    height: u64
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Definition
{
    page_bytes:                  u64,
    ram_base:                    String,
    minimum_ram_bytes:           String,
    flash:                       Region,
    boot:                        Region,
    firmware_heap:               Region,
    images:                      Region,
    modules:                     Region,
    display_scratch:             Region,
    display:                     Display,
    application_relocation_base: String,
    application_primary_base:    String,
    definitions:                 Region,
    fdt_tail_bytes:              String,
    boot_stack_bytes:            String,
    system_heap_bytes:           String
}
/// Only validation can construct this record. All generated artifacts use it.
#[derive(Debug)]
pub struct Checked
{
    pub(super) page_bytes:                  u64,
    pub(super) ram:                         Range<u64>,
    pub(super) flash:                       Range<u64>,
    pub(super) boot:                        Range<u64>,
    pub(super) firmware_heap:               Range<u64>,
    pub(super) images:                      Range<u64>,
    pub(super) modules:                     Range<u64>,
    pub(super) display_scratch:             Range<u64>,
    pub(super) display_width:               u64,
    pub(super) display_height:              u64,
    pub(super) application_relocation_base: u64,
    pub(super) application_primary_base:    u64,
    pub(super) definitions:                 Range<u64>,
    pub(super) fdt_tail_bytes:              u64,
    pub(super) boot_stack_bytes:            u64,
    pub(super) system_heap_bytes:           u64
}
fn address(name: &str, text: &str) -> Result<u64>
{
    text.strip_prefix("0x")
        .and_then(|hex| u64::from_str_radix(hex, 16).ok())
        .ok_or_else(|| error(format!("platform memory {name} must be a u64 0x hexadecimal value")))
}
fn region(name: &str, value: &Region, page: u64) -> Result<Range<u64>>
{
    let base = address(&format!("{name}.base"), &value.base)?;
    let bytes = address(&format!("{name}.bytes"), &value.bytes)?;
    let end = base.checked_add(bytes)
                  .ok_or_else(|| error(format!("platform memory {name} overflows")))?;
    if bytes == 0 || !base.is_multiple_of(page) || !bytes.is_multiple_of(page)
    {
        return Err(error(format!("platform memory {name} must be nonempty and page aligned")));
    }
    Ok(base..end)
}
fn overlaps(a: &Range<u64>, b: &Range<u64>) -> bool
{
    a.start < b.end && b.start < a.end
}
impl Definition
{
    pub(super) fn check(&self) -> Result<Checked>
    {
        let page = self.page_bytes;
        if !page.is_power_of_two() || page < 4096
        {
            return Err(error("platform memory page_bytes must be a power of two of at least \
                              4096"));
        }
        let ram = region("minimum RAM",
                         &Region { base:  self.ram_base.clone(),
                                   bytes: self.minimum_ram_bytes.clone() },
                         page)?;
        let flash = region("flash", &self.flash, page)?;
        let boot = region("boot", &self.boot, page)?;
        let firmware_heap = region("firmware_heap", &self.firmware_heap, page)?;
        let images = region("images", &self.images, page)?;
        let modules = region("modules", &self.modules, page)?;
        let display_scratch = region("display_scratch", &self.display_scratch, page)?;
        let display_width = self.display.width;
        let display_height = self.display.height;
        if display_width == 0
           || display_height <= 21
           || display_width > i16::MAX as u64
           || display_height > i16::MAX as u64
        {
            return Err(error("platform display must be wider than 0, taller than 21 and fit \
                              signed 16-bit geometry"));
        }
        let framebuffer = display_width.checked_mul(display_height)
                                       .and_then(|pixels| pixels.checked_mul(4))
                                       .ok_or_else(|| error("platform display size overflows"))?;
        let framebuffer_reservation =
            framebuffer.checked_add(page - 1)
                       .map(|bytes| bytes / page * page)
                       .ok_or_else(|| error("platform display reservation overflows"))?;
        let display_bytes = framebuffer_reservation.checked_mul(2).ok_or_else(|| {
                                                                       error("platform display \
                                                                              frontbuffer/\
                                                                              tracking reservation \
                                                                              overflows")
                                                                   })?;
        if display_scratch.end - display_scratch.start < display_bytes
        {
            return Err(error(format!("platform display_scratch must cover the \
                                      {display_width}x{display_height} framebuffer and \
                                      tracking raster ({display_bytes} bytes)")));
        }
        let tail = address("fdt_tail_bytes", &self.fdt_tail_bytes)?;
        if tail == 0 || !tail.is_multiple_of(page) || tail >= ram.end - ram.start
        {
            return Err(error("platform memory fdt_tail_bytes must be nonzero, page aligned and \
                              smaller than minimum RAM"));
        }
        let fdt = ram.end - tail..ram.end;
        let regions = [("boot", &boot),
                       ("firmware_heap", &firmware_heap),
                       ("images", &images),
                       ("display_scratch", &display_scratch),
                       ("modules", &modules),
                       ("FDT tail", &fdt)];
        for (index, (name, range)) in regions.iter().enumerate()
        {
            if range.start < ram.start || range.end > ram.end
            {
                return Err(error(format!("platform memory {name} lies outside minimum RAM")));
            }
            for (previous, other) in &regions[..index]
            {
                if overlaps(range, other)
                {
                    return Err(error(format!("platform memory {name} overlaps {previous}")));
                }
            }
            if overlaps(range, &flash)
            {
                return Err(error(format!("platform memory flash overlaps {name}")));
            }
        }
        if overlaps(&flash, &ram)
        {
            return Err(error("platform memory flash overlaps minimum RAM"));
        }
        let application_relocation_base =
            address("application_relocation_base", &self.application_relocation_base)?;
        if !application_relocation_base.is_multiple_of(page)
           || !images.contains(&application_relocation_base)
        {
            return Err(error("platform memory application_relocation_base must be page aligned \
                              inside images"));
        }
        let application_primary_base =
            address("application_primary_base", &self.application_primary_base)?;
        let definitions = region("definitions", &self.definitions, page)?;
        if !application_primary_base.is_multiple_of(page)
           || application_primary_base <= images.start
           || application_primary_base >= definitions.start
           || definitions.end > application_relocation_base
           || definitions.start < images.start
        {
            return Err(error("platform memory application/definition regions overlap, are \
                              unaligned or lie outside images"));
        }
        let boot_stack_bytes = address("boot_stack_bytes", &self.boot_stack_bytes)?;
        if boot_stack_bytes == 0
           || !boot_stack_bytes.is_multiple_of(16)
           || boot_stack_bytes >= boot.end - boot.start
        {
            return Err(error("platform memory boot_stack_bytes must be nonzero, 16-byte \
                              aligned and smaller than boot RAM"));
        }
        let system_heap_bytes = address("system_heap_bytes", &self.system_heap_bytes)?;
        if system_heap_bytes == 0
           || !system_heap_bytes.is_multiple_of(page)
           || system_heap_bytes >= images.end - images.start
        {
            return Err(error("platform memory system_heap_bytes must be nonzero, page aligned \
                              and smaller than images"));
        }
        Ok(Checked { page_bytes: page,
                     ram,
                     flash,
                     boot,
                     firmware_heap,
                     images,
                     modules,
                     display_scratch,
                     display_width,
                     display_height,
                     application_relocation_base,
                     application_primary_base,
                     definitions,
                     fdt_tail_bytes: tail,
                     boot_stack_bytes,
                     system_heap_bytes })
    }
}
impl Checked
{
    pub fn display_dimensions(&self) -> (usize, usize)
    {
        (self.display_width as usize, self.display_height as usize)
    }

    pub fn display_constants(&self) -> String
    {
        format!("// Generated from the checked platform display record.\npub(crate) const WIDTH: \
                 usize = {};\npub(crate) const HEIGHT: usize = {};\n",
                self.display_dimensions().0,
                self.display_dimensions().1)
    }

    pub(super) fn permits_mmio(&self, range: core::ops::Range<u64>) -> bool
    {
        !overlaps(&range, &self.ram) && !overlaps(&range, &self.flash)
    }

    pub fn validate_image(&self, role: mac_formats::native::Role, base: u64, limit: u64)
                          -> Result<()>
    {
        use mac_formats::native::Role;
        let valid =
            base < limit
            && base.is_multiple_of(self.page_bytes)
            && limit.is_multiple_of(self.page_bytes)
            && match role
            {
                Role::System => base >= self.images.start && limit <= self.application_primary_base,
                Role::Application =>
                {
                    (base >= self.application_primary_base && limit <= self.definitions.start)
                    || (base >= self.application_relocation_base && limit <= self.images.end)
                }
                Role::CodeResource =>
                {
                    base >= self.definitions.start && limit <= self.definitions.end
                }
            };
        if !valid
        {
            return Err(error(format!("native {role:?} reservation {base:#x}..{limit:#x} \
                                      conflicts with the selected platform memory map; update \
                                      its manifest or platform definition and rebuild")));
        }
        Ok(())
    }

    pub fn flash_base(&self) -> u64
    {
        self.flash.start
    }

    pub fn flash_bytes(&self) -> u64
    {
        self.flash.end - self.flash.start
    }

    pub(super) fn linker_symbols(&self) -> String
    {
        let mut out = String::from("\n/* Generated from the checked platform memory record. */\n");
        for (name, region) in [("FLASH", &self.flash),
                               ("BOOT", &self.boot),
                               ("FIRMWARE_HEAP", &self.firmware_heap),
                               ("IMAGES", &self.images),
                               ("MODULES", &self.modules),
                               ("DISPLAY_SCRATCH", &self.display_scratch),
                               ("DEFINITIONS", &self.definitions)]
        {
            writeln!(out, "__platform_{name}_START = {:#x};", region.start).unwrap();
            writeln!(out, "__platform_{name}_END = {:#x};", region.end).unwrap();
        }
        for (name, value) in [("PAGE_BYTES", self.page_bytes),
                              ("RAM_BASE", self.ram.start),
                              ("MINIMUM_RAM_BYTES", self.ram.end - self.ram.start),
                              ("APPLICATION_RELOCATION_BASE", self.application_relocation_base),
                              ("APPLICATION_PRIMARY_BASE", self.application_primary_base),
                              ("FDT_TAIL_BYTES", self.fdt_tail_bytes),
                              ("BOOT_STACK_BYTES", self.boot_stack_bytes),
                              ("SYSTEM_HEAP_BYTES", self.system_heap_bytes)]
        {
            writeln!(out, "__platform_{name} = {value:#x};").unwrap();
        }
        out
    }

    pub(super) fn system_constants(&self) -> String
    {
        let mut out = String::from("// Generated from the checked platform memory record.\n");
        for (name, value) in
            [("PAGE_BYTES", self.page_bytes),
             ("MODULE_BASE", self.modules.start),
             ("DISPLAY_FRONTBUFFER_BASE", self.display_scratch.start),
             ("DISPLAY_TRACKING_BASE",
              self.display_scratch.start
              + (self.display_width * self.display_height * 4).div_ceil(self.page_bytes)
                * self.page_bytes),
             ("DISPLAY_SCRATCH_BASE", self.display_scratch.start),
             ("DISPLAY_SCRATCH_BYTES", self.display_scratch.end - self.display_scratch.start),
             ("MODULE_LIMIT", self.modules.end),
             ("FDT_TAIL_BYTES", self.fdt_tail_bytes),
             ("APPLICATION_RELOCATION_BASE", self.application_relocation_base),
             ("APPLICATION_PRIMARY_BASE", self.application_primary_base),
             ("APPLICATION_PRIMARY_LIMIT", self.definitions.start),
             ("DEFINITION_BASE", self.definitions.start),
             ("DEFINITION_LIMIT", self.definitions.end),
             ("IMAGE_LIMIT", self.images.end)]
        {
            writeln!(out, "pub const {name}: u64 = {value:#x};").unwrap();
        }
        out.push_str(application_region_source());
        writeln!(out, "pub const SYSTEM_HEAP_BYTES: usize = {:#x};",self.system_heap_bytes).unwrap();
        out
    }

    pub(super) fn boot_constants(&self) -> String
    {
        let mut out = String::from("// Generated from the checked platform memory record.\n");
        for (name, value) in [("RAM_BASE", self.ram.start),
                              ("MINIMUM_RAM_BYTES", self.ram.end - self.ram.start),
                              ("DISPLAY_WIDTH", self.display_width),
                              ("DISPLAY_HEIGHT", self.display_height),
                              ("DISPLAY_SCRATCH_BASE", self.display_scratch.start),
                              ("DISPLAY_SCRATCH_BYTES",
                               self.display_scratch.end - self.display_scratch.start),
                              ("IMAGE_BASE", self.images.start),
                              ("IMAGE_LIMIT", self.images.end),
                              ("APPLICATION_PRIMARY_BASE", self.application_primary_base),
                              ("APPLICATION_PRIMARY_LIMIT", self.definitions.start),
                              ("APPLICATION_RELOCATION_BASE", self.application_relocation_base),
                              ("PAGE_BYTES", self.page_bytes),
                              ("FDT_TAIL_BYTES", self.fdt_tail_bytes)]
        {
            writeln!(out, "pub const {name}: u64 = {value:#x};").unwrap();
        }
        out.push_str(application_region_source());
        writeln!(out,
                 "pub const BOOT_RESERVED: [(u64,u64);4] = \
                  [({:#x},{:#x}),({:#x},{:#x}),({:#x},{:#x}),({:#x},{:#x})];",
                 self.boot.start,
                 self.boot.end,
                 self.firmware_heap.start,
                 self.firmware_heap.end,
                 self.modules.start,
                 self.modules.end,
                 self.display_scratch.start,
                 self.display_scratch.end).unwrap();
        out
    }
}

fn application_region_source() -> &'static str
{
    "pub fn application_region(base: u64, limit: u64) -> bool { base < limit && ((base >= \
     APPLICATION_PRIMARY_BASE && limit <= APPLICATION_PRIMARY_LIMIT) || (base >= \
     APPLICATION_RELOCATION_BASE && limit <= IMAGE_LIMIT)) }\n"
}

#[cfg(test)]
mod tests
{
    use super::*;
    fn definition() -> Definition
    {
        let json: serde_json::Value =
            serde_json::from_str(include_str!("../../../../platforms/QemuRISC-V.json")).unwrap();
        serde_json::from_value(json["memory"].clone()).unwrap()
    }
    #[test]
    fn rejects_overlaps_alignment_overflow_and_outside_ram_before_emission()
    {
        let original = definition();
        original.check().unwrap();
        let mut bad = original.clone();
        bad.modules.base = original.firmware_heap.base.clone();
        assert!(bad.check().unwrap_err().to_string().contains("modules overlaps firmware_heap"));
        let mut bad = original.clone();
        bad.modules.base = "0x90000000".into();
        assert!(bad.check().unwrap_err().to_string().contains("modules lies outside minimum RAM"));
        let mut bad = original.clone();
        bad.modules.base = "0x87800001".into();
        assert!(bad.check()
                   .unwrap_err()
                   .to_string()
                   .contains("modules must be nonempty and page aligned"));
        let mut bad = original.clone();
        bad.modules.base = "0xfffffffffffff000".into();
        assert!(bad.check().unwrap_err().to_string().contains("modules overflows"));
        let mut bad = original.clone();
        bad.display_scratch.base = original.modules.base.clone();
        assert!(bad.check().unwrap_err().to_string().contains("modules overlaps display_scratch"));
        let mut bad = original.clone();
        bad.display_scratch.base = "0x90000000".into();
        assert!(bad.check()
                   .unwrap_err()
                   .to_string()
                   .contains("display_scratch lies outside minimum RAM"));
        let mut bad = original.clone();
        bad.display_scratch.bytes = "0x120001".into();
        assert!(bad.check()
                   .unwrap_err()
                   .to_string()
                   .contains("display_scratch must be nonempty and page aligned"));
        let mut bad = original.clone();
        bad.display.width = 0;
        assert!(bad.check().unwrap_err().to_string().contains("platform display must be"));
        let mut bad = original.clone();
        bad.display.height = 21;
        assert!(bad.check().unwrap_err().to_string().contains("platform display must be"));
        let mut bad = original.clone();
        bad.display.width = 40000;
        assert!(bad.check().unwrap_err().to_string().contains("platform display must be"));
        let mut bad = original.clone();
        bad.display_scratch.bytes = "0x3a9000".into();
        assert!(bad.check()
                   .unwrap_err()
                   .to_string()
                   .contains("display_scratch must cover the 800x600 framebuffer"));
        let mut bad = original.clone();
        bad.application_relocation_base = "0x87000000".into();
        assert!(bad.check()
                   .unwrap_err()
                   .to_string()
                   .contains("application_relocation_base must be page aligned inside images"));
        let mut bad = original.clone();
        bad.fdt_tail_bytes = "0x300000".into();
        assert!(bad.check().unwrap_err().to_string().contains("FDT tail overlaps modules"));
        let mut bad = original.clone();
        bad.boot_stack_bytes = original.boot.bytes.clone();
        assert!(bad.check().unwrap_err().to_string().contains("smaller than boot RAM"));
        let mut bad = original.clone();
        bad.system_heap_bytes = original.images.bytes.clone();
        assert!(bad.check().unwrap_err().to_string().contains("smaller than images"));
        let mut bad = original.clone();
        bad.page_bytes = 3;
        assert!(bad.check().unwrap_err().to_string().contains("page_bytes must be a power of two"));
    }
    #[test]
    fn constants_linker_and_rom_packaging_use_the_same_checked_ranges()
    {
        let mut definition = definition();
        definition.modules.bytes = "0x400000".into();
        definition.system_heap_bytes = "0x600000".into();
        let checked = definition.check().unwrap();
        let linker = checked.linker_symbols();
        let rust = checked.system_constants();
        assert!(linker.contains("__platform_MODULES_END = 0x87c00000;"));
        assert!(rust.contains("MODULE_LIMIT: u64 = 0x87c00000;"));
        assert!(linker.contains("__platform_SYSTEM_HEAP_BYTES = 0x600000;"));
        assert!(rust.contains("SYSTEM_HEAP_BYTES: usize = 0x600000;"));
        assert!(checked.boot_constants().contains("IMAGE_LIMIT: u64 = 0x86c56000;"));
        assert!(linker.contains("__platform_DISPLAY_SCRATCH_START = 0x86c56000;"));
        assert!(rust.contains("DISPLAY_SCRATCH_BASE: u64 = 0x86c56000;"));
        assert!(rust.contains("DISPLAY_SCRATCH_BYTES: u64 = 0x3aa000;"));
        assert!(rust.contains("DISPLAY_FRONTBUFFER_BASE: u64 = 0x86c56000;"));
        assert!(rust.contains("DISPLAY_TRACKING_BASE: u64 = 0x86e2b000;"));
        assert!(checked.display_constants().contains("WIDTH: usize = 800;"));
        assert!(checked.display_constants().contains("HEIGHT: usize = 600;"));
        assert!(checked.boot_constants().contains("(0x86c56000,0x87000000)"));
        assert_eq!(checked.flash_base(), checked.flash.start);
        assert_eq!(checked.flash_bytes(), checked.flash.end - checked.flash.start);
    }
    #[test]
    fn preferred_image_roles_cannot_overlap_firmware_or_another_role()
    {
        use mac_formats::native::Role;
        let checked = definition().check().unwrap();
        checked.validate_image(Role::System, 0x80200000, 0x81000000).unwrap();
        checked.validate_image(Role::Application, 0x81000000, 0x81800000).unwrap();
        checked.validate_image(Role::Application, 0x83000000, 0x83800000).unwrap();
        checked.validate_image(Role::CodeResource, 0x82000000, 0x82100000).unwrap();
        for (role, base, limit) in [(Role::System, 0x80200000, 0x81800000),
                                    (Role::Application, 0x82000000, 0x82100000),
                                    (Role::Application, 0x86f00000, 0x87100000),
                                    (Role::CodeResource, 0x83000000, 0x83100000)]
        {
            let message = checked.validate_image(role, base, limit).unwrap_err().to_string();
            assert!(message.contains("conflicts with the selected platform memory map")
                    && message.contains("update its manifest or platform definition and rebuild"),
                    "{message}");
        }
        let mut bad = definition();
        bad.definitions.base = "0x81000000".into();
        assert!(bad.check()
                   .unwrap_err()
                   .to_string()
                   .contains("application/definition regions overlap"));
    }
}

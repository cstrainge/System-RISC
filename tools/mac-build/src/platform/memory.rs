//! Checked physical placement policy. Discovered RAM remains a boot-time input.
use std::{fmt::Write, ops::Range};

use serde::Deserialize;

use crate::{Result, error};
mod stacks;
mod io;
mod accounts;
mod application_heap;
mod tls;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Region
{
    base:  String,
    bytes: String
}
// The only supported monitor profile. Unknown reset/transport values fail at
// platform deserialization; there is no CPU-only reset, hotplug or SBI fallback.
#[derive(Clone, Copy, Deserialize)]
enum MonitorReset
{
    #[serde(rename = "whole-machine")]
    WholeMachine
}
#[derive(Clone, Copy, Deserialize)]
enum MonitorBinding
{
    #[serde(rename = "legacy-clint")]
    LegacyClint
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct MonitorPolicy
{
    reset:      MonitorReset,
    binding:    MonitorBinding,
    compatible: [String; 2]
}
impl MonitorPolicy
{
    fn check(&self) -> Result<()>
    {
        // Consume the selected reset/binding before emitting this profile's
        // shared CLINT/frame geometry. This named profile supports exactly the
        // two compatible tags decoded by the actual legacy route parser.
        match (self.reset, self.binding)
        {
            (MonitorReset::WholeMachine, MonitorBinding::LegacyClint) =>
            {
                if self.compatible != ["sifive,clint0", "riscv,clint0"]
                {
                    return Err(error("legacy-clint monitor compatibility must be sifive,clint0 \
                                      and riscv,clint0; another binding requires production \
                                      support"));
                }
                Ok(())
            }
        }
    }
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
    width:      u64,
    height:     u64,
    /// Largest mode a display driver may apply; frame storage is reserved for it.
    /// Defaults to the initial size, which keeps the display fixed.
    max_width:  Option<u64>,
    max_height: Option<u64>
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Definition
{
    page_bytes:                  u64,
    ram_base:                    String,
    minimum_ram_bytes:           String,
    /// The boot payload's read-only medium. A board whose firmware loads its
    /// payload from an SD card or another medium declares no flash window.
    flash:                       Option<Region>,
    /// QEMU's firmware-configuration device. A physical board has no fw_cfg.
    fw_cfg:                      Option<Region>,
    /// The bootstrap IO windows this platform's first stage drives. A board
    /// booted by vendor firmware from removable media declares none.
    bootstrap_io:                Option<io::Definition>,
    boot:                        Region,
    firmware_heap:               Region,
    images:                      Region,
    display_scratch:             Region,
    display:                     Display,
    application_relocation_base: String,
    application_primary_base:    String,
    definitions:                 Region,
    fdt_tail_bytes:              String,
    boot_stack_bytes:            String,
    monitor_stack_bytes:         String,
    /// The monitor's reset/transport binding. Only the RISC-V legacy CLINT
    /// profile exists, so a board with a different interrupt controller omits
    /// it rather than claiming a binding it does not have.
    machine_monitor:             Option<MonitorPolicy>,
    system_heap_bytes:           String,
    system_recovery_bytes:       String,
    stacks:                      stacks::Definition,
    application_heap:            application_heap::Definition,
    tls:                         tls::Definition,
    process_accounts:            accounts::Definition,
    module_accounts:             accounts::ModuleDefinition
}
/// Only validation can construct this record. All generated artifacts use it.
#[derive(Debug)]
pub struct Checked
{
    pub(super) page_bytes:                  u64,
    pub(super) ram:                         Range<u64>,
    pub(super) flash:                       Option<Range<u64>>,
    pub(super) fw_cfg:                      Option<Range<u64>>,
    bootstrap_io:                           Option<io::Checked>,
    pub(super) boot:                        Range<u64>,
    pub(super) firmware_heap:               Range<u64>,
    pub(super) images:                      Range<u64>,
    pub(super) display_scratch:             Range<u64>,
    pub(super) display_width:               u64,
    pub(super) display_height:              u64,
    pub(super) display_max_width:           u64,
    pub(super) display_max_height:          u64,
    pub(super) application_relocation_base: u64,
    pub(super) application_primary_base:    u64,
    pub(super) definitions:                 Range<u64>,
    pub(super) fdt_tail_bytes:              u64,
    pub(super) boot_stack_bytes:            u64,
    monitor_stack_bytes:                    u64,
    pub(super) system_heap_bytes:           u64,
    system_recovery_bytes:                  u64,
    stacks:                                 stacks::Checked,
    application_heap:                       application_heap::Checked,
    tls:                                    tls::Checked,
    process_accounts:                       accounts::Checked,
    module_accounts:                        accounts::ModuleChecked
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
        let flash = self.flash.as_ref().map(|window| region("flash", window, page)).transpose()?;
        let fw_cfg =
            self.fw_cfg.as_ref().map(|window| region("fw_cfg", window, page)).transpose()?;
        if let Some(fw_cfg) = &fw_cfg
        {
            if overlaps(fw_cfg, &ram)
               || flash.as_ref().is_some_and(|flash| overlaps(fw_cfg, flash))
               || fw_cfg.end > 1 << 56
            {
                return Err(error("platform memory fw_cfg must be a physical MMIO window \
                                  outside RAM and flash"));
            }
        }
        let bootstrap_io = self.bootstrap_io
                               .as_ref()
                               .map(|io| io.check(page, &ram, flash.as_ref(), fw_cfg.as_ref()))
                               .transpose()?;
        let boot = region("boot", &self.boot, page)?;
        let firmware_heap = region("firmware_heap", &self.firmware_heap, page)?;
        let images = region("images", &self.images, page)?;
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
        let display_max_width = self.display.max_width.unwrap_or(display_width);
        let display_max_height = self.display.max_height.unwrap_or(display_height);
        if display_max_width < display_width
           || display_max_height < display_height
           || display_max_width > i16::MAX as u64
           || display_max_height > i16::MAX as u64
        {
            return Err(error("platform display maximum must not be smaller than the initial \
                              size and must fit signed 16-bit geometry"));
        }
        let framebuffer =
            display_max_width.checked_mul(display_max_height)
                             .and_then(|pixels| pixels.checked_mul(4))
                             .ok_or_else(|| error("platform display size overflows"))?;
        let framebuffer_reservation =
            framebuffer.checked_add(page - 1)
                       .map(|bytes| bytes / page * page)
                       .ok_or_else(|| error("platform display reservation overflows"))?;
        let display_bytes =
            framebuffer_reservation.checked_mul(3).ok_or_else(|| {
                                                       error("platform display \
                                                              frontbuffer/tracking/backbuffer \
                                                              reservation overflows")
                                                   })?;
        if display_scratch.end - display_scratch.start < display_bytes
        {
            return Err(error(format!("platform display_scratch must cover the \
                                      {display_max_width}x{display_max_height} framebuffer \
                                      and tracking raster and backbuffer ({display_bytes} \
                                      bytes)")));
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
            if let Some(flash) = &flash
            {
                if overlaps(range, flash)
                {
                    return Err(error(format!("platform memory flash overlaps {name}")));
                }
            }
        }
        if let Some(flash) = &flash
        {
            if overlaps(flash, &ram)
            {
                return Err(error("platform memory flash overlaps minimum RAM"));
            }
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
        if let Some(monitor) = &self.machine_monitor
        {
            monitor.check()?;
        }
        let monitor_stack_bytes = address("monitor_stack_bytes", &self.monitor_stack_bytes)?;
        if monitor_stack_bytes < page
           || !monitor_stack_bytes.is_multiple_of(page)
           || monitor_stack_bytes < mac_abi::boot::monitor::CONTROL_BYTES as u64
           || !monitor_stack_bytes.is_multiple_of(mac_abi::boot::monitor::CONTROL_ALIGNMENT as u64)
        {
            return Err(error("monitor_stack_bytes must be page aligned and cover the \
                              integer-only leaf monitor frame; Rust calls/nesting require a \
                              revised policy"));
        }
        let system_heap_bytes = address("system_heap_bytes", &self.system_heap_bytes)?;
        if system_heap_bytes == 0 || !system_heap_bytes.is_multiple_of(page)
        {
            return Err(error("platform memory system_heap_bytes must be nonzero, page aligned \
                              physical heap capacity; it is not part of the image arena"));
        }
        let system_recovery_bytes = address("system_recovery_bytes", &self.system_recovery_bytes)?;
        if system_recovery_bytes == 0
           || !system_recovery_bytes.is_multiple_of(page)
           || system_recovery_bytes >= system_heap_bytes
        {
            return Err(error("platform memory system_recovery_bytes must be nonzero, page \
                              aligned and smaller than system_heap_bytes; it is protected \
                              storage inside that same heap, not additional physical capacity"));
        }
        let stacks = self.stacks.check(page)?;
        if framebuffer_reservation.checked_add(stacks.system_default())
                                  .is_none_or(|bytes| bytes > system_recovery_bytes)
        {
            return Err(error("platform memory system_recovery_bytes cannot hold the terminal \
                              framebuffer and independent System stack"));
        }
        let application_heap = self.application_heap.check(page)?;
        let tls = self.tls.check()?;
        let process_accounts = self.process_accounts.check(page, stacks.application_default())?;
        let module_accounts = self.module_accounts.check(page)?;
        Ok(Checked { page_bytes: page,
                     ram,
                     flash,
                     fw_cfg,
                     bootstrap_io,
                     boot,
                     firmware_heap,
                     images,
                     display_scratch,
                     display_width,
                     display_height,
                     display_max_width,
                     display_max_height,
                     application_relocation_base,
                     application_primary_base,
                     definitions,
                     fdt_tail_bytes: tail,
                     boot_stack_bytes,
                     monitor_stack_bytes,
                     system_heap_bytes,
                     system_recovery_bytes,
                     stacks,
                     application_heap,
                     tls,
                     process_accounts,
                     module_accounts })
    }
}
impl Checked
{
    /// The RAM window this platform maps, for consumers that describe the
    /// machine to something else rather than place a payload in it.
    pub fn ram_base(&self) -> u64
    {
        self.ram.start
    }

    pub fn minimum_ram_bytes(&self) -> u64
    {
        self.ram.end - self.ram.start
    }

    pub fn system_image_storage(&self) -> Range<u64>
    {
        self.images.start..self.application_primary_base
    }

    pub fn system_stack_default(&self) -> u64
    {
        self.stacks.system_default()
    }

    pub(super) fn system_stack_maximum(&self) -> u64
    {
        self.stacks.system_maximum()
    }

    pub fn application_stack_default(&self) -> u64
    {
        self.stacks.application_default()
    }

    pub fn display_dimensions(&self) -> (usize, usize)
    {
        (self.display_width as usize, self.display_height as usize)
    }

    pub fn display_constants(&self) -> String
    {
        format!("// Generated from the checked platform display record.\npub(crate) const \
                 DEFAULT_WIDTH: usize = {};\npub(crate) const DEFAULT_HEIGHT: usize = \
                 {};\npub(crate) const MAX_WIDTH: usize = {};\npub(crate) const MAX_HEIGHT: usize \
                 = {};\n",
                self.display_dimensions().0,
                self.display_dimensions().1,
                self.display_max_width,
                self.display_max_height)
    }

    pub(super) fn permits_mmio(&self, range: core::ops::Range<u64>) -> bool
    {
        !overlaps(&range, &self.ram)
        && self.flash.as_ref().is_none_or(|flash| !overlaps(&range, flash))
    }

    /// The platform's checked hardware mapping granularity.
    pub fn page_bytes(&self) -> u64
    {
        self.page_bytes
    }

    /// Uniform build origin/capacity for independently placed native code.
    /// This names link geometry only; actual runtime storage is admitted by
    /// the System module/definition allocators.
    pub fn code_link_range(&self) -> Range<u64>
    {
        self.definitions.clone()
    }

    /// The read-only boot payload medium, when this platform has one. A board
    /// whose firmware loads the payload from removable media has none.
    pub fn flash(&self) -> Option<Range<u64>>
    {
        self.flash.clone()
    }

    pub(super) fn linker_symbols(&self) -> String
    {
        let mut out = String::from("\n/* Generated from the checked platform memory record. */\n");
        let mut regions: Vec<(&str, &Range<u64>)> = Vec::new();
        if let Some(flash) = &self.flash
        {
            regions.push(("FLASH", flash));
        }
        if let Some(fw_cfg) = &self.fw_cfg
        {
            regions.push(("FW_CFG", fw_cfg));
        }
        regions.extend([("BOOT", &self.boot),
                        ("FIRMWARE_HEAP", &self.firmware_heap),
                        ("IMAGES", &self.images),
                        ("DISPLAY_SCRATCH", &self.display_scratch),
                        ("DEFINITIONS", &self.definitions)]);
        for (name, region) in regions
        {
            writeln!(out, "__platform_{name}_START = {:#x};", region.start).unwrap();
            writeln!(out, "__platform_{name}_END = {:#x};", region.end).unwrap();
        }
        let mut values = vec![("PAGE_BYTES", self.page_bytes),
                              ("RAM_BASE", self.ram.start),
                              ("MINIMUM_RAM_BYTES", self.ram.end - self.ram.start),
                              ("APPLICATION_RELOCATION_BASE", self.application_relocation_base),
                              ("APPLICATION_PRIMARY_BASE", self.application_primary_base),
                              ("FDT_TAIL_BYTES", self.fdt_tail_bytes),
                              ("BOOT_STACK_BYTES", self.boot_stack_bytes),
                              ("MONITOR_STACK_BYTES", self.monitor_stack_bytes),
                              ("MONITOR_CONTROL_BYTES",
                               mac_abi::boot::monitor::CONTROL_BYTES as u64),
                              ("SYSTEM_HEAP_BYTES", self.system_heap_bytes),
                              ("SYSTEM_RECOVERY_BYTES", self.system_recovery_bytes)];
        values.extend(self.display_buffers());
        values.extend(self.stacks.constants());
        if let Some(bootstrap_io) = &self.bootstrap_io
        {
            values.extend(bootstrap_io.constants());
        }
        else
        {
            // A board with no bootstrap IO declares no virtio windows at all.
            // Emitting the names with zero values keeps the device inventory
            // code free of an architecture or board test: it simply finds no
            // virtio slots to probe, which is the truth for such a board.
            values.extend([("VIRTIO_MMIO_SLOTS", 0),
                           ("VIRTIO_MMIO_BASE", 0),
                           ("VIRTIO_MMIO_STRIDE", 0),
                           ("VIRTIO_INTERRUPT_BASE", 0),
                           ("UART_BASE", 0),
                           ("RTC_BASE", 0),
                           ("CLINT_BASE", 0),
                           ("CLINT_BYTES", 0)]);
        }
        for (name, value) in values
        {
            writeln!(out, "__platform_{name} = {value:#x};").unwrap();
        }
        out
    }

    pub(super) fn system_constants(&self) -> String
    {
        let mut out = String::from("// Generated from the checked platform memory record.\n");
        let mut values = vec![("PAGE_BYTES", self.page_bytes)];
        // A board without flash or fw_cfg still has to compile against the same
        // shared memory code. Zero means "no such window", so a consumer that
        // iterates or excludes these regions finds nothing to act on, which is
        // the truth for such a board, without a board test in shared code.
        match &self.flash
        {
            Some(flash) =>
            {
                values.push(("FLASH_BASE", flash.start));
                values.push(("FLASH_BYTES", flash.end - flash.start));
            }
            None =>
            {
                values.push(("FLASH_BASE", 0));
                values.push(("FLASH_BYTES", 0));
            }
        }
        values.push(("FW_CFG_BASE", self.fw_cfg.as_ref().map(|fw| fw.start).unwrap_or(0)));
        values.extend([("IMAGE_BASE", self.images.start),
                       ("DISPLAY_SCRATCH_BASE", self.display_scratch.start),
                       ("DISPLAY_SCRATCH_BYTES",
                        self.display_scratch.end - self.display_scratch.start),
                       ("FIRMWARE_HEAP_BASE", self.firmware_heap.start),
                       ("FIRMWARE_HEAP_BYTES", self.firmware_heap.end - self.firmware_heap.start),
                       ("MONITOR_STACK_BYTES", self.monitor_stack_bytes),
                       ("MONITOR_CONTROL_BYTES", mac_abi::boot::monitor::CONTROL_BYTES as u64),
                       ("APPLICATION_PRIMARY_BASE", self.application_primary_base),
                       ("DEFINITION_BASE", self.definitions.start),
                       ("DEFINITION_LIMIT", self.definitions.end),
                       ("IMAGE_LIMIT", self.images.end)]);
        values.extend(self.display_buffers());
        values.extend(self.stacks.constants());
        values.extend(self.application_heap.constants());
        values.extend(self.tls.constants());
        values.extend(self.process_accounts.constants());
        values.extend(self.module_accounts.constants());
        if let Some(bootstrap_io) = &self.bootstrap_io
        {
            values.extend(bootstrap_io.constants());
        }
        for (name, value) in values
        {
            writeln!(out, "pub const {name}: u64 = {value:#x};").unwrap();
        }
        out.push_str(&self.reservation_constant());
        if let Some(bootstrap_io) = &self.bootstrap_io
        {
            out.push_str(&bootstrap_io.ranges_source(self.fw_cfg.as_ref()));
        }
        else
        {
            // No bootstrap IO means no MMIO windows to exclude. The empty array
            // is what lets the shared memory checks iterate over nothing rather
            // than needing to know which board they are on.
            out.push_str("pub const BOOT_MMIO_REGIONS: &[(u64,u64)] = &[];\n");
        }
        writeln!(out, "pub const SYSTEM_HEAP_BYTES: usize = {:#x};",self.system_heap_bytes).unwrap();
        writeln!(out,
                 "pub const SYSTEM_RECOVERY_BYTES: usize = {:#x};",
                 self.system_recovery_bytes).unwrap();
        out
    }

    // Three page-aligned frames remain reserved by the checked platform record.
    // The retired middle scratch span is still owned, not reclaimed or reused.
    // Generate only live views; preserve the original backbuffer's +2 offset.
    fn display_buffers(&self) -> [(&'static str, u64); 2]
    {
        let stride =
            (self.display_max_width * self.display_max_height * 4).div_ceil(self.page_bytes)
            * self.page_bytes;
        [("DISPLAY_FRONTBUFFER_BASE", self.display_scratch.start),
         ("DISPLAY_BACKBUFFER_BASE", self.display_scratch.start + 2 * stride)]
    }

    fn reservation_constant(&self) -> String
    {
        let mut out = String::new();
        writeln!(out,
                 "pub const BOOT_RESERVED: [(u64,u64);3] = \
                  [({:#x},{:#x}),({:#x},{:#x}),({:#x},{:#x})];",
                 self.boot.start,
                 self.boot.end,
                 self.firmware_heap.start,
                 self.firmware_heap.end,
                 self.display_scratch.start,
                 self.display_scratch.end).unwrap();
        out
    }

    /// Shared physical admission inputs for firmware and native boot probes.
    /// Topology is still discovered at reset; these are platform reservations.
    pub fn boot_admission_constants(&self) -> String
    {
        let mut out = String::from("// Generated checked physical admission policy.\n");
        for (name, value) in [("RAM_BASE", self.ram.start),
                              ("IMAGE_BASE", self.images.start),
                              ("IMAGE_LIMIT", self.images.end),
                              ("PAGE_BYTES", self.page_bytes),
                              ("FDT_TAIL_BYTES", self.fdt_tail_bytes)]
        {
            writeln!(out, "pub const {name}: u64 = {value:#x};").unwrap();
        }
        out.push_str(&self.reservation_constant());
        out
    }

    pub(super) fn boot_constants(&self) -> String
    {
        let mut out = self.boot_admission_constants();
        let mut values = vec![("MINIMUM_RAM_BYTES", self.ram.end - self.ram.start)];
        if let Some(flash) = &self.flash
        {
            values.push(("FLASH_BASE", flash.start));
            values.push(("FLASH_BYTES", flash.end - flash.start));
        }
        if let Some(fw_cfg) = &self.fw_cfg
        {
            values.push(("FW_CFG_BASE", fw_cfg.start));
        }
        values.extend([("FIRMWARE_HEAP_BASE", self.firmware_heap.start),
                       ("FIRMWARE_HEAP_BYTES", self.firmware_heap.end - self.firmware_heap.start),
                       ("MONITOR_STACK_BYTES", self.monitor_stack_bytes),
                       ("MONITOR_CONTROL_BYTES", mac_abi::boot::monitor::CONTROL_BYTES as u64),
                       ("DISPLAY_WIDTH", self.display_width),
                       ("DISPLAY_HEIGHT", self.display_height),
                       ("DISPLAY_SCRATCH_BASE", self.display_scratch.start),
                       ("DISPLAY_SCRATCH_BYTES",
                        self.display_scratch.end - self.display_scratch.start),
                       ("APPLICATION_PRIMARY_BASE", self.application_primary_base),
                       ("APPLICATION_PRIMARY_LIMIT", self.definitions.start),
                       ("APPLICATION_RELOCATION_BASE", self.application_relocation_base)]);
        if let Some(bootstrap_io) = &self.bootstrap_io
        {
            values.extend(bootstrap_io.constants());
        }
        for (name, value) in values
        {
            writeln!(out, "pub const {name}: u64 = {value:#x};").unwrap();
        }
        if let Some(bootstrap_io) = &self.bootstrap_io
        {
            out.push_str(&bootstrap_io.ranges_source(self.fw_cfg.as_ref()));
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
        let json: serde_json::Value = super::super::record("qemu_riscv").unwrap();
        serde_json::from_value(json["memory"].clone()).unwrap()
    }
    #[test]
    fn rejects_overlaps_alignment_overflow_and_outside_ram_before_emission()
    {
        let original = definition();
        original.check().unwrap();
        let mut bad = original.clone();
        bad.fw_cfg.as_mut().unwrap().base = original.ram_base.clone();
        assert!(bad.check()
                   .unwrap_err()
                   .to_string()
                   .contains("fw_cfg must be a physical MMIO window outside RAM and flash"));
        let mut bad = original.clone();
        bad.fw_cfg.as_mut().unwrap().base = original.flash.as_ref().unwrap().base.clone();
        assert!(bad.check().unwrap_err().to_string().contains("outside RAM and flash"));
        let mut bad = original.clone();
        bad.fw_cfg.as_mut().unwrap().base = "0x10100001".into();
        assert!(bad.check()
                   .unwrap_err()
                   .to_string()
                   .contains("fw_cfg must be nonempty and page aligned"));
        let mut bad = original.clone();
        bad.fw_cfg.as_mut().unwrap().base = "0x100000000000000".into();
        assert!(bad.check().unwrap_err().to_string().contains("physical MMIO window"));
        let mut bad = original.clone();
        bad.display_scratch.base = original.firmware_heap.base.clone();
        assert!(bad.check()
                   .unwrap_err()
                   .to_string()
                   .contains("display_scratch overlaps firmware_heap"));
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
                   .contains("display_scratch must cover the 1920x1200 framebuffer"));
        let mut bad = original.clone();
        bad.application_relocation_base = "0x87000000".into();
        assert!(bad.check()
                   .unwrap_err()
                   .to_string()
                   .contains("application_relocation_base must be page aligned inside images"));
        let mut bad = original.clone();
        bad.fdt_tail_bytes = "0x9000000".into();
        assert!(bad.check().unwrap_err().to_string().contains("FDT tail overlaps firmware_heap"));
        let mut bad = original.clone();
        bad.boot_stack_bytes = original.boot.bytes.clone();
        assert!(bad.check().unwrap_err().to_string().contains("smaller than boot RAM"));
        let mut bad = original.clone();
        bad.system_heap_bytes = "0x0".into();
        assert!(bad.check().unwrap_err().to_string().contains("nonzero, page aligned"));
        let mut bad = original.clone();
        bad.system_heap_bytes = "0x20000001".into();
        assert!(bad.check().unwrap_err().to_string().contains("nonzero, page aligned"));
        let mut page_backed = original.clone();
        page_backed.system_heap_bytes = "0x20000000".into();
        assert_eq!(page_backed.check().unwrap().system_heap_bytes,
                   512 << 20,
                   "the heap has its own physical lease, not image-arena storage");
        let mut bad = original.clone();
        bad.page_bytes = 3;
        assert!(bad.check().unwrap_err().to_string().contains("page_bytes must be a power of two"));
    }
    #[test]
    fn constants_linker_and_rom_packaging_use_the_same_checked_ranges()
    {
        let mut definition = definition();
        definition.system_heap_bytes = "0x600000".into();
        let checked = definition.check().unwrap();
        let linker = checked.linker_symbols();
        let rust = checked.system_constants();
        assert!(linker.contains("__platform_FW_CFG_START = 0x10100000;"));
        assert!(linker.contains("__platform_FW_CFG_END = 0x10101000;"));
        assert!(rust.contains("FW_CFG_BASE: u64 = 0x10100000;"));
        assert!(checked.boot_constants().contains("FW_CFG_BASE: u64 = 0x10100000;"));
        assert!(!linker.contains("__platform_MODULES"));
        for (name, value) in checked.stacks.constants()
        {
            assert!(linker.contains(&format!("__platform_{name} = {value:#x};")));
            assert!(rust.contains(&format!("{name}: u64 = {value:#x};")));
        }
        assert!(!rust.contains("MODULE_LIMIT:"));
        assert!(linker.contains("__platform_SYSTEM_HEAP_BYTES = 0x600000;"));
        assert!(rust.contains("SYSTEM_HEAP_BYTES: usize = 0x600000;"));
        assert!(checked.boot_constants().contains("IMAGE_LIMIT: u64 = 0x86a81000;"));
        assert!(linker.contains("__platform_DISPLAY_SCRATCH_START = 0x86a81000;"));
        assert!(rust.contains("DISPLAY_SCRATCH_BASE: u64 = 0x86a81000;"));
        assert!(rust.contains("DISPLAY_SCRATCH_BYTES: u64 = 0x57f000;"));
        assert!(rust.contains("DISPLAY_FRONTBUFFER_BASE: u64 = 0x86a81000;"));
        assert!(rust.contains("DISPLAY_TRACKING_BASE: u64 = 0x888ca000;"));
        assert!(rust.contains("DISPLAY_BACKBUFFER_BASE: u64 = 0x86e2b000;"));
        assert!(linker.contains("__platform_DISPLAY_BACKBUFFER_BASE = 0x86e2b000;"));
        assert!(checked.display_constants().contains("DEFAULT_WIDTH: usize = 800;"));
        assert!(checked.display_constants().contains("DEFAULT_HEIGHT: usize = 600;"));
        assert!(checked.boot_constants().contains("(0x86a81000,0x87000000)"));
        let flash = checked.flash().expect("the RISC-V record declares flash");
        assert_eq!(flash.start, 0x20000000);
        assert_eq!(flash.end - flash.start, 0x2000000);
    }
    #[test]
    fn physical_image_definition_regions_cannot_overlap()
    {
        let mut bad = definition();
        bad.definitions.base = "0x81000000".into();
        assert!(bad.check()
                   .unwrap_err()
                   .to_string()
                   .contains("application/definition regions overlap"));
    }
}

//! Build-defined bootstrap IO windows. Register offsets remain device protocols;
//! board placement, window counts and interrupt routes come from the platform.
use std::{fmt::Write, ops::Range};

use serde::Deserialize;

use super::{Region, address, error, overlaps, region};
use crate::Result;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Virtio
{
    base:           String,
    stride:         String,
    slots:          u32,
    interrupt_base: u32
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Definition
{
    uart:        Region,
    rtc:         Region,
    clint:       Region,
    virtio_mmio: Virtio
}
#[derive(Debug)]
pub(super) struct Checked
{
    uart:           Range<u64>,
    rtc:            Range<u64>,
    clint:          Range<u64>,
    virtio:         Range<u64>,
    stride:         u64,
    slots:          u32,
    interrupt_base: u32
}
impl Definition
{
    pub(super) fn check(&self, page: u64, ram: &Range<u64>, flash: &Range<u64>,
                        fw_cfg: &Range<u64>)
                        -> Result<Checked>
    {
        let uart = region("bootstrap_io.uart", &self.uart, page)?;
        let rtc = region("bootstrap_io.rtc", &self.rtc, page)?;
        let clint = region("bootstrap_io.clint", &self.clint, page)?;
        if clint.end - clint.start != 0x10000
        {
            return Err(error("selected legacy CLINT window must cover exactly its 64 KiB \
                              binding"));
        }
        let stride = address("bootstrap_io.virtio_mmio.stride", &self.virtio_mmio.stride)?;
        let slots = self.virtio_mmio.slots;
        let bytes = stride.checked_mul(u64::from(slots))
                          .ok_or_else(|| error("platform bootstrap virtio MMIO extent overflows"))?;
        let virtio = region("bootstrap_io.virtio_mmio",
                            &Region { base:  self.virtio_mmio.base.clone(),
                                      bytes: format!("{bytes:#x}") },
                            page)?;
        if !stride.is_power_of_two()
           || stride < 512
           || slots.checked_sub(1)
                   .and_then(|last| self.virtio_mmio.interrupt_base.checked_add(last))
                   .is_none()
        {
            return Err(error("platform bootstrap virtio MMIO stride must be a power of two \
                              covering the register window and interrupt routes must not \
                              overflow"));
        }
        let windows = [fw_cfg, &uart, &rtc, &virtio, &clint];
        for (index, range) in windows.iter().enumerate()
        {
            if range.end > 1 << 56 || overlaps(range, ram) || overlaps(range, flash)
            {
                return Err(error(format!("platform bootstrap MMIO {:#x}..{:#x} must fit \
                                          physical addresses outside RAM/flash",
                                         range.start, range.end)));
            }
            for previous in &windows[..index]
            {
                if overlaps(range, previous)
                {
                    return Err(error("platform bootstrap MMIO windows overlap"));
                }
            }
        }
        Ok(Checked { uart,
                     rtc,
                     clint,
                     virtio,
                     stride,
                     slots,
                     interrupt_base: self.virtio_mmio.interrupt_base })
    }
}
impl Checked
{
    pub(super) fn constants(&self) -> impl Iterator<Item = (&'static str, u64)>
    {
        [("UART_BASE", self.uart.start),
         ("RTC_BASE", self.rtc.start),
         ("CLINT_BASE", self.clint.start),
         ("CLINT_BYTES", self.clint.end - self.clint.start),
         ("VIRTIO_MMIO_BASE", self.virtio.start),
         ("VIRTIO_MMIO_STRIDE", self.stride),
         ("VIRTIO_MMIO_SLOTS", u64::from(self.slots)),
         ("VIRTIO_INTERRUPT_BASE", u64::from(self.interrupt_base))].into_iter()
    }

    pub(super) fn ranges_source(&self, fw_cfg: &Range<u64>) -> String
    {
        let mut output = String::from("pub const BOOT_MMIO_REGIONS: &[(u64,u64)] = &[\n");
        for range in [fw_cfg, &self.uart, &self.rtc, &self.virtio, &self.clint]
        {
            writeln!(output, "({:#x},{:#x}),", range.start, range.end).unwrap();
        }
        output.push_str("];\n");
        output
    }
}
#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn checked_bootstrap_windows_generate_one_set_of_facts_and_refuse_bad_maps()
    {
        let json: serde_json::Value =
            serde_json::from_str(include_str!("../../../../../platforms/QemuRISC-V.json")).unwrap();
        let original: Definition =
            serde_json::from_value(json["memory"]["bootstrap_io"].clone()).unwrap();
        let check = |d: &Definition| {
            d.check(4096,
                    &(0x80000000..0x88000000),
                    &(0x20000000..0x22000000),
                    &(0x10100000..0x10101000))
        };
        let checked = check(&original).unwrap();
        let constants: Vec<_> = checked.constants().collect();
        assert!(constants.contains(&("UART_BASE", 0x10000000)));
        assert!(constants.contains(&("RTC_BASE", 0x101000)));
        assert!(constants.contains(&("VIRTIO_MMIO_BASE", 0x10001000)));
        assert!(constants.contains(&("VIRTIO_MMIO_SLOTS", 8)));
        assert!(checked.ranges_source(&(0x10100000..0x10101000))
                       .contains("(0x10001000,0x10009000)"));
        let mut bad = original.clone();
        bad.uart.base = "0x80000000".into();
        assert!(check(&bad).unwrap_err().to_string().contains("outside RAM/flash"));
        let mut bad = original.clone();
        bad.rtc = bad.uart.clone();
        assert!(check(&bad).unwrap_err().to_string().contains("overlap"));
        let mut bad = original.clone();
        bad.virtio_mmio.base = "0x10001001".into();
        assert!(check(&bad).unwrap_err().to_string().contains("page aligned"));
        let mut bad = original.clone();
        bad.virtio_mmio.slots = 0;
        assert!(check(&bad).unwrap_err().to_string().contains("nonempty"));
        let mut bad = original.clone();
        bad.virtio_mmio.interrupt_base = u32::MAX;
        assert!(check(&bad).unwrap_err().to_string().contains("interrupt routes"));
    }
}

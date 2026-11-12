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
pub(super) struct Uart
{
    base:  String,
    bytes: String,
    /// The register protocol this window speaks.
    ///
    /// A bootstrap console is written to before any device tree can be read, so
    /// this cannot come from the machine. It is stated rather than inferred: an
    /// architecture does not decide it, because a board of any architecture can
    /// carry either part, and polling the wrong status bit produces silence
    /// rather than an error.
    kind:  ConsoleKind
}
#[derive(Clone, Copy, Deserialize, PartialEq, Eq, Debug)]
pub(super) enum ConsoleKind
{
    /// A 16550-compatible UART: line status at offset 5, transmitter empty at 0x20.
    #[serde(rename = "16550")]
    Uart16550,
    /// An ARM PrimeCell PL011: flag register at 0x18, transmit FIFO not full at 0x20.
    #[serde(rename = "pl011")]
    Pl011
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Definition
{
    uart:        Uart,
    /// Absent on a machine that has no such controller, which is encoded as a
    /// zero window rather than as a separate schema for that machine.
    #[serde(default)]
    rtc:         Option<Region>,
    #[serde(default)]
    clint:       Option<Region>,
    /// The extent the machine's interrupt controller occupies, when the resident
    /// path has to program one before any interrupt can be delivered.
    ///
    /// The extent, not the register blocks: the machine's own tree states where
    /// the distributor and the CPU interface sit inside it, and the kernel
    /// checks that statement against this declaration rather than deriving one
    /// address from the other. This window exists so the kernel root maps it at
    /// all - an address the kernel cannot reach is a controller it cannot use,
    /// whatever the tree says.
    #[serde(default)]
    interrupt_controller: Option<Region>,
    /// Absent on a board with no virtio transport bank. Such a board still
    /// needs its console declared - System's own diagnostics are written to it
    /// before any device tree is read - so the bank cannot be what makes this
    /// whole section mandatory. Absent is emitted as a zero window, like every
    /// other part a machine does not have.
    #[serde(default)]
    virtio_mmio: Option<Virtio>
}
#[derive(Debug)]
pub(super) struct Checked
{
    uart:                 Range<u64>,
    rtc:                  Option<Range<u64>>,
    clint:                Option<Range<u64>>,
    interrupt_controller: Option<Range<u64>>,
    console:              ConsoleKind,
    virtio:               Option<Range<u64>>,
    stride:               u64,
    slots:                u32,
    interrupt_base:       u32
}
impl Definition
{
    pub(super) fn check(&self, page: u64, ram: &Range<u64>, flash: Option<&Range<u64>>,
                        fw_cfg: Option<&Range<u64>>)
                        -> Result<Checked>
    {
        let uart = region("bootstrap_io.uart",
                          &Region { base:  self.uart.base.clone(),
                                    bytes: self.uart.bytes.clone() },
                          page)?;
        let rtc = self.rtc.as_ref().map(|r| region("bootstrap_io.rtc", r, page)).transpose()?;
        let clint =
            self.clint.as_ref().map(|c| region("bootstrap_io.clint", c, page)).transpose()?;
        let interrupt_controller = self.interrupt_controller
                                       .as_ref()
                                       .map(|c| {
                                           region("bootstrap_io.interrupt_controller", c, page)
                                       })
                                       .transpose()?;
        if clint.as_ref().is_some_and(|clint| clint.end - clint.start != 0x10000)
        {
            return Err(error("selected legacy CLINT window must cover exactly its 64 KiB \
                              binding"));
        }
        let (virtio, stride, slots, interrupt_base) = match &self.virtio_mmio
        {
            None => (None, 0, 0, 0),
            Some(bank) =>
            {
                let stride = address("bootstrap_io.virtio_mmio.stride", &bank.stride)?;
                let slots = bank.slots;
                let bytes =
                    stride.checked_mul(u64::from(slots))
                          .ok_or_else(|| error("platform bootstrap virtio MMIO extent overflows"))?;
                let virtio = region("bootstrap_io.virtio_mmio",
                                    &Region { base:  bank.base.clone(),
                                              bytes: format!("{bytes:#x}") },
                                    page)?;
                if !stride.is_power_of_two()
                   || stride < 512
                   || slots.checked_sub(1)
                           .and_then(|last| bank.interrupt_base.checked_add(last))
                           .is_none()
                {
                    return Err(error("platform bootstrap virtio MMIO stride must be a power of \
                                      two covering the register window and interrupt routes \
                                      must not overflow"));
                }
                (Some(virtio), stride, slots, bank.interrupt_base)
            }
        };
        let mut windows: Vec<&Range<u64>> = Vec::new();
        if let Some(fw_cfg) = fw_cfg
        {
            windows.push(fw_cfg);
        }
        windows.extend([Some(&uart), rtc.as_ref(), virtio.as_ref(), clint.as_ref(),
                        interrupt_controller.as_ref()]
                           .into_iter()
                           .flatten());
        for (index, range) in windows.iter().enumerate()
        {
            if range.end > 1 << 56
               || overlaps(range, ram)
               || flash.is_some_and(|flash| overlaps(range, flash))
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
                     interrupt_controller,
                     console: self.uart.kind,
                     virtio,
                     stride,
                     slots,
                     interrupt_base })
    }
}
impl Checked
{
    pub(super) fn constants(&self) -> impl Iterator<Item = (&'static str, u64)>
    {
        // A window this machine does not have is emitted as zero, which is what
        // makes shared code find nothing to act on rather than needing a test.
        [("UART_BASE", self.uart.start),
         ("RTC_BASE", self.rtc.as_ref().map_or(0, |rtc| rtc.start)),
         ("CLINT_BASE", self.clint.as_ref().map_or(0, |clint| clint.start)),
         ("CLINT_BYTES", self.clint.as_ref().map_or(0, |clint| clint.end - clint.start)),
         ("VIRTIO_MMIO_BASE", self.virtio.as_ref().map_or(0, |bank| bank.start)),
         ("VIRTIO_MMIO_STRIDE", self.stride),
         ("VIRTIO_MMIO_SLOTS", u64::from(self.slots)),
         ("VIRTIO_INTERRUPT_BASE", u64::from(self.interrupt_base)),
         ("INTERRUPT_CONTROLLER_BASE",
          self.interrupt_controller.as_ref().map_or(0, |c| c.start)),
         ("INTERRUPT_CONTROLLER_BYTES",
          self.interrupt_controller.as_ref().map_or(0, |c| c.end - c.start)),
         ("CONSOLE_KIND", match self.console
         {
             ConsoleKind::Uart16550 => super::CONSOLE_KIND_16550,
             ConsoleKind::Pl011 => super::CONSOLE_KIND_PL011
         })].into_iter()
    }

    pub(super) fn ranges_source(&self, fw_cfg: Option<&Range<u64>>) -> String
    {
        let mut output = String::from("pub const BOOT_MMIO_REGIONS: &[(u64,u64)] = &[\n");
        let mut windows: Vec<&Range<u64>> = Vec::new();
        if let Some(fw_cfg) = fw_cfg
        {
            windows.push(fw_cfg);
        }
        windows.extend([Some(&self.uart),
                        self.rtc.as_ref(),
                        self.virtio.as_ref(),
                        self.clint.as_ref(),
                        self.interrupt_controller.as_ref()]
                           .into_iter()
                           .flatten());
        for range in windows
        {
            writeln!(output, "({:#x},{:#x}),", range.start, range.end).unwrap();
        }
        output.push_str("];\n");
        output
    }
}

//! How a platform is built and run on a specific real board, as opposed to
//! under an emulator. Optional, independent of the build, and unrelated to
//! `emulation`: a platform may declare either, both or neither.
//!
//! This vocabulary is deliberately *not* the emulation vocabulary. Emulation
//! describes a virtual machine in terms of device functions a hypervisor can
//! synthesise. A board describes integrated peripherals by identity, with their
//! memory windows and interrupts, because that is what a SoC port has to know.
//! Sharing one set of kinds between the two would force one of them to lie.
//!
//! Everything except the board identity is optional so a record can be honest
//! about what has actually been qualified: an absent field states that the
//! platform has not specified that part, not that a default applies.
use serde::Deserialize;

use super::storage::quantity;
use crate::{Result, error};

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Definition
{
    /// What this board is, in one line a newcomer can read.
    pub description: String,
    pub board:       String,
    /// The SoC, when the board name alone does not identify it.
    #[serde(default)]
    pub soc:         Option<String>,
    pub boot:        Boot,
    /// The medium this board is given, by the name of a `storage` image. The
    /// layout itself lives at the platform level so the same described medium
    /// can also be attached to an emulator; a board does not describe its own
    /// partition table.
    #[serde(default)]
    pub boot_image:  Option<String>,
    #[serde(default)]
    pub console:     Option<Console>,
    /// Whether this workspace compiles the record's device information into a
    /// device-tree blob bound into the bootloader.
    ///
    /// Absence means false, and false does **not** mean the platform has no
    /// device tree: every platform hands the kernel one. It means no tree is
    /// generated here, so the bootloader supplies the platform's own native
    /// description at boot - the firmware's blob where there is one, or a
    /// translation of ACPI on a machine that has no device tree of its own.
    /// A platform that neither generates a tree nor receives one fails at boot
    /// with a named error rather than being handed an invented one.
    #[serde(default)]
    pub generate_device_tree: bool,
    /// The RAM window the SoC maps, as a board fact. This is not the platform
    /// level `memory` section: that describes the paging, linker and ROM ranges
    /// this workspace's kernel runs in, while this states where the board puts
    /// its DRAM. A SoC port reads it to place its payload; nothing in this
    /// workspace's builders consumes it yet.
    #[serde(default)]
    pub ram:         Option<Region>,
    /// Integrated peripherals, by identity. Not emulator device kinds.
    #[serde(default)]
    pub devices:     Vec<Peripheral>,
    /// Controllers this board carries that this platform deliberately does not
    /// use, with the reason. Several SoCs have two parts that could serve one
    /// function; recording the one that is not wired up stops a later reader
    /// from driving the wrong controller and debugging a silent absence.
    #[serde(default)]
    pub unused:      Vec<Unused>,
    /// Programs needed to write the artifact to the medium.
    #[serde(default)]
    pub tooling:     Vec<String>
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Boot
{
    pub medium:      Medium,
    /// What runs before this platform's payload. A board whose first stage is
    /// vendor firmware — a BCM2837-class board, for example — has no bootloader
    /// package of ours at all, and that is a normal configuration.
    pub first_stage: FirstStage
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Medium
{
    Sd
    {
        #[serde(default)]
        offset: Option<String>
    },
    Emmc
    {
        #[serde(default)]
        offset: Option<String>
    },
    SpiFlash
    {
        #[serde(default)]
        offset: Option<String>
    },
    UartLoad
    {
        #[serde(default)]
        offset: Option<String>
    },
    /// The medium is not described yet, or this platform is not programmed
    /// through one.
    None
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FirstStage
{
    /// The board's own firmware loads this platform's payload.
    VendorFirmware,
    /// This platform builds a bootloader package that runs first.
    PlatformBootloader,
    /// The payload is the first thing the processor executes.
    None
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Console
{
    /// The UART model, e.g. `pl011`.
    pub kind:   String,
    /// The device node a host sees it as, e.g. `ttyAMA0`.
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub baud:   Option<u32>
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Region
{
    pub base:  String,
    pub bytes: String
}

/// A controller the board carries but this platform does not use.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Unused
{
    /// The part's own name, e.g. `bcm2835-sdhost`.
    pub model:  String,
    pub base:   String,
    /// Why this platform drives a different controller instead.
    pub reason: String
}

/// An integrated peripheral identified the way a SoC datasheet identifies it.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Peripheral
{    pub kind:      PeripheralKind,
    /// The part's own name, e.g. `pl011`, `sdhci`, `genet`.
    pub model:     String,
    #[serde(default)]
    pub base:      Option<String>,
    #[serde(default)]
    pub bytes:     Option<String>,
    #[serde(default)]
    pub interrupt: Option<u32>
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PeripheralKind
{
    Uart,
    Storage,
    GpioMailbox,
    Network,
    Pcie,
    Timer,
    InterruptController,
    Display,
    Entropy
}

impl Definition
{
    pub fn check(&self, platform: &str) -> Result<()>
    {
        let reject = |field: &str, why: String| {
            error(format!("platform '{platform}' hardware.{field}: {why}"))
        };
        if self.description.trim().is_empty()
        {
            return Err(reject("description", "must describe the board".into()));
        }
        if self.board.trim().is_empty()
        {
            return Err(reject("board", "must name the board".into()));
        }
        if let Some(image) = &self.boot_image
        {
            if image.trim().is_empty()
            {
                return Err(reject("boot_image", "must name a storage image".into()));
            }
        }
        if self.boot.first_stage == FirstStage::PlatformBootloader && self.boot_image.is_none()
        {
            // A board this workspace boots itself needs to know which medium to
            // write; a board booted by vendor firmware may name none.
            return Err(reject("boot_image",
                              "must name the storage image this board is written with".into()));
        }
        // Vendor firmware that hands over its own tree needs no generated one,
        // so there is nothing to require here. A platform that generates one
        // declares it with `generate_device_tree`.
        for tool in &self.tooling
        {
            if tool.trim().is_empty()
            {
                return Err(reject("tooling", "entries must be nonempty".into()));
            }
        }
        if let Some(console) = &self.console
        {
            if console.kind.trim().is_empty()
            {
                return Err(reject("console.kind", "must name the UART model".into()));
            }
            if let Some(baud) = console.baud
            {
                // A UART's line rate is a real quantity; zero and a rate no
                // divider can produce are both defects, not settings.
                if !(300..=4_000_000).contains(&baud)
                {
                    return Err(reject("console.baud",
                                      format!("{baud} is not a usable line rate; give a rate \
                                               between 300 and 4000000")));
                }
            }
        }
        if let Some(ram) = &self.ram
        {
            region(&reject, "ram", ram)?;
        }
        for (index, device) in self.devices.iter().enumerate()
        {
            let field = format!("devices[{index}]");
            if device.model.trim().is_empty()
            {
                return Err(reject(&format!("{field}.model"),
                                  format!("the {:?} peripheral must name its model",
                                          device.kind)));
            }
            match (&device.base, &device.bytes)
            {
                (None, None) =>
                {}
                (Some(base), Some(bytes)) =>
                {
                    region(&reject,
                           &field,
                           &Region { base:  base.clone(),
                                     bytes: bytes.clone() })?;
                }
                (base, bytes) =>
                {
                    return Err(reject(&field,
                                      format!("declares base {:?} and bytes {:?}; a \
                                               peripheral's memory window is either stated in \
                                               full or not at all",
                                              base, bytes)));
                }
            }
        }
        Ok(())
    }

    pub fn description(&self) -> &str
    {
        &self.description
    }

    /// The storage images this board is written with, by name.
    pub fn images(&self) -> impl Iterator<Item = &str>
    {
        self.boot_image.iter().map(String::as_str)
    }

    /// Whether this platform builds a bootloader of its own for this board.
    pub fn needs_bootloader(&self) -> bool
    {
        self.boot.first_stage == FirstStage::PlatformBootloader
    }
}

/// Validate one memory window as a quantity: both ends must parse, and the
/// window must be nonempty, because a zero-byte region is a typo rather than a
/// device.
fn region(reject: &impl Fn(&str, String) -> Box<dyn std::error::Error>, field: &str,
          window: &Region)
          -> Result<()>
{
    let base =
        quantity(&window.base).map_err(|why| reject(&format!("{field}.base"), why.to_string()))?;
    let bytes =
        quantity(&window.bytes).map_err(|why| reject(&format!("{field}.bytes"), why.to_string()))?;
    if bytes == 0
    {
        return Err(reject(&format!("{field}.bytes"), "must be nonzero".into()));
    }
    base.checked_add(bytes).ok_or_else(|| {
                                reject(&format!("{field}"),
                                       format!("base {base:#x} plus {bytes:#x} bytes overflows \
                                                the address space"))
                            })?;
    Ok(())
}

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
    /// The artifact this board must be given, and the shape it must have.
    #[serde(default)]
    pub image:       Option<ImageLayout>,
    #[serde(default)]
    pub console:     Option<Console>,
    #[serde(default)]
    pub device_tree: Option<DeviceTree>,
    /// The RAM window the platform expects.
    #[serde(default)]
    pub memory:      Option<Region>,
    /// Integrated peripherals, by identity. Not emulator device kinds.
    #[serde(default)]
    pub devices:     Vec<Peripheral>,
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
    /// vendor firmware — a Raspberry Pi, for example — has no bootloader
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

/// The shape of the artifact. A raw payload at an offset, a populated
/// filesystem image and a container are not interchangeable, so each is its own
/// shape rather than one struct with optional fields.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ImageLayout
{
    /// A bootable disk image written whole to the medium.
    Disk
    {
        artifact: String
    },
    /// A raw payload placed at an absolute offset.
    Raw
    {
        artifact: String,
        #[serde(default)]
        offset:   Option<String>
    },
    /// A filesystem image populated with named files at named paths, the way a
    /// board that boots from a FAT partition expects.
    Filesystem
    {
        artifact:   String,
        filesystem: String,
        #[serde(default)]
        files:      Vec<BootFile>
    },
    /// A vendor container or archive.
    Container
    {
        artifact: String, format: String
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BootFile
{
    /// The build artifact to place.
    pub source: String,
    /// Where the board's firmware looks for it.
    pub path:   String
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

/// A device tree is either supplied by board firmware, or is a file this
/// platform must ship. Which one is a platform fact, not a build assumption.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum DeviceTree
{
    Firmware,
    File
    {
        path: String
    },
    None
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Region
{
    pub base:  String,
    pub bytes: String
}

/// An integrated peripheral identified the way a SoC datasheet identifies it.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Peripheral
{
    pub kind:      PeripheralKind,
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
    Display
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
        if let Some(image) = &self.image
        {
            let artifact = image.artifact();
            if artifact.trim().is_empty()
            {
                return Err(reject("image.artifact", "must name the artifact to produce".into()));
            }
            match image
            {
                ImageLayout::Raw { offset: None,
                                   .. } =>
                {
                    return Err(reject("image.offset",
                                      format!("raw artifact '{artifact}' must state its \
                                               offset on the medium")));
                }
                ImageLayout::Filesystem { filesystem,
                                          files,
                                          .. } =>
                {
                    if filesystem.trim().is_empty()
                    {
                        return Err(reject("image.filesystem", "must name a filesystem".into()));
                    }
                    for file in files
                    {
                        if file.source.trim().is_empty() || file.path.trim().is_empty()
                        {
                            return Err(reject("image.files",
                                              "each file needs a source and a path".into()));
                        }
                    }
                }
                ImageLayout::Container { format,
                                         .. } if format.trim().is_empty() =>
                {
                    return Err(reject("image.format", "must name its container format".into()));
                }
                _ =>
                {}
            }
        }
        if self.boot.first_stage == FirstStage::VendorFirmware && self.device_tree.is_none()
        {
            // Vendor firmware normally hands over a device tree; a board that
            // says neither is a record the author has not finished.
            return Err(reject("device_tree",
                              "a board booted by vendor firmware must say where its device \
                               tree comes from"
                                               .into()));
        }
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
            if console.baud == Some(0)
            {
                return Err(reject("console.baud", "must be nonzero".into()));
            }
        }
        for device in &self.devices
        {
            if device.model.trim().is_empty()
            {
                return Err(reject("devices.model",
                                  format!("the {:?} peripheral must name its model",
                                          device.kind)));
            }
        }
        Ok(())
    }

    pub fn description(&self) -> &str
    {
        &self.description
    }

    pub fn board(&self) -> &str
    {
        &self.board
    }

    /// The artifact this board must be given, if the record states its shape.
    pub fn artifact(&self) -> Option<&str>
    {
        self.image.as_ref().map(ImageLayout::artifact)
    }

    /// Whether this platform builds a bootloader of its own for this board.
    pub fn needs_bootloader(&self) -> bool
    {
        self.boot.first_stage == FirstStage::PlatformBootloader
    }

    pub fn tooling(&self) -> &[String]
    {
        &self.tooling
    }
}

impl ImageLayout
{
    pub fn artifact(&self) -> &str
    {
        match self
        {
            Self::Disk { artifact }
            | Self::Raw { artifact,
                        .. }
            | Self::Filesystem { artifact,
                               .. }
            | Self::Container { artifact,
                              .. } => artifact
        }
    }
}

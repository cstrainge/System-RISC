//! How a platform is emulated, described as a machine rather than a command
//! line.
//!
//! A platform record states the board model, the processors, the memory, and
//! the devices by *function*. It never names an emulator flag: the translation
//! from this description to a concrete invocation lives in `tools/xtask`, once
//! per emulator family, so adding a device or changing the RAM size is an edit
//! to data and not to code.
//!
//! One carve-out: where the guest itself depends on an address — the virtio-MMIO
//! window and the per-function bus slot — the record states it explicitly. Those
//! numbers are part of the machine's contract with the guest, not an emulator
//! detail, so they are data the author must be able to see and check.

use std::num::{NonZeroU64, NonZeroUsize};

use serde::Deserialize;

use crate::{Result, error};

/// The emulator families this build knows how to drive. A record naming any
/// other family is rejected when it loads, naming the field and the value.
pub const FAMILIES: &[&str] = &["qemu"];

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Family
{
    Qemu
}

impl Family
{
    pub fn name(self) -> &'static str
    {
        match self
        {
            Self::Qemu => "qemu"
        }
    }
}

/// Where a device sits on the machine. `virtio-mmio` carries the bus slot
/// because the guest discovers MMIO devices by address, which makes the slot a
/// contract rather than a launcher preference.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Transport
{
    VirtioMmio
    {
        slot: u32
    },
    VirtioPci
}

impl Transport
{
    pub fn slot(&self) -> Option<u32>
    {
        match self
        {
            Self::VirtioMmio { slot } => Some(*slot),
            Self::VirtioPci => None
        }
    }
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DisplayKind
{
    VirtioGpu,
    /// A framebuffer configured through the SoC's VideoCore mailbox property
    /// channel. It is part of the machine, so it has no attachable bus.
    #[serde(rename = "videocore-mailbox")]
    VideoCoreMailbox
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Framebuffer
{
    #[default]
    None,
    Ramfb,
    /// The framebuffer the mailbox property channel allocates from the SoC's
    /// reserved video memory.
    #[serde(rename = "videocore-mailbox")]
    VideoCoreMailbox
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Display
{
    pub kind:        DisplayKind,
    #[serde(default)]
    pub framebuffer: Framebuffer,
    /// The bus the display device sits on. A device integrated into the SoC has
    /// no attachable bus and omits this.
    #[serde(default)]
    pub transport:   Option<Transport>
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum KeyboardKind
{
    VirtioKeyboard,
    UsbKeyboard
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Keyboard
{
    pub kind:      KeyboardKind,
    pub transport: Transport
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PointerKind
{
    Tablet,
    Mouse
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Pointer
{
    pub kind:      PointerKind,
    pub transport: Transport
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum EntropyKind
{
    VirtioRng,
    /// The SoC's own random number generator. It is part of the machine, so it
    /// has no attachable bus.
    Bcm2835Rng
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum EntropyBackend
{
    /// The emulator's own generator.
    #[default]
    Builtin
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Entropy
{
    pub kind:      EntropyKind,
    /// Where the bytes come from. A machine may care — a guest that must see
    /// the host's entropy is a different machine from one that will accept the
    /// emulator's — so it is stated rather than assumed.
    #[serde(default)]
    pub backend:   EntropyBackend,
    /// The bus the generator sits on. A device integrated into the SoC has no
    /// attachable bus and omits this.
    #[serde(default)]
    pub transport: Option<Transport>
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkKind
{
    #[default]
    None,
    User
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct Network
{
    #[serde(default)]
    pub kind: NetworkKind
}

/// How the boot image reaches the machine. A flash attachment looks like a
/// firmware ROM the processor fetches from; a direct attachment is a bootable
/// image the emulator loads itself.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum BootMedium
{
    Flash
    {
        #[serde(default)]
        unit:     u8,
        #[serde(default = "raw")]
        format:   String,
        #[serde(default = "readonly")]
        readonly: bool
    },
    /// The machine's own firmware reads the boot payload from an SD medium on
    /// the SoC's SD host controller. The medium is attached by `disks`, so this
    /// variant carries no attachment details of its own.
    Sd,
    Direct
}

fn raw() -> String
{
    "raw".into()
}

fn readonly() -> bool
{
    true
}

/// The bus a disk attaches to. `virtio-mmio` requires explicit slots because
/// each slot is a fixed guest-visible address; a bus that assigns its own
/// addresses needs only a capacity.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DiskTransport
{
    VirtioMmio,
    VirtioPci,
    /// The SoC's SD host controller. It owns the medium the machine's firmware
    /// boots from and assigns its own card slot, so attachments name no slot.
    Sdhci
}

/// Attachable block devices, one variant per transport, because the two
/// transports count disks differently and one number must not stand for both.
///
/// `virtio-mmio` exposes a fixed set of bus slots, each at a guest-visible
/// address, so a machine that uses it lists every slot it offers and the list
/// is part of the guest contract. `virtio-pci` assigns its own addresses, so a
/// machine that uses it states only how many disks it can hold.
///
/// `attach` names platform media, by storage image name, at the slots the guest
/// expects them. Exactly one attachment is marked `boot` on a machine that
/// boots from a disk; the boot medium is named, never inferred from position or
/// from the order of `storage.images`.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "transport", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Disks
{
    VirtioMmio
    {
        #[serde(default = "raw")]
        format:   String,
        #[serde(default = "readonly")]
        readonly: bool,
        /// Every bus slot this machine exposes to disks.
        slots:    Vec<u32>,
        #[serde(default)]
        attach:   Vec<Attachment>
    },
    VirtioPci
    {
        #[serde(default = "raw")]
        format:   String,
        #[serde(default = "readonly")]
        readonly: bool,
        #[serde(default = "four")]
        capacity: NonZeroUsize,
        #[serde(default)]
        attach:   Vec<Attachment>
    },
    /// The SoC's SD host controller. It exposes one card slot, which the
    /// machine's firmware uses to read the boot medium.
    Sdhci
    {
        #[serde(default = "raw")]
        format:   String,
        #[serde(default = "readonly")]
        readonly: bool,
        #[serde(default)]
        attach:   Vec<Attachment>
    }
}

impl Disks
{
    pub fn transport(&self) -> DiskTransport
    {
        match self
        {
            Self::VirtioMmio { .. } => DiskTransport::VirtioMmio,
            Self::VirtioPci { .. } => DiskTransport::VirtioPci,
            Self::Sdhci { .. } => DiskTransport::Sdhci
        }
    }

    pub fn format(&self) -> &str
    {
        match self
        {
            Self::VirtioMmio { format,
                               .. }
            | Self::VirtioPci { format,
                              .. }
            | Self::Sdhci { format,
                          .. } => format
        }
    }

    pub fn readonly(&self) -> bool
    {
        match self
        {
            Self::VirtioMmio { readonly,
                               .. }
            | Self::VirtioPci { readonly,
                              .. }
            | Self::Sdhci { readonly,
                          .. } => *readonly
        }
    }

    pub fn attach(&self) -> &[Attachment]
    {
        match self
        {
            Self::VirtioMmio { attach,
                               .. }
            | Self::VirtioPci { attach,
                              .. }
            | Self::Sdhci { attach,
                          .. } => attach
        }
    }

    /// How many disks the machine can hold, in the terms its transport counts.
    pub fn capacity(&self) -> usize
    {
        match self
        {
            Self::VirtioMmio { slots,
                               .. } => slots.len(),
            Self::VirtioPci { capacity,
                              .. } => capacity.get(),
            // The SD host controller drives one card.
            Self::Sdhci { .. } => 1
        }
    }

    /// The fixed bus slots, empty on a transport that assigns its own.
    pub fn slots(&self) -> &[u32]
    {
        match self
        {
            Self::VirtioMmio { slots,
                               .. } => slots,
            Self::VirtioPci { .. } | Self::Sdhci { .. } => &[]
        }
    }
}

/// One platform medium attached to the machine.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Attachment
{
    /// The storage image name.
    pub image: String,
    /// The bus slot, required on a bus whose slots are fixed guest addresses
    /// and forbidden on a bus that assigns its own.
    #[serde(default)]
    pub slot:  Option<u32>,
    /// Whether the machine boots from this medium. At most one attachment may
    /// be the boot medium, and a machine with attachments must name one.
    #[serde(default)]
    pub boot:  bool
}

fn four() -> NonZeroUsize
{
    NonZeroUsize::new(4).expect("a fixed nonzero disk capacity")
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ScreenMode
{
    Window,
    None
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Screen
{
    pub interactive: ScreenMode,
    pub headless:    ScreenMode
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ConsoleMode
{
    Stdio,
    File
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Console
{
    pub interactive: ConsoleMode,
    pub headless:    ConsoleMode
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Firmware
{
    /// No firmware is loaded and the machine's own default applies. A machine
    /// whose built-in firmware would otherwise claim the boot path states
    /// `disabled` instead.
    #[default]
    None,
    /// The emulator's built-in firmware is explicitly suppressed, so the boot
    /// payload is the first thing the processor runs.
    Disabled,
    /// A firmware image the machine loads before the boot image.
    Image
    {
        path: String
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Readiness
{
    /// How long the machine may take to reach a boot marker.
    ///
    /// An emulated cold boot is not fast: this record's own platform reaches its
    /// System marker at around 19 s on a development host, so a value near that
    /// reports the platform as broken whenever the host is busy. The budget is
    /// data because a slower board or a slower host changes it, and a timeout is
    /// only useful when it is longer than a working boot.
    pub timeout_seconds: NonZeroU64,
    /// A line containing any of these means the platform came up.
    pub booted:          Vec<String>,
    /// A line containing any of these means it failed.
    pub failed:          Vec<String>
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Definition
{
    /// What this machine is, in one line a newcomer can read.
    pub description: String,
    pub family:      Family,
    /// The program that implements the family, e.g. `qemu-system-riscv64`.
    pub binary:      String,
    /// The board model by name, e.g. `virt`.
    pub machine:     String,
    #[serde(default)]
    pub cpu:         Option<String>,
    pub ram:         String,
    pub harts:       NonZeroUsize,
    #[serde(default)]
    pub firmware:    Firmware,
    pub boot_medium: BootMedium,
    #[serde(default)]
    pub disks:       Option<Disks>,
    #[serde(default)]
    pub display:     Option<Display>,
    #[serde(default)]
    pub keyboard:    Option<Keyboard>,
    #[serde(default)]
    pub pointer:     Option<Pointer>,
    #[serde(default)]
    pub entropy:     Option<Entropy>,
    #[serde(default)]
    pub network:     Network,
    pub screen:      Screen,
    pub console:     Console,
    pub readiness:   Readiness,
    /// Devices this machine models, as measured rather than as the board has
    /// them. A machine whose description is incomplete — one that models a
    /// subset of a board — states the subset here, and this is the only list a
    /// generated tree for it may be built from. Copying the board's device list
    /// would invite the bootloader to probe hardware the machine does not have.
    #[serde(default)]
    pub devices:     Vec<super::hardware::Peripheral>,
    /// Whether this workspace compiles the machine's device list into a
    /// device-tree blob and hands it to the emulator with `-dtb`. Set it only
    /// for a machine that supplies no tree of its own; a machine that does —
    /// QEMU's own `virt` machine, or firmware that loads a board description —
    /// needs nothing generated and the bootloader reads the pointer it passes.
    ///
    /// Absence means false, and false does **not** mean the platform has no
    /// device tree: every platform hands the kernel one. It means this machine
    /// supplies its own. A machine that supplies none and is not given a
    /// generated one fails at boot with a named error — the aarch64 bootloader
    /// reports `BOOT_AARCH64_DT_REFUSED: the machine supplied no usable device
    /// tree` — rather than being handed an invented one.
    #[serde(default)]
    pub generate_device_tree: bool,
    /// Escape hatch for a one-off. Discouraged: anything a platform genuinely
    /// needs belongs in the schema above. No shipped platform record uses it.
    #[serde(default)]
    pub extra_args:  Vec<String>
}

impl Definition
{
    pub fn check(&self, platform: &str) -> Result<()>
    {
        let reject = |field: &str, why: String| {
            error(format!("platform '{platform}' emulation.{field}: {why}"))
        };
        if self.description.trim().is_empty()
        {
            return Err(reject("description", "must describe the machine".into()));
        }
        for (field, value, why) in [("binary", &self.binary, "must name the emulator program"),
                                    ("machine", &self.machine, "must name the board model"),
                                    ("ram", &self.ram, "must state the memory size")]
        {
            if value.trim().is_empty()
            {
                return Err(reject(field, why.into()));
            }
        }
        if self.readiness.booted.is_empty()
        {
            return Err(reject("readiness.booted", "must list a boot marker".into()));
        }
        if self.readiness.failed.is_empty()
        {
            return Err(reject("readiness.failed", "must list a failure marker".into()));
        }
        let markers =
            self.readiness
                .booted
                .iter()
                .map(|marker| ("readiness.booted", marker))
                .chain(self.readiness.failed.iter().map(|marker| ("readiness.failed", marker)));
        for (field, marker) in markers
        {
            if marker.is_empty()
            {
                return Err(reject(field, "must not be empty".into()));
            }
        }
        if let Firmware::Image { path } = &self.firmware
        {
            if path.trim().is_empty()
            {
                return Err(reject("firmware.image.path", "must name a firmware image".into()));
            }
        }
        if let BootMedium::Flash { format,
                                   .. } = &self.boot_medium
        {
            if format.trim().is_empty()
            {
                return Err(reject("boot_medium.format", "must name a disk format".into()));
            }
        }
        if self.boot_medium == BootMedium::Sd
        {
            // A machine that boots from SD must say which controller owns the
            // card, because that is what the guest and the launcher attach it
            // to. A flash or direct machine has no such requirement.
            match &self.disks
            {
                Some(disks) if disks.transport() == DiskTransport::Sdhci =>
                {}
                Some(_) =>
                {
                    return Err(reject("boot_medium.kind",
                                      "sd boot requires disks.transport sdhci".into()));
                }
                None =>
                {
                    return Err(reject("boot_medium.kind",
                                      "sd boot requires a disks section naming the sdhci \
                                       transport"
                                                 .into()));
                }
            }
        }
        let mut slots: Vec<(u32, &str)> = Vec::new();
        if let Some(disks) = &self.disks
        {
            if disks.format().trim().is_empty()
            {
                return Err(reject("disks.format", "must name a disk format".into()));
            }
            if let Disks::VirtioMmio { slots: offered,
                                       .. } = disks
            {
                if offered.is_empty() && disks.attach().is_empty()
                {
                    return Err(reject("disks.slots",
                                      "must list at least one bus slot for virtio-mmio".into()));
                }
                slots.extend(offered.iter().map(|slot| (*slot, "disks.slots")));
            }
            let mut boot = None;
            for (index, attachment) in disks.attach().iter().enumerate()
            {
                if attachment.image.trim().is_empty()
                {
                    return Err(reject(&format!("disks.attach[{index}].image"),
                                      "must name a storage image".into()));
                }
                if attachment.boot
                {
                    if boot.replace(index).is_some()
                    {
                        return Err(reject("disks.attach",
                                          "only one attachment may be the boot medium".into()));
                    }
                }
                match (disks.transport(), attachment.slot)
                {
                    (DiskTransport::VirtioMmio, Some(slot)) if disks.slots().contains(&slot) =>
                    {}
                    (DiskTransport::VirtioMmio, Some(slot)) =>
                    {
                        return Err(reject(&format!("disks.attach[{index}].slot"),
                                          format!("{slot} is not one of this machine's disk \
                                                   slots {:?}",
                                                  disks.slots())));
                    }
                    (DiskTransport::VirtioMmio, None) =>
                    {
                        return Err(reject(&format!("disks.attach[{index}].slot"),
                                          "must name the bus slot the guest expects on \
                                           virtio-mmio"
                                                       .into()));
                    }
                    (DiskTransport::VirtioPci, None) =>
                    {}
                    (DiskTransport::VirtioPci, Some(_)) =>
                    {
                        return Err(reject(&format!("disks.attach[{index}].slot"),
                                          "must be omitted for virtio-pci, which assigns its \
                                           own addresses"
                                                         .into()));
                    }
                    (DiskTransport::Sdhci, None) =>
                    {}
                    (DiskTransport::Sdhci, Some(_)) =>
                    {
                        return Err(reject(&format!("disks.attach[{index}].slot"),
                                          "must be omitted for the SD host controller, which \
                                           assigns its own card slot"
                                                                     .into()));
                    }
                }
            }
            if disks.transport() == DiskTransport::Sdhci && disks.attach().len() > 1
            {
                return Err(reject("disks.attach",
                                  "the SD host controller drives one card".into()));
            }
            if boot.is_none() && !disks.attach().is_empty()
            {
                return Err(reject("disks.attach",
                                  "must mark exactly one attachment `\"boot\": true`, so the \
                                   machine's boot medium is declared rather than inferred"
                                                                         .into()));
            }
        }
        if let Some(display) = &self.display
        {
            match (display.kind, &display.transport)
            {
                (DisplayKind::VirtioGpu, Some(_)) =>
                {}
                (DisplayKind::VirtioGpu, None) =>
                {
                    return Err(reject("display.transport",
                                      "must name the bus a virtio-gpu display sits on".into()));
                }
                (DisplayKind::VideoCoreMailbox, None) =>
                {}
                (DisplayKind::VideoCoreMailbox, Some(_)) =>
                {
                    return Err(reject("display.transport",
                                      "must be omitted for a display integrated into the SoC"
                                                                                 .into()));
                }
            }
            match (display.kind, display.framebuffer)
            {
                (DisplayKind::VideoCoreMailbox, Framebuffer::VideoCoreMailbox) =>
                {}
                (DisplayKind::VideoCoreMailbox, other) =>
                {
                    return Err(reject("display.framebuffer",
                                      format!("a VideoCore mailbox display needs framebuffer \
                                               videocore-mailbox, not {other:?}")));
                }
                (DisplayKind::VirtioGpu, Framebuffer::VideoCoreMailbox) =>
                {
                    return Err(reject("display.framebuffer",
                                      "a virtio-gpu display cannot use the SoC mailbox \
                                       framebuffer"
                                                   .into()));
                }
                _ =>
                {}
            }
        }
        if let Some(entropy) = &self.entropy
        {
            match (entropy.kind, &entropy.transport)
            {
                (EntropyKind::VirtioRng, Some(_)) =>
                {}
                (EntropyKind::VirtioRng, None) =>
                {
                    return Err(reject("entropy.transport",
                                      "must name the bus a virtio-rng device sits on".into()));
                }
                (EntropyKind::Bcm2835Rng, None) =>
                {}
                (EntropyKind::Bcm2835Rng, Some(_)) =>
                {
                    return Err(reject("entropy.transport",
                                      "must be omitted for a generator integrated into the SoC"
                                                                                 .into()));
                }
            }
        }
        let devices =
            [("display", self.display.as_ref().and_then(|device| device.transport.as_ref())),
             ("keyboard", self.keyboard.as_ref().map(|device| &device.transport)),
             ("pointer", self.pointer.as_ref().map(|device| &device.transport)),
             ("entropy", self.entropy.as_ref().and_then(|device| device.transport.as_ref()))];
        for (field, transport) in
            devices.into_iter().filter_map(|(field, transport)| Some((field, transport?)))
        {
            if let Some(slot) = transport.slot()
            {
                slots.push((slot, field));
            }
        }
        for (index, (slot, field)) in slots.iter().enumerate()
        {
            let Some((_, other)) = slots[..index].iter().find(|(seen, _)| seen == slot)
            else
            {
                continue;
            };
            if other != field
            {
                return Err(error(format!("platform '{platform}' emulation: bus slot {slot} is \
                                          claimed by both {other} and {field}")));
            }
            return Err(reject(field, format!("bus slot {slot} is listed twice")));
        }
        for argument in &self.extra_args
        {
            if argument.trim().is_empty()
            {
                return Err(reject("extra_args", "must not contain an empty argument".into()));
            }
        }
        Ok(())
    }

    /// The bus slots available to caller-supplied media, in attach order: every
    /// disk slot the machine exposes that the platform does not already occupy.
    /// A bus that assigns its own addresses has no fixed slots to hand out.
    pub fn disk_slots(&self) -> Vec<u32>
    {
        let attached = self.attachments().filter_map(|(_, slot)| slot).collect::<Vec<_>>();
        self.disks
            .as_ref()
            .map(|disks| {
                disks.slots().iter().copied().filter(|slot| !attached.contains(slot)).collect()
            })
            .unwrap_or_default()
    }

    /// The platform media this machine attaches, as (storage image, bus slot).
    pub fn attachments(&self) -> impl Iterator<Item = (&str, Option<u32>)>
    {
        self.disks
            .iter()
            .flat_map(|disks| disks.attach())
            .map(|attachment| (attachment.image.as_str(), attachment.slot))
    }

    /// The storage image this machine boots from, when the record names one.
    pub fn boot_image(&self) -> Option<&str>
    {
        self.disks
            .iter()
            .flat_map(|disks| disks.attach())
            .find(|attachment| attachment.boot)
            .map(|attachment| attachment.image.as_str())
    }

    /// How many disks this machine accepts, in the terms its transport counts.
    pub fn disk_capacity(&self) -> usize
    {
        self.disks.as_ref().map(Disks::capacity).unwrap_or_default()
    }
}

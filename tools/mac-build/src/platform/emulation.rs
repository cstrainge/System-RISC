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
    VirtioGpu
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Framebuffer
{
    #[default]
    None,
    Ramfb
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Display
{
    pub kind:        DisplayKind,
    #[serde(default)]
    pub framebuffer: Framebuffer,
    pub transport:   Transport
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
    VirtioRng
}

#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Entropy
{
    pub kind:      EntropyKind,
    pub transport: Transport
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
    VirtioPci
}

/// Attachable block devices. `slots` lists every bus slot this machine exposes
/// to disks; each slot is a fixed guest-visible address, so the list is part of
/// the contract and not a count of how many disks happen to be attached today.
/// `attach` names platform media, by storage image name, at the slots the guest
/// expects them; the remaining slots take caller-supplied media.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Disks
{
    pub transport: DiskTransport,
    #[serde(default = "raw")]
    pub format:    String,
    #[serde(default = "readonly")]
    pub readonly:  bool,
    #[serde(default)]
    pub slots:     Vec<u32>,
    #[serde(default)]
    pub attach:    Vec<Attachment>,
    #[serde(default = "four")]
    pub capacity:  NonZeroUsize
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
    pub slot:  Option<u32>
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
    /// No firmware: the boot image is the first thing the processor runs.
    #[default]
    None,
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
        if self.binary.trim().is_empty()
        {
            return Err(reject("binary", "must name the emulator program".into()));
        }
        if self.machine.trim().is_empty()
        {
            return Err(reject("machine", "must name the board model".into()));
        }
        if self.ram.trim().is_empty()
        {
            return Err(reject("ram", "must state the memory size".into()));
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
        let mut slots: Vec<(u32, &str)> = Vec::new();
        if let Some(disks) = &self.disks
        {
            if disks.format.trim().is_empty()
            {
                return Err(reject("disks.format", "must name a disk format".into()));
            }
            match disks.transport
            {
                DiskTransport::VirtioMmio =>
                {
                    if disks.slots.is_empty() && disks.attach.is_empty()
                    {
                        return Err(reject("disks.slots",
                                          "must list at least one bus slot for virtio-mmio".into()));
                    }
                    slots.extend(disks.slots.iter().map(|slot| (*slot, "disks.slots")));
                }
                DiskTransport::VirtioPci if !disks.slots.is_empty() =>
                {
                    return Err(reject("disks.slots",
                                      "must be empty for virtio-pci, which assigns its own \
                                       addresses"
                                                 .into()));
                }
                DiskTransport::VirtioPci =>
                {}
            }
            for (index, attachment) in disks.attach.iter().enumerate()
            {
                if attachment.image.trim().is_empty()
                {
                    return Err(reject(&format!("disks.attach[{index}].image"),
                                      "must name a storage image".into()));
                }
                match (disks.transport, attachment.slot)
                {
                    (DiskTransport::VirtioMmio, Some(slot)) if disks.slots.contains(&slot) =>
                    {}
                    (DiskTransport::VirtioMmio, Some(slot)) =>
                    {
                        return Err(reject(&format!("disks.attach[{index}].slot"),
                                          format!("{slot} is not one of this machine's disk \
                                                   slots {:?}",
                                                  disks.slots)));
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
                }
            }
        }
        let devices = [("display", self.display.as_ref().map(|device| &device.transport)),
                       ("keyboard", self.keyboard.as_ref().map(|device| &device.transport)),
                       ("pointer", self.pointer.as_ref().map(|device| &device.transport)),
                       ("entropy", self.entropy.as_ref().map(|device| &device.transport))];
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
                disks.slots.iter().copied().filter(|slot| !attached.contains(slot)).collect()
            })
            .unwrap_or_default()
    }

    /// The platform media this machine attaches, as (storage image, bus slot).
    pub fn attachments(&self) -> impl Iterator<Item = (&str, Option<u32>)>
    {
        self.disks
            .iter()
            .flat_map(|disks| disks.attach.iter())
            .map(|attachment| (attachment.image.as_str(), attachment.slot))
    }

    /// How many disks this machine accepts.
    pub fn disk_capacity(&self) -> usize
    {
        match &self.disks
        {
            Some(disks) if disks.slots.is_empty() => disks.capacity.get(),
            Some(disks) => disks.slots.len(),
            None => 0
        }
    }
}

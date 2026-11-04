//! Translate a platform's declared machine into an emulator command line.
//!
//! This is the only place that knows an emulator's flag syntax. A record
//! describes devices by function (`pointer: { kind: tablet }`) and this module
//! maps that function to the flags of a specific emulator family, keyed by
//! [`Family`]. Adding a platform therefore never means writing flags, and a
//! record can never smuggle a flag string in: unknown kinds and families are
//! rejected when the record loads.

use std::{ffi::OsString,
          path::{Path, PathBuf},
          process::Command,
          time::Duration};

use mac_build::{Result, error,
                platform::{Platform,
                           emulation::{BootMedium, ConsoleMode, Definition, DiskTransport,
                                       Family, Firmware, Framebuffer, NetworkKind, PointerKind,
                                       ScreenMode, Transport}}};

use crate::{boot, tools};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode
{
    Interactive,
    Headless
}

impl Mode
{
    fn name(self) -> &'static str
    {
        match self
        {
            Self::Interactive => "interactive",
            Self::Headless => "headless"
        }
    }
}

/// Where the guest's console goes. Serial capture uses a file, never a named
/// pipe, so the same code works on a host with no POSIX facilities.
#[derive(Clone, Debug)]
pub enum Serial
{
    Stdio,
    File(PathBuf)
}

/// A ready-to-spawn emulator process, plus the readiness contract the caller
/// uses to decide whether the guest came up.
pub struct Invocation
{
    pub program:     OsString,
    pub args:        Vec<String>,
    pub timeout:     Duration,
    pub booted:      Vec<String>,
    pub failed:      Vec<String>,
    pub interactive: bool,
    pub description: String
}

impl Invocation
{
    pub fn command(&self) -> Command
    {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        command
    }

    /// One line naming the machine and its binary, for the build report and for
    /// the message printed before the guest starts.
    pub fn describe(&self) -> String
    {
        format!("{} ({})", self.description, Path::new(&self.program).display())
    }

    pub fn booted_by(&self, line: &str) -> bool
    {
        self.booted.iter().any(|marker| line.contains(marker.as_str()))
    }

    pub fn failed_by(&self, line: &str) -> bool
    {
        self.failed.iter().any(|marker| line.contains(marker.as_str()))
    }

    fn mode_name(&self) -> &'static str
    {
        if self.interactive { Mode::Interactive.name() } else { Mode::Headless.name() }
    }
}

/// Build the emulator invocation for a platform.
///
/// `media` are caller-supplied disks (a clone of the platform disk, for
/// instance); the platform's own declared attachments are added here so the
/// caller never restates them. `extra` is the user's `--` passthrough.
pub fn invocation(platform: &Platform, root: &Path, dist: &Path, media: &[PathBuf], mode: Mode,
                  serial: Serial, extra: &[String])
                  -> Result<Invocation>
{
    let definition = platform.require_emulation()?;
    let program = tools::resolve(&definition.binary)?;
    let mut args = Vec::new();
    match definition.family
    {
        Family::Qemu => qemu(definition, platform, root, dist, media, mode, &serial, &mut args)?
    }
    args.extend(definition.extra_args.iter().cloned());
    args.extend(extra.iter().cloned());
    Ok(Invocation { program: program.into(),
                    args,
                    timeout: Duration::from_secs(definition.readiness.timeout_seconds.get()),
                    booted: definition.readiness.booted.clone(),
                    failed: definition.readiness.failed.clone(),
                    interactive: mode == Mode::Interactive,
                    description: definition.description.clone() })
}

fn push(args: &mut Vec<String>, flag: &str, value: impl Into<String>)
{
    args.push(flag.to_string());
    args.push(value.into());
}

fn device(args: &mut Vec<String>, model: &str, transport: Transport)
{
    match transport
    {
        Transport::VirtioMmio { slot } =>
        {
            push(args, "-device", format!("{model},bus=virtio-mmio-bus.{slot}"))
        }
        Transport::VirtioPci => push(args, "-device", model.to_string())
    }
}

fn qemu(definition: &Definition, platform: &Platform, root: &Path, dist: &Path, media: &[PathBuf],
        mode: Mode, serial: &Serial, args: &mut Vec<String>)
        -> Result<()>
{
    push(args, "-machine", definition.machine.clone());
    if let Some(cpu) = &definition.cpu
    {
        push(args, "-cpu", cpu.clone());
    }
    push(args, "-m", definition.ram.clone());
    push(args, "-smp", definition.harts.get().to_string());
    match &definition.firmware
    {
        Firmware::None => push(args, "-bios", "none"),
        Firmware::Image { path } => push(args, "-bios", root.join(path).display().to_string())
    }
    match &definition.boot_medium
    {
        BootMedium::Flash { unit,
                            format,
                            readonly } =>
        {
            let payload = boot::payload(platform, dist)?;
            push(args,
                 "-drive",
                 format!("if=pflash,unit={unit},format={format},readonly={},file={}",
                         if *readonly { "on" } else { "off" },
                         payload.display()));
        }
        BootMedium::Direct =>
        {}
    }
    // The legacy MMIO transport is off by default in current QEMU; a machine
    // that exposes virtio-mmio slots needs it, and a machine that does not must
    // not be given the flag.
    if uses_mmio(definition)
    {
        push(args, "-global", "virtio-mmio.force-legacy=false");
    }
    push(args, "-monitor", "none");
    match definition.network.kind
    {
        NetworkKind::None => push(args, "-nic", "none"),
        NetworkKind::User => push(args, "-nic", "user")
    }
    if let Some(display) = &definition.display
    {
        device(args, "virtio-gpu-device", display.transport.clone());
        if display.framebuffer == Framebuffer::Ramfb
        {
            push(args, "-device", "ramfb");
        }
    }
    if let Some(keyboard) = &definition.keyboard
    {
        device(args, "virtio-keyboard-device", keyboard.transport.clone());
    }
    if let Some(pointer) = &definition.pointer
    {
        let model = match pointer.kind
        {
            PointerKind::Tablet => "virtio-tablet-device",
            PointerKind::Mouse => "virtio-mouse-device"
        };
        device(args, model, pointer.transport.clone());
    }
    if let Some(entropy) = &definition.entropy
    {
        push(args, "-object", "rng-builtin,id=system-rng");
        match entropy.transport
        {
            Transport::VirtioMmio { slot } =>
            {
                push(args,
                     "-device",
                     format!("virtio-rng-device,rng=system-rng,bus=virtio-mmio-bus.{slot}"))
            }
            Transport::VirtioPci =>
            {
                push(args, "-device", "virtio-rng-device,rng=system-rng".to_string())
            }
        }
    }
    let screen = match mode
    {
        Mode::Interactive => definition.screen.interactive,
        Mode::Headless => definition.screen.headless
    };
    if screen == ScreenMode::None
    {
        push(args, "-display", "none");
    }
    let console = match mode
    {
        Mode::Interactive => definition.console.interactive,
        Mode::Headless => definition.console.headless
    };
    match (console, serial)
    {
        (ConsoleMode::Stdio, _) | (_, Serial::Stdio) => push(args, "-serial", "stdio"),
        (ConsoleMode::File, Serial::File(path)) =>
        {
            push(args, "-serial", format!("file:{}", path.display()))
        }
    }
    attach_disks(definition, platform, dist, media, args)
}

fn uses_mmio(definition: &Definition) -> bool
{
    let mmio = |transport: &Transport| matches!(transport, Transport::VirtioMmio { .. });
    definition.display.as_ref().is_some_and(|display| mmio(&display.transport))
    || definition.keyboard.as_ref().is_some_and(|keyboard| mmio(&keyboard.transport))
    || definition.pointer.as_ref().is_some_and(|pointer| mmio(&pointer.transport))
    || definition.entropy.as_ref().is_some_and(|entropy| mmio(&entropy.transport))
    || definition.disks.as_ref().is_some_and(|disks| disks.transport == DiskTransport::VirtioMmio)
}

fn attach_disks(definition: &Definition, platform: &Platform, dist: &Path, media: &[PathBuf],
                args: &mut Vec<String>)
                -> Result<()>
{
    let Some(disks) = &definition.disks
    else
    {
        if !media.is_empty()
        {
            return Err(error(format!("platform '{}' declares no disk bus, so it cannot attach \
                                      {}",
                                     platform.name,
                                     media.iter()
                                          .map(|path| path.display().to_string())
                                          .collect::<Vec<_>>()
                                          .join(", "))));
        }
        return Ok(());
    };
    let mut attached: Vec<(u32, PathBuf)> = Vec::new();
    let mut used: Vec<u32> = Vec::new();
    for (image, slot) in definition.attachments()
    {
        let path = boot::media(platform, dist, image)?;
        let slot = match (slot, disks.transport)
        {
            (Some(slot), _) => slot,
            (None, DiskTransport::VirtioPci) => 0,
            (None, DiskTransport::VirtioMmio) =>
            {
                return Err(error(format!("platform '{}' attaches image '{image}' to a \
                                          virtio-mmio bus without a slot; slots are fixed \
                                          guest addresses, so each attachment must name one",
                                         platform.name)));
            }
        };
        attached.push((slot, path));
        used.push(slot);
    }
    let mut free = disks.slots.clone();
    free.retain(|slot| !used.contains(slot));
    for path in media
    {
        if attached.len() >= disks.capacity.get()
        {
            return Err(error(format!("platform '{}' accepts at most {} attached disks, and {} \
                                      were requested",
                                     platform.name,
                                     disks.capacity.get(),
                                     attached.len() + media.len())));
        }
        let Some(slot) = free.first().copied()
        else
        {
            return Err(error(format!("platform '{}' declares disk slots {:?}, all of which \
                                      are taken; add a slot to emulation.disks.slots to \
                                      attach another disk",
                                     platform.name, disks.slots)));
        };
        free.remove(0);
        attached.push((slot, path.clone()));
    }
    for (index, (slot, path)) in attached.iter().enumerate()
    {
        let drive = format!("disk{index}");
        // The medium's own magic decides its format; the platform default
        // covers a file too short to sniff.
        let format =
            boot::disk_format(path).map(str::to_string).unwrap_or_else(|_| disks.format.clone());
        push(args,
             "-drive",
             format!("if=none,id={drive},readonly={},format={},file={}",
                     if disks.readonly { "on" } else { "off" },
                     format,
                     path.display()));
        match disks.transport
        {
            DiskTransport::VirtioMmio =>
            {
                push(args,
                     "-device",
                     format!("virtio-blk-device,drive={drive},bus=virtio-mmio-bus.{slot}"))
            }
            DiskTransport::VirtioPci =>
            {
                push(args, "-device", format!("virtio-blk-pci,drive={drive}"))
            }
        }
    }
    Ok(())
}

/// Render the invocation for a report line without resolving the binary, so a
/// report can be written on a host that has no emulator installed.
pub fn render(invocation: &Invocation) -> String
{
    let mut line = Path::new(&invocation.program).display().to_string();
    for arg in &invocation.args
    {
        line.push(' ');
        if arg.contains(' ')
        {
            line.push('"');
            line.push_str(arg);
            line.push('"');
        }
        else
        {
            line.push_str(arg);
        }
    }
    format!("[{}] {line}", invocation.mode_name())
}

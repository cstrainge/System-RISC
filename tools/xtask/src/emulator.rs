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
                                       DisplayKind, EntropyBackend, EntropyKind, Family,
                                       Firmware, Framebuffer, NetworkKind, PointerKind,
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
/// `media` says which disks the machine is given; see `Media`. `extra` is the
/// user's `--` passthrough.
/// Which disks the machine is given.
///
/// The record normally says, and that is the shipped machine. A boot check or a
/// request to boot a named disk is talking about specific media instead, so it
/// states them and the record's own attachments are not attached underneath it —
/// otherwise a case that means "no disk" would silently boot the platform's disk.
#[derive(Clone, Copy)]
pub enum Media<'a>
{
    /// The disks the platform record attaches.
    Declared,
    /// Exactly these disks, in order, replacing the record's attachments.
    Given(&'a [PathBuf])
}

pub fn invocation(platform: &Platform, root: &Path, dist: &Path, media: Media<'_>, mode: Mode,
                  extra: &[String])
                  -> Result<Invocation>
{
    let definition = platform.require_emulation()?;
    let program = tools::resolve(&definition.binary)?;
    let mut args = Vec::new();
    match definition.family
    {
        Family::Qemu => qemu(definition, platform, root, dist, media, mode, &mut args)?
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

// B18: the platform owns the emulator invocation. Every argument below is
// either the spelling of a field the record states, or a fact about the
// emulator itself. There is no third category, and `extra_args` is not one:
// a machine that needs something says so in the schema, so the next machine
// inherits the concept instead of a string that bypasses every cross-check the
// loader performs. The user reserved the right to revisit this placement; it is
// a deliberate choice, made for the same reason storage, hardware and the
// bootloader are records — a machine's run parameters are facts about the
// machine, and keeping them out of Rust is what makes a second board a data
// change rather than a code change.
fn qemu(definition: &Definition, platform: &Platform, root: &Path, dist: &Path, media: Media<'_>,
        mode: Mode, args: &mut Vec<String>)
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
        Firmware::None =>
        {}
        // `-bios none` is RISC-V's spelling for "start the kernel directly". An
        // aarch64 machine rejects it outright ("Could not find ROM image
        // 'none'"), and there the same meaning is expressed by omitting -bios
        // entirely, which is what this renderer already does for Firmware::None.
        Firmware::Disabled =>
        {
            if platform.rust_target.starts_with("riscv")
            {
                push(args, "-bios", "none");
            }
        }
        Firmware::Image { path } => push(args, "-bios", root.join(path).display().to_string())
    }
    // The four fields of `boot_medium.flash` are the four parts of QEMU's
    // `-drive if=pflash` spelling: which pflash unit holds the payload, what
    // format the payload is in, whether the machine may write it back, and the
    // payload itself, which the bootloader record produced.
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
        BootMedium::Sd =>
        {}
        // A machine that loads a bootable image itself takes the payload where
        // the record says its first stage goes. `-kernel` is that machine's
        // spelling for it: the emulator reads the image and places it at its
        // link address rather than exposing a flash device to fetch from.
        BootMedium::Direct =>
        {
            let payload = boot::payload(platform, dist)?;
            push(args, "-kernel", payload.display().to_string());
        }
    }
    // A machine that supplies no tree of its own is handed the one this build
    // generated from its emulation profile. The emulator passes it back in x0,
    // so the bootloader sees one pointer either way and the tree's origin is a
    // decision made here rather than something the bootloader or kernel knows.
    if definition.generate_device_tree
    {
        let tree = dist.join(format!("{}.dtb", platform.name));
        push(args, "-dtb", tree.display().to_string());
    }
    // Translates the transports the record names: a machine whose disks,
    // display, keyboard, pointer or entropy sit on `virtio-mmio` needs the
    // modern transport, and one that does not must never be given the flag.
    // QEMU's own default for this global depends on its version, which is a
    // fact about the emulator rather than about the machine, so the renderer
    // states it instead of asking the record to repeat it.
    if uses_mmio(definition)
    {
        push(args, "-global", "virtio-mmio.force-legacy=false");
    }
    // Translates `console`: the record says which channel the guest's console
    // owns, so the emulator's own human monitor never contends for it.
    push(args, "-monitor", "none");
    match definition.network.kind
    {
        NetworkKind::None => push(args, "-nic", "none"),
        NetworkKind::User => push(args, "-nic", "user")
    }
    if let Some(display) = &definition.display
    {
        // `display.kind` selects the device; `display.transport` selects where
        // it is attached. A display integrated into the SoC has no attachable
        // bus and is provided by the machine itself, so the renderer emits no
        // device for it.
        match display.kind
        {
            DisplayKind::VirtioGpu =>
            {
                if let Some(transport) = display.transport.clone()
                {
                    device(args, "virtio-gpu-device", transport);
                }
                // `display.framebuffer`: a machine that asks for `ramfb` gets
                // the second display device that provides it.
                if display.framebuffer == Framebuffer::Ramfb
                {
                    push(args, "-device", "ramfb");
                }
            }
            DisplayKind::VideoCoreMailbox =>
            {}
        }
    }
    if let Some(keyboard) = &definition.keyboard
    {
        // `keyboard.kind` selects the device; `keyboard.transport` its bus.
        device(args, "virtio-keyboard-device", keyboard.transport.clone());
    }
    if let Some(pointer) = &definition.pointer
    {
        // `pointer.kind` selects the device; `pointer.transport` its bus.
        let model = match pointer.kind
        {
            PointerKind::Tablet => "virtio-tablet-device",
            PointerKind::Mouse => "virtio-mouse-device"
        };
        device(args, model, pointer.transport.clone());
    }
    if let Some(entropy) = &definition.entropy
    {
        // `entropy.kind` selects the generator. A generator integrated into the
        // SoC is provided by the machine itself, so it needs no backend object
        // and no device.
        match entropy.kind
        {
            EntropyKind::VirtioRng =>
            {
                // `entropy.backend` selects the source. `id=system-rng` is the
                // renderer's own name for the pairing between the backend
                // object and the device that consumes it, so it is plumbing,
                // not a machine fact.
                let backend = match entropy.backend
                {
                    EntropyBackend::Builtin => "rng-builtin,id=system-rng"
                };
                push(args, "-object", backend.to_string());
                // `entropy.transport` selects the bus.
                match entropy.transport
                {
                    Some(Transport::VirtioMmio { slot }) =>
                    {
                        push(args,
                             "-device",
                             format!("virtio-rng-device,rng=system-rng,bus=virtio-mmio-bus.{slot}"))
                    }
                    Some(Transport::VirtioPci) =>
                    {
                        push(args, "-device", "virtio-rng-device,rng=system-rng".to_string())
                    }
                    None =>
                    {}
                }
            }
            EntropyKind::Bcm2835Rng =>
            {}
        }
    }
    // `screen.interactive` / `screen.headless` say whether this mode shows a
    // window.
    let screen = match mode
    {
        Mode::Interactive => definition.screen.interactive,
        Mode::Headless => definition.screen.headless
    };
    let clipboard = definition.clipboard.as_ref().filter(|_| screen == ScreenMode::Window);
    if let Some(clipboard) = clipboard
    {
        push(args, "-chardev", "qemu-vdagent,id=clipboard,clipboard=on,mouse=off");
        match clipboard.transport
        {
            Transport::VirtioMmio { slot } =>
            {
                push(args,
                     "-device",
                     format!("virtio-serial-device,id=clipboardserial,bus=virtio-mmio-bus.\
                              {slot},max_ports=2"));
            }
            Transport::VirtioPci =>
            {
                push(args, "-device", "virtio-serial-pci,id=clipboardserial,max_ports=2");
            }
        }
        push(args, "-device",
             "virtserialport,bus=clipboardserial.0,nr=1,chardev=clipboard,name=com.redhat.spice.0");
    }
    match screen
    {
        ScreenMode::None => push(args, "-display", "none"),
        ScreenMode::Window =>
        {
            let display = if cfg!(target_os = "macos")
            {
                "cocoa,zoom-to-fit=off,scale=1.5"
            }
            else
            {
                "gtk,zoom-to-fit=off,scale=1.5"
            };
            let display = if clipboard.is_some() && !cfg!(target_os = "macos")
            {
                format!("{display},clipboard=on")
            }
            else
            {
                display.to_string()
            };
            push(args, "-display", display);
        }
    }
    // `console.interactive` / `console.headless` say where the guest's console
    // goes in each mode; the caller may also have captured it to a file.
    let console = match mode
    {
        Mode::Interactive => definition.console.interactive,
        Mode::Headless => definition.console.headless
    };
    // `console.interactive` / `console.headless` name where the console goes.
    // It is always delivered on the emulator's standard output, because that is
    // the only destination the host can read while the guest is still running:
    // the emulator's own file backend buffers, so a reader watching that file
    // sees nothing until the buffer fills or the process exits. The host writes
    // the console where the record says, which is a storage decision rather
    // than an argument, so it does not change this renderer.
    match console
    {
        ConsoleMode::Stdio | ConsoleMode::File => push(args, "-serial", "stdio")
    }
    attach_disks(definition, platform, dist, media, args)
}

fn uses_mmio(definition: &Definition) -> bool
{
    let mmio = |transport: &Transport| matches!(transport, Transport::VirtioMmio { .. });
    definition.display.as_ref().and_then(|display| display.transport.as_ref()).is_some_and(mmio)
    || definition.keyboard.as_ref().is_some_and(|keyboard| mmio(&keyboard.transport))
    || definition.pointer.as_ref().is_some_and(|pointer| mmio(&pointer.transport))
    || definition.entropy.as_ref().and_then(|entropy| entropy.transport.as_ref()).is_some_and(mmio)
    || definition.clipboard.as_ref().is_some_and(|clipboard| mmio(&clipboard.transport))
    || definition.disks.as_ref().is_some_and(|disks| disks.transport() == DiskTransport::VirtioMmio)
}

fn attach_disks(definition: &Definition, platform: &Platform, dist: &Path, media: Media<'_>,
                args: &mut Vec<String>)
                -> Result<()>
{
    // The record's attachments are the machine's own. A caller that names media
    // is describing a different machine for one run, so it replaces them rather
    // than adding to them.
    let (given, declared, writable): (&[PathBuf], Vec<(&str, Option<u32>)>, bool) = match media
    {
        Media::Declared => (&[], definition.attachments().collect(), false),
        Media::Given(paths) => (paths, Vec::new(), false)
    };
    let Some(disks) = &definition.disks
    else
    {
        if !given.is_empty()
        {
            return Err(error(format!("platform '{}' declares no disk bus, so it cannot attach \
                                      {}",
                                     platform.name,
                                     given.iter()
                                          .map(|path| path.display().to_string())
                                          .collect::<Vec<_>>()
                                          .join(", "))));
        }
        return Ok(());
    };
    let mut attached: Vec<(u32, PathBuf)> = Vec::new();
    let mut used: Vec<u32> = Vec::new();
    for (image, slot) in declared
    {
        let path = boot::media(platform, dist, image)?;
        let slot = match (slot, disks.transport())
        {
            (Some(slot), _) => slot,
            (None, DiskTransport::VirtioPci | DiskTransport::Sdhci) => 0,
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
    let mut free = disks.slots().to_vec();
    free.retain(|slot| !used.contains(slot));
    for path in given
    {
        if attached.len() >= disks.capacity()
        {
            return Err(error(format!("platform '{}' accepts at most {} attached disks, and {} \
                                      were requested",
                                     platform.name,
                                     disks.capacity(),
                                     attached.len() + given.len())));
        }
        let Some(slot) = free.first().copied()
        else
        {
            return Err(error(format!("platform '{}' declares disk slots {:?}, all of which \
                                      are taken; add a slot to emulation.disks.slots to \
                                      attach another disk",
                                     platform.name,
                                     disks.slots())));
        };
        free.remove(0);
        attached.push((slot, path.clone()));
    }
    for (index, (slot, path)) in attached.iter().enumerate()
    {
        let drive = format!("disk{index}");
        // The medium's own magic decides its format; the platform default
        // covers a file too short to sniff.
        let format = boot::disk_format(path).map(str::to_string)
                                            .unwrap_or_else(|_| disks.format().to_string());
        let readonly = if !writable && disks.readonly() { "on" } else { "off" };
        match disks.transport()
        {
            DiskTransport::VirtioMmio =>
            {
                push(args,
                     "-drive",
                     format!("if=none,id={drive},readonly={readonly},format={format},file={}",
                             path.display()));
                push(args,
                     "-device",
                     format!("virtio-blk-device,drive={drive},bus=virtio-mmio-bus.{slot}"))
            }
            DiskTransport::VirtioPci =>
            {
                push(args,
                     "-drive",
                     format!("if=none,id={drive},readonly={readonly},format={format},file={}",
                             path.display()));
                push(args, "-device", format!("virtio-blk-pci,drive={drive}"))
            }
            DiskTransport::Sdhci =>
            {
                // The machine's SD host controller is its default block device,
                // so the medium is attached as `if=sd`; the controller owns the
                // card slot and there is no separate device object to add.
                push(args,
                     "-drive",
                     format!("if=sd,readonly={readonly},format={format},file={}", path.display()));
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

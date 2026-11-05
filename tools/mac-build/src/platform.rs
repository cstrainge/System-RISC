//! Current native platform selection. The Rust target selects the LP64 calling
//! convention; the complete LLVM profile selects instructions, including V/B.
use std::{collections::BTreeSet,
          env, fs,
          path::{Path, PathBuf},
          process::Command,
          sync::LazyLock};

use serde::Deserialize;
use serde_json::Value;

use crate::{Result, error};

pub mod bootloader;
pub mod emulation;
pub mod hardware;
pub mod install;
pub mod memory;
mod devices;
pub mod storage;
pub mod virtual_memory;
pub mod scheduling;

/// The build manifest for one platform. Every section that describes a
/// particular architecture's physical machine is optional: a platform record
/// that only names an emulator or a board is valid, and a consumer that needs a
/// missing section fails with a message naming it rather than receiving a
/// default that was never declared.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Platform
{
    pub name:             String,
    pub rust_target:      String,
    /// The Rust target the pinned private SDK provides for `std` applications.
    /// Absent means this platform installs no `std` application, so the target
    /// is never needed; a platform that does install one and omits it is
    /// rejected rather than falling back to another platform's triple.
    #[serde(default)]
    pub std_target:       Option<String>,
    /// The instruction set profile as a `target-feature` value. Empty means the
    /// architecture has no such profile, in which case no flag is emitted.
    #[serde(default)]
    pub llvm_profile:     String,
    /// The feature names the compiler must report for `llvm_profile`. The
    /// profile is a platform fact, so what it has to contain is recorded beside
    /// it rather than assumed here: an architecture without a V/F/D-style
    /// profile lists what its own expansion must contain, and an empty list
    /// asks only that the profile itself appears.
    #[serde(default)]
    pub profile_requires: Vec<String>,
    /// The compiler record this platform is built with, relative to
    /// `platforms/`.
    ///
    /// One run resolves exactly one toolchain: rustup reads
    /// `rust-toolchain.toml` before any of this code executes, so two platforms
    /// naming different compilers need two checkouts rather than one build that
    /// switches between them. The field still exists, because a platform that
    /// says nothing about the tool that builds it is an incomplete record, and
    /// `cargo xtask` checks this file against the bootstrap pin and refuses to
    /// build when they disagree.
    #[serde(default = "compiler_record")]
    pub toolchain:        String,
    /// The linker script guest images link with, relative to the repository
    /// root. The target alone cannot choose it: two targets may share an
    /// architecture but need different section rules, and the record is the
    /// authority for the machine's link geometry.
    #[serde(default)]
    pub guest_linker:     Option<String>,
    /// Which workspace packages this platform installs, and with which
    /// features. The build tool reads the graph from Cargo and the selection
    /// from here, so changing the installed set is a platform edit.
    #[serde(default)]
    pub install:          install::Install,
    /// How to run this platform under an emulator. Optional and independent of
    /// the build; absent means this platform declares no emulation.
    #[serde(default)]
    pub emulation:        Option<emulation::Definition>,
    /// How to build and run this platform on a real board. Optional and
    /// independent of both the build and the emulation description.
    #[serde(default)]
    pub hardware:         Option<hardware::Definition>,
    /// The media this platform produces, attaches and writes. Storage lives
    /// here rather than under `emulation` or `hardware` because one described
    /// medium has to serve both.
    #[serde(default)]
    pub storage:          Option<storage::Storage>,
    /// The bootloader this platform builds. Optional: a board whose first stage
    /// is vendor firmware declares none.
    #[serde(default)]
    pub bootloader:       Option<bootloader::Definition>,
    /// True for a record meant only to be included by other platforms. Such a
    /// record describes shared data, not a machine, so it is not listed and
    /// cannot be selected.
    #[serde(default)]
    pub base:             bool,
    fatal_shutdown:       Option<FatalShutdown>,
    #[serde(default)]
    mmio_devices:         Vec<devices::Definition>,
    scheduling:           Option<scheduling::Definition>,
    memory:               Option<memory::Definition>,
    virtual_memory:       Option<virtual_memory::Definition>
}
pub const DEFAULT_PLATFORM: &str = "qemu_riscv";
pub const PLATFORM_ENV: &str = "SR_PLATFORM";

/// The only directory a platform record is read from.
fn directory() -> PathBuf
{
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../platforms")
}

/// Platform names available on this checkout, for error messages. Base records
/// are excluded: they are not selectable, so naming them would mislead.
fn known() -> String
{
    let names: Vec<String> =
        stems().into_iter().filter(|stem| !base_record(stem).unwrap_or(false)).collect();
    if names.is_empty() { "<none>".into() } else { names.join(", ") }
}

/// Every platform record in this checkout, with a one-line summary for
/// `cargo xtask platforms`. Each record is loaded and validated, so a broken
/// record is reported instead of being listed as if it were usable.
pub fn catalogue() -> Result<Vec<(String, String)>>
{
    let mut listed = Vec::new();
    for name in stems()
    {
        if base_record(&name)?
        {
            continue;
        }
        // Listing records answers "which platforms exist". One record that
        // cannot load is named with its own error rather than deciding that
        // every other platform is unreportable too; building or selecting that
        // record still fails on `load`.
        let platform = match load(&name)
        {
            Ok(platform) => platform,
            Err(why) =>
            {
                listed.push((name, format!("unusable, so it cannot be selected: {why}")));
                continue;
            }
        };
        let summary = platform.emulation()
                              .map(|emulation| emulation.description.clone())
                              .or_else(|| {
                                  platform.hardware.as_ref().map(|hardware| {
                                                                format!("{}; no emulation declared",
                                                                        hardware.description)
                                                            })
                              })
                              .unwrap_or_else(|| "no emulation or hardware description".into());
        listed.push((name, summary));
    }
    Ok(listed)
}

/// `include` composes platform records. A base record is merged into the record
/// that includes it, depth first, before that record is read:
///
/// * scalars and plain arrays replace the included value;
/// * objects merge key by key, recursively;
/// * `{"$append": [...]}` appends to the included array instead of replacing it, and is equivalent
///   to the plain array when the key is absent.
///
/// The path is relative to the directory of the file that names it, includes
/// may nest to any depth, and a repeated or self-referential include is
/// reported as a cycle rather than recursing.
const INCLUDE: &str = "include";
const APPEND: &str = "$append";

fn document(path: &Path, stack: &mut Vec<PathBuf>) -> Result<Value>
{
    let resolved =
        path.canonicalize()
            .map_err(|cause| {
                error(format!("cannot read platform file {}: {cause}", path.display()))
            })?;
    if let Some(position) = stack.iter().position(|seen| *seen == resolved)
    {
        let mut chain: Vec<String> =
            stack[position..].iter().map(|file| file.display().to_string()).collect();
        chain.push(resolved.display().to_string());
        return Err(error(format!("platform include cycle: {}", chain.join(" -> "))));
    }
    let bytes = fs::read(&resolved)?;
    let mut value: Value = serde_json::from_slice(&bytes).map_err(|cause| {
                                                             error(format!("invalid platform \
                                                                            file {}: {cause}",
                                                                           resolved.display()))
                                                         })?;
    let Value::Object(map) = &mut value
    else
    {
        return Err(error(format!("platform file {} must be a JSON object", resolved.display())));
    };
    let Some(include) = map.remove(INCLUDE)
    else
    {
        return Ok(value);
    };
    let include = include.as_str().ok_or_else(|| {
                                       error(format!("platform include in {} must be a string \
                                                      path",
                                                     resolved.display()))
                                   })?;
    let target = resolved.parent().unwrap_or(Path::new(".")).join(include);
    stack.push(resolved.clone());
    let base = document(&target, stack).map_err(|cause| {
                                           error(format!("platform {} includes '{include}': \
                                                          {cause}",
                                                         resolved.display()))
                                       })?;
    stack.pop();
    merge(base, value)
}

fn merge(base: Value, overlay: Value) -> Result<Value>
{
    // `$append` extends an array rather than replacing it.
    if let Value::Object(object) = &overlay
    {
        if let Some(append) = object.get(APPEND).filter(|_| object.len() == 1)
        {
            let Value::Array(items) = append
            else
            {
                return Err(error(format!("'{APPEND}' must be an array")));
            };
            let Value::Array(mut existing) = base
            else
            {
                return Err(error(format!("'{APPEND}' appends to an array, but the included \
                                          value is not one")));
            };
            existing.extend(items.iter().cloned());
            return Ok(Value::Array(existing));
        }
    }
    match (base, overlay)
    {
        (Value::Object(mut base), Value::Object(overlay)) =>
        {
            for (key, value) in overlay
            {
                let merged = match base.remove(&key)
                {
                    // An absent key takes the overlay value; `$append` is then
                    // equivalent to the plain array it wraps.
                    None => plain(value)?,
                    Some(current) => merge(current, value)?
                };
                base.insert(key, merged);
            }
            Ok(Value::Object(base))
        }
        // A scalar or a plain array replaces what it overlays.
        (_, overlay) => Ok(overlay)
    }
}

/// The value an overlay contributes when it overlays nothing.
fn plain(value: Value) -> Result<Value>
{
    let Value::Object(mut object) = value
    else
    {
        return Ok(value);
    };
    if object.len() != 1 || !object.contains_key(APPEND)
    {
        return Ok(Value::Object(object));
    }
    match object.remove(APPEND).unwrap()
    {
        Value::Array(items) => Ok(Value::Array(items)),
        _ => Err(error(format!("'{APPEND}' must be an array")))
    }
}

/// The merged JSON record for a platform, with every include resolved.
///
/// This is the interpreter's view of the manifest: callers that need to read a
/// section the typed [`Platform`] does not model (or that need the record's
/// own spelling of a value) use this instead of re-reading the file, so
/// include and merge semantics stay in one place.
pub fn record(name: &str) -> Result<Value>
{
    let name = resolve(name)?;
    let path = directory().join(format!("{name}.json"));
    document(&path, &mut Vec::new())
}

/// The canonical spelling of a platform name.
///
/// This is the single identifier used for `--platform`, for the output
/// directory and for everything the build report prints. It is derived from a
/// user-supplied spelling by lowercasing ASCII letters and replacing each run
/// of anything else with `_`, so `qemu_riscv`, `QemuRISC-V`, `QEMU_RISCV` and
/// `qemu riscv` all name the same platform. The result contains only
/// `[a-z0-9_]`, which is path-safe on every host, and it is case-folded, so two
/// spellings can never create two directories on a case-insensitive filesystem.
pub fn canonical(name: &str) -> String
{
    let mut out = String::with_capacity(name.len());
    for character in name.chars()
    {
        if character.is_ascii_alphanumeric()
        {
            out.push(character.to_ascii_lowercase());
        }
        else if !out.ends_with('_')
        {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

/// The spelling-insensitive key a typed name is matched by. `QemuRISC-V`,
/// `QEMU_RISCV`, `qemu riscv` and `qemu_riscv` all squash to `qemuriscv`, so a
/// developer does not have to remember where the underscores are.
fn squash(name: &str) -> String
{
    name.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

/// The name a record's file must carry to be usable as a directory segment on
/// every host: lower-case ASCII, digits and single underscores.
fn path_safe(stem: &str) -> bool
{
    !stem.is_empty() && canonical(stem) == stem
}

/// Whether a record is a base other platforms include, read without resolving
/// so that `resolve` can ask the question without recursing.
fn base_record(stem: &str) -> Result<bool>
{
    let path = directory().join(format!("{stem}.json"));
    let text = fs::read_to_string(&path).map_err(|cause| {
                                            error(format!("cannot read platform file {}: {cause}",
                                                          path.display()))
                                        })?;
    let value: Value = serde_json::from_str(&text).map_err(|cause| {
                                                      error(format!("cannot read platform file \
                                                                     {}: {cause}",
                                                                    path.display()))
                                                  })?;
    Ok(value.get("base").and_then(Value::as_bool) == Some(true))
}

fn stems() -> Vec<String>
{
    let Ok(entries) = fs::read_dir(directory())
    else
    {
        return Vec::new();
    };
    let mut names: Vec<String> = entries.filter_map(|entry| {
                                            let path = entry.ok()?.path();
                                            if path.extension()? != "json"
                                            {
                                                return None;
                                            }
                                            Some(path.file_stem()?.to_str()?.to_string())
                                        })
                                        .collect();
    names.sort();
    names
}

/// Resolve any accepted spelling of a platform name to its canonical record
/// name, so callers never invent a second slugger for paths.
pub fn resolve(name: &str) -> Result<String>
{
    if name.contains(['/', '\\']) || name.contains("..")
    {
        return Err(error(format!("cannot select platform '{name}': a platform name must \
                                  contain no path separators or '..'")));
    }
    let wanted = squash(name);
    if wanted.is_empty()
    {
        return Err(error(format!("cannot select platform '{name}': a platform name must \
                                  contain at least one letter or digit; known platforms: {}",
                                 known())));
    }
    let mut matches: Vec<String> =
        stems().into_iter().filter(|stem| squash(stem) == wanted).collect();
    match matches.len()
    {
        1 =>
        {
            let stem = matches.remove(0);
            if !path_safe(&stem)
            {
                return Err(error(format!("platform record '{stem}.json' is named badly: the \
                                          file name must be lower-case ASCII letters, digits \
                                          and underscores so it can name an output directory \
                                          on every host; rename it to '{}.json'",
                                         canonical(&stem))));
            }
            if base_record(&stem)?
            {
                return Err(error(format!("cannot select platform '{name}': '{stem}' is a base \
                                          record that other platforms include, not a machine \
                                          of its own; select one of: {}",
                                         known())));
            }
            Ok(stem)
        }
        0 =>
        {
            Err(error(format!("cannot select platform '{name}': no platform record normalises \
                               to '{wanted}'; known platforms: {}",
                              known())))
        }
        _ =>
        {
            matches.sort();
            Err(error(format!("cannot select platform '{name}': '{wanted}' is ambiguous between \
                               {}",
                              matches.join(", "))))
        }
    }
}

pub fn load(name: &str) -> Result<Platform>
{
    let name = resolve(name)?;
    let path = directory().join(format!("{name}.json"));
    let document = record(&name)?;
    let platform: Platform = serde_json::from_value(document).map_err(|cause| {
                                                                 error(format!("invalid platform \
                                                                                '{name}' at {}: \
                                                                                {cause}",
                                                                               path.display()))
                                                             })?;
    if platform.name != name
    {
        return Err(error(format!("platform '{name}' declares a different name '{}'",
                                 platform.name)));
    }
    // A platform that installs guest modules links them, so it must name the
    // script they link with. A platform with no guest set is not required to
    // carry a linker record it never uses.
    if !platform.install.modules.is_empty() && platform.guest_linker.is_none()
    {
        return Err(error(format!("platform '{}' installs guest modules but declares no \
                                  guest_linker; set it to the linker script this target links \
                                  with",
                                 platform.name)));
    }
    platform.install.check(&platform.name)?;
    // Validate every section the record does declare, and nothing it does not:
    // a platform without an emulator or without a physical layout still loads.
    if platform.memory.is_some()
    {
        platform.memory()?;
    }
    if platform.virtual_memory.is_some()
    {
        platform.virtual_memory()?;
    }
    if platform.scheduling.is_some()
    {
        platform.scheduling()?;
    }
    if !platform.mmio_devices.is_empty() || platform.fatal_shutdown.is_some()
    {
        platform.device_artifacts(true)?;
    }
    if let Some(emulation) = &platform.emulation
    {
        emulation.check(&platform.name)?;
    }
    if let Some(hardware) = &platform.hardware
    {
        hardware.check(&platform.name)?;
    }
    platform.check_peripheral_windows()?;
    if let Some(storage) = &platform.storage
    {
        storage.check(&platform.name)?;
    }
    if let Some(bootloader) = &platform.bootloader
    {
        bootloader.check(&platform.name)?;
    }
    // A medium referenced by name must exist; a silently missing disk looks
    // exactly like a guest that fails to find its boot volume.
    if let (Some(storage), Some(emulation)) = (&platform.storage, &platform.emulation)
    {
        for (image, _) in emulation.attachments()
        {
            if storage.image(image).is_none()
            {
                return Err(error(format!("platform '{}' emulation.disks.attach names image \
                                          '{image}', which storage does not declare; declared \
                                          images: {}",
                                         platform.name,
                                         storage.names())));
            }
        }
    }
    if let (Some(storage), Some(hardware)) = (&platform.storage, &platform.hardware)
    {
        for image in hardware.images()
        {
            if storage.image(image).is_none()
            {
                return Err(error(format!("platform '{}' hardware.boot.image names image \
                                          '{image}', which storage does not declare; declared \
                                          images: {}",
                                         platform.name,
                                         storage.names())));
            }
        }
    }
    Ok(platform)
}
static PLATFORM: LazyLock<Platform> = LazyLock::new(|| {
    let name = env::var(PLATFORM_ENV).unwrap_or_else(|_| DEFAULT_PLATFORM.into());
    load(&name).expect("native platform selection failed")
});
pub fn current() -> &'static Platform
{
    &PLATFORM
}

/// A peripheral window as the record declares it, keyed by the part's own name
/// so a consumer asks for `sdhci` or `pl011` rather than an address.
pub struct DeclaredDevice
{
    pub compatible: String,
    pub base:       u64,
    pub bytes:      u64
}

impl Platform
{
    /// The harts this platform describes. A record that declares no emulation
    /// still runs somewhere, so the kernel-facing count defaults to one.
    pub fn harts(&self) -> usize
    {
        self.emulation.as_ref().map(|emulation| emulation.harts.get()).unwrap_or(1)
    }

    /// Every peripheral window the record declares. A peripheral that states a
    /// base but no size is refused rather than given a default or looked up in
    /// the MMIO inventory: one window must have one declaration, and a second
    /// source could disagree with the first. A peripheral with no base describes
    /// a function rather than a window and is skipped.
    pub fn declared_devices(&self) -> Result<Vec<DeclaredDevice>>
    {
        let hardware = match self.hardware.as_ref()
        {
            Some(hardware) => hardware,
            None => return Ok(Vec::new())
        };
        let mut devices = Vec::new();
        for peripheral in &hardware.devices
        {
            let base = match peripheral.base.as_deref()
            {
                Some(text) => Some(parse_hex(text).ok_or_else(|| {
                                                       crate::error(format!("platform '{}' \
                                                                             peripheral {} states an \
                                                                             unreadable base {text:?}",
                                                                            self.name,
                                                                            peripheral.model))
                                                   })?),
                None => None
            };
            let base = match base
            {
                Some(base) => base,
                None => continue
            };
            let bytes = match peripheral.bytes.as_deref()
            {
                Some(text) => parse_hex(text).ok_or_else(|| {
                                                 crate::error(format!("platform '{}' peripheral {} \
                                                                       states an unreadable size \
                                                                       {text:?}",
                                                                      self.name,
                                                                      peripheral.model))
                                             })?,
                None =>
                {
                    return Err(crate::error(format!("platform '{}' peripheral {} states a base but \
                                                     no size, so its window is not fully declared",
                                                    self.name,
                                                    peripheral.model)));
                }
            };
            devices.push(DeclaredDevice { compatible: peripheral.model.clone(),
                                          base,
                                          bytes });
        }
        Ok(devices)
    }

    /// Ties a peripheral's exact window to the page-granular range a driver
    /// claims. The MMIO inventory cannot state a sub-page window — its ranges
    /// must be page aligned and page sized — so a peripheral whose registers
    /// start part way into a page is declared twice: the window in
    /// `hardware.devices`, which becomes the device-tree node, and the page in
    /// `mmio_devices`, which a module matches and claims. Those are two
    /// declarations of one device, and a record that states them inconsistently
    /// is refused here rather than discovered when a driver claims the wrong
    /// page. Two windows inside one claimed page are refused for the same
    /// reason: one claim would then cover two devices.
    fn check_peripheral_windows(&self) -> Result<()>
    {
        let windows = self.declared_devices()?;
        if windows.is_empty() || self.mmio_devices.is_empty()
        {
            return Ok(());
        }
        let ranges = devices::check(&self.mmio_devices, &self.memory()?)?;
        for window in &windows
        {
            let end = window.base
                           .checked_add(window.bytes)
                           .ok_or_else(|| {
                               crate::error(format!("platform '{}' peripheral {} window \
                                                     0x{:x}+0x{:x} overflows the address space",
                                                    self.name, window.compatible, window.base,
                                                    window.bytes))
                           })?;
            // A device window must be covered by declared MMIO ranges, and a
            // block that genuinely spans several pages claims one range per
            // page: the clock manager's two-page extent is one device and two
            // claimable pages. The ranges therefore have to tile the window
            // exactly — no gap, no overlap — rather than all contain it.
            let mut covering: Vec<&devices::Checked> =
                ranges.iter()
                      .filter(|range| {
                          range.address < end && window.base < range.address + range.bytes
                      })
                      .collect();
            covering.sort_by_key(|range| range.address);
            if covering.is_empty()
            {
                return Err(crate::error(format!("platform '{}' peripheral {} window \
                                                 0x{:x}+0x{:x} lies outside every declared MMIO \
                                                 range, so no module can claim it",
                                                self.name,
                                                window.compatible,
                                                window.base,
                                                window.bytes)));
            }
            let mut cursor = window.base;
            for range in &covering
            {
                if range.address > cursor
                {
                    return Err(crate::error(format!("platform '{}' peripheral {} window \
                                                     0x{:x}+0x{:x} is not covered at 0x{:x}: the \
                                                     declared MMIO ranges leave a gap, so part of \
                                                     the device could not be claimed",
                                                    self.name,
                                                    window.compatible,
                                                    window.base,
                                                    window.bytes,
                                                    cursor)));
                }
                cursor = cursor.max(range.address + range.bytes);
                let occupants = windows.iter()
                                       .filter(|other| {
                                           range.address <= other.base
                                           && other.base + other.bytes
                                              <= range.address + range.bytes
                                       })
                                       .count();
                if occupants > 1
                {
                    return Err(crate::error(format!("platform '{}' MMIO range {} \
                                                     0x{:x}+0x{:x} contains {occupants} declared \
                                                     peripheral windows; one claim would cover \
                                                     more than one device",
                                                    self.name,
                                                    range.id,
                                                    range.address,
                                                    range.bytes)));
                }
            }
            if cursor < end
            {
                return Err(crate::error(format!("platform '{}' peripheral {} window \
                                                 0x{:x}+0x{:x} runs past its last declared MMIO \
                                                 range at 0x{:x}",
                                                self.name,
                                                window.compatible,
                                                window.base,
                                                window.bytes,
                                                cursor)));
            }
        }
        Ok(())
    }
}

fn parse_hex(text: &str) -> Option<u64>
{
    let digits = text.trim().trim_start_matches("0x").trim_start_matches("0X");
    u64::from_str_radix(digits, 16).ok()
}

/// The module contract target this platform admits. Modules carry their
/// architecture, pointer width, endianness and feature profile in the DABI/EABI
/// resource, and the resident kernel validates every contract against the target
/// of the machine it is running on. The record's `rust_target` is the single
/// source of that fact, so a second board states its own target rather than
/// inheriting a hardcoded one.
pub fn module_target() -> Result<mac_module::Target>
{
    let platform = current();
    match platform.rust_target.as_str()
    {
        "riscv64imac-unknown-none-elf" => Ok(mac_module::Target::RVA23U64),
        "aarch64-unknown-none" => Ok(mac_module::Target::AARCH64),
        target =>
        {
            Err(error(format!("platform '{}' declares rust_target {target:?}, which has no module \
                               contract target; add its architecture to \
                               mac_module::Target",
                              platform.name)))
        }
    }
}

/// The resident kernel's copy of the module contract target, generated into the
/// build output so the kernel compares module contracts against the machine the
/// record describes rather than a constant chosen at authoring time.
pub fn module_target_constants() -> Result<String>
{
    let target = module_target()?;
    Ok(format!("// Generated from the platform record's module contract target.\n\
                pub(crate) const MODULE_TARGET: mac_module::Target = mac_module::Target {{ \
                architecture: {architecture}, pointer_bits: {pointer_bits}, endian: {endian}, \
                convention: {convention}, features: {features} }};\n",
               architecture = target.architecture,
               pointer_bits = target.pointer_bits,
               endian = target.endian,
               convention = target.convention,
               features = target.features))
}

/// The rustup channel this checkout's compiler pin names.
///
/// The pin lives in `platforms/rust/sdk.json` beside the target specification
/// and the patches, so everything under `platforms/` is read here rather than
/// by whichever module happens to need one string out of it.
fn compiler_record() -> String
{
    "rust/sdk.json".to_string()
}

/// The absolute path of the compiler record a platform names.
pub fn compiler_path(root: &Path, platform: &Platform) -> Result<PathBuf>
{
    let record = platform.toolchain.trim();
    if record.is_empty()
    {
        return Err(error(format!("platform '{}' names no compiler record; set `toolchain` to \
                                  the platform's compiler pin, for example \"rust/sdk.json\"",
                                 platform.name)));
    }
    Ok(root.join("platforms").join(record))
}

/// The linker script this platform's guest images link with.
pub fn guest_linker_path(root: &Path, platform: &Platform) -> Result<PathBuf>
{
    let relative = platform.guest_linker.as_deref().ok_or_else(|| {
                                                        error(format!("platform '{}' declares no \
                                                                       guest_linker, so its \
                                                                       guest images cannot be \
                                                                       linked",
                                                                      platform.name))
                                                    })?;
    let path = Path::new(relative);
    if path.is_absolute() || relative.contains("..")
    {
        return Err(error(format!("platform '{}' guest_linker '{}' must be a relative path \
                                  inside the repository",
                                 platform.name, relative)));
    }
    Ok(root.join(path))
}

pub fn rust_toolchain(root: &Path, platform: &Platform) -> Result<String>
{
    let path = compiler_path(root, platform)?;
    let text = fs::read_to_string(&path).map_err(|cause| {
                                            error(format!("cannot read {}: {cause}",
                                                          path.display()))
                                        })?;
    let pin: Value = serde_json::from_str(&text).map_err(|cause| {
                                                    error(format!("{} is not a valid compiler \
                                                                   pin: {cause}",
                                                                  path.display()))
                                                })?;
    match pin["rustup_toolchain"].as_str()
    {
        Some(toolchain) if !toolchain.trim().is_empty() => Ok(toolchain.to_string()),
        _ => Err(error(format!("{} does not name a `rustup_toolchain`", path.display())))
    }
}
/// Guest builds launched outside the workspace still receive the same profile.
/// Existing flags remain visible; validation below rejects a narrowed or
/// broadened instruction set rather than silently ignoring caller flags.
pub fn configure(command: &mut Command)
{
    configure_for(current(), command);
}
/// The rustflags this record's instruction-set profile implies for the guest
/// target, as cargo's `target.<triple>.rustflags` list.
///
/// This is the one place the profile becomes a compiler flag. Nothing in
/// `.cargo/config.toml` repeats it: a static copy there would silently keep
/// building the old ISA after the record changed, and it would be a second
/// authority for the same fact. Cargo cannot read this workspace's JSON, so the
/// values are handed to the invocation that builds the guest instead.
pub fn guest_rustflags(platform: &Platform) -> Vec<String>
{
    let mut flags = Vec::new();
    if !platform.llvm_profile.is_empty()
    {
        flags.push("-C".to_string());
        flags.push(format!("target-feature=+{}", platform.llvm_profile));
    }
    flags.push("-C".to_string());
    flags.push("relocation-model=pic".to_string());
    flags
}

/// The same flags as one `cargo --config` argument.
///
/// The key is scoped to the guest triple, so host build scripts and proc macros
/// are not compiled with the guest's instruction set.
pub fn guest_rustflags_config(platform: &Platform) -> String
{
    let flags = guest_rustflags(platform);
    let encoded = flags.iter().map(|flag| format!("{flag:?}")).collect::<Vec<_>>().join(",");
    format!("target.{}.rustflags=[{encoded}]", platform.rust_target)
}

/// Extend one cargo invocation with the guest target's instruction profile.
pub fn configure_for(platform: &Platform, command: &mut Command)
{
    command.arg("--config").arg(guest_rustflags_config(platform));
    command.env(PLATFORM_ENV, &platform.name);
}
/// The compiler flag covers every Cargo-built target dependency, not just the
/// final executable. Refuse conflicting overrides instead of last-flag-wins.
pub fn validate_pic_flags() -> Result<()>
{
    let flags = env::var("CARGO_ENCODED_RUSTFLAGS").map_err(|_| {
                                                       error("native builds require encoded \
                                                              relocation-model=pic flags across \
                                                              all target dependencies; use cargo \
                                                              xtask image")
                                                   })?;
    let flags: Vec<_> = flags.split('\u{1f}').collect();
    let mut found = false;
    for (index, flag) in flags.iter().enumerate()
    {
        let option = if matches!(*flag, "-C" | "--codegen")
        {
            flags.get(index + 1).copied()
        }
        else
        {
            flag.strip_prefix("-C").or_else(|| flag.strip_prefix("--codegen="))
        };
        if let Some(value) = option.and_then(|option| option.strip_prefix("relocation-model="))
        {
            if value != "pic"
            {
                return Err(error(format!("native code requires relocation-model=pic for every \
                                          target dependency; remove conflicting {value:?} \
                                          override and rebuild with cargo xtask image")));
            }
            found = true;
        }
    }
    if !found
    {
        return Err(error("native code lacks relocation-model=pic across target dependencies; \
                          rebuild using cargo xtask image"));
    }
    Ok(())
}
fn features(text: &str) -> BTreeSet<&str>
{
    text.lines()
        .filter_map(|line| line.strip_prefix("target_feature=\"").and_then(|s| s.strip_suffix('"')))
        .collect()
}
/// Validate the compiler's entire profile expansion, not a hand-maintained
/// subset of extension names which could silently drift behind the profile.
pub fn validate_guest(target: &str, actual: &str) -> Result<()>
{
    let platform = current();
    if target != platform.rust_target
    {
        return Err(error(format!("native guest target must be {} with full {}; rebuild using \
                                  cargo xtask image",
                                 platform.rust_target, platform.llvm_profile)));
    }
    validate_profile(target, actual)
}

/// Application-role target supplied by the pinned private Rust SDK. Low-level
/// image roles continue to require the platform's explicit no_std target.
pub fn validate_std_application(target: &str, actual: &str) -> Result<()>
{
    let platform = current();
    let Some(std_target) = platform.std_target.as_deref()
    else
    {
        return Err(error(format!("platform '{}' declares no std_target, so it cannot install \
                                  applications built against the pinned System RISC SDK; add \
                                  \"std_target\" to platforms/{}.json",
                                 platform.name, platform.name)));
    };
    if target != std_target
    {
        return Err(error(format!("std application requires the pinned System RISC SDK target \
                                  {std_target}")));
    }
    validate_profile(target, actual)
}

fn validate_profile(target: &str, actual: &str) -> Result<()>
{
    let platform = current();
    let compiler = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let probe = |extra: &[&str]| -> std::io::Result<std::process::Output> {
        let mut command = Command::new(&compiler);
        command.args(["--print", "cfg", "--target", target]);
        command.args(extra);
        command.args(["-C"]).arg(format!("target-feature=+{}", platform.llvm_profile)).output()
    };
    let mut expected = probe(&[])?;
    // A platform's std target is a JSON specification this repository owns, so
    // the compiler needs the unstable switch that admits it. The switch is
    // asked for only after the plain probe says it is required, so a platform
    // pinned to a stable compiler still reaches the real error below instead of
    // being handed a flag its toolchain rejects.
    if !expected.status.success()
       && String::from_utf8_lossy(&expected.stderr).contains("`-Zunstable-options`")
    {
        expected = probe(&["-Zunstable-options"])?;
    }
    if !expected.status.success()
    {
        return Err(error(format!("compiler cannot expand the required {} profile: {}",
                                 platform.llvm_profile,
                                 String::from_utf8_lossy(&expected.stderr))));
    }
    let text = String::from_utf8(expected.stdout)?;
    let expected = features(&text);

    let required: Vec<&str> =
        core::iter::once(platform.llvm_profile.as_str()).chain(platform.profile_requires
                                                                       .iter()
                                                                       .map(String::as_str))
                                                        .filter(|name| !name.is_empty())
                                                        .collect();
    if !required.iter().all(|name| expected.contains(name))
    {
        return Err(error(format!("compiler does not implement the full {} profile ({} \
                                  required); install a compatible Rust/LLVM toolchain",
                                 platform.llvm_profile,
                                 required.join("/"))));
    }
    let actual: BTreeSet<_> = actual.split(',').filter(|name| !name.is_empty()).collect();
    let missing: Vec<_> = expected.difference(&actual).copied().collect();
    let extra: Vec<_> = actual.difference(&expected).copied().collect();
    if !missing.is_empty() || !extra.is_empty()
    {
        return Err(error(format!("native instruction profile differs from {}: missing \
                                  {missing:?}; unexpected {extra:?}; remove ISA overrides and \
                                  rebuild with cargo xtask image",
                                 platform.llvm_profile)));
    }
    Ok(())
}
#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn full_compiler_profile_is_required_and_refusals_explain_recovery()
    {
        let p = current();
        let output = Command::new("rustc").args(["--print", "cfg", "--target", &p.rust_target,
                                                 "-C"])
                                          .arg(format!("target-feature=+{}", p.llvm_profile))
                                          .output()
                                          .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        let set = features(&text);
        let actual = set.iter().copied().collect::<Vec<_>>().join(",");
        validate_guest(&p.rust_target, &actual).unwrap();
        let narrowed = set.iter()
                          .copied()
                          .filter(|&s| s != p.llvm_profile)
                          .collect::<Vec<_>>()
                          .join(",");
        let error = validate_guest(&p.rust_target, &narrowed).unwrap_err().to_string();
        assert!(error.contains(&format!("missing [\"{}\"]", p.llvm_profile))
                && error.contains("rebuild with cargo xtask image"));
        let error =
            validate_guest(&p.rust_target, &format!("{actual},zvl256b")).unwrap_err().to_string();
        assert!(error.contains("unexpected [\"zvl256b\"]"));
        let foreign = if p.rust_target == "riscv64imac-unknown-none-elf"
        {
            "riscv64gc-unknown-none-elf"
        }
        else
        {
            "riscv64imac-unknown-none-elf"
        };
        assert!(validate_guest(foreign, &actual).unwrap_err()
                                                .to_string()
                                                .contains("native guest target must be"));
    }

    /// Include composition is the schema's only inheritance mechanism, so its
    /// rules are checked rather than trusted: scalars and plain arrays replace,
    /// objects merge key by key, `$append` extends an array, a missing include
    /// and a cycle are errors that name the files involved.
    #[test]
    fn includes_merge_by_the_documented_rules()
    {
        let root = env::temp_dir().join(format!("platform-include-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("base.json"),
                  br#"{"name":"base","scalar":"base","list":[1,2],"object":{"a":1,"b":2}}"#)
            .unwrap();
        fs::write(root.join("nested/leaf.json"), br#"{"leaf":true}"#).unwrap();
        fs::write(root.join("mid.json"),
                  br#"{"include":"nested/leaf.json","object":{"c":3}}"#).unwrap();
        fs::write(root.join("over.json"),
                  br#"{"include":"mid.json","scalar":"over","list":{"$append":[3]},
                       "object":{"b":20}}"#).unwrap();
        fs::write(root.join("extends.json"),
                  br#"{"include":"base.json","list":{"$append":[3]},"object":{"b":20}}"#).unwrap();
        fs::write(root.join("cycle-a.json"), br#"{"include":"cycle-b.json"}"#).unwrap();
        fs::write(root.join("cycle-b.json"), br#"{"include":"cycle-a.json"}"#).unwrap();

        let merged = document(&root.join("extends.json"), &mut Vec::new()).unwrap();
        assert_eq!(merged["scalar"], serde_json::json!("base"), "an absent key keeps the base");
        assert_eq!(merged["list"], serde_json::json!([1, 2, 3]), "$append extends the base array");
        assert_eq!(merged["object"],
                   serde_json::json!({"a":1,"b":20}),
                   "objects merge key by key and a scalar leaf overrides");

        let recursive = document(&root.join("over.json"), &mut Vec::new()).unwrap();
        assert_eq!(recursive["scalar"], serde_json::json!("over"), "a scalar overrides");
        assert_eq!(recursive["object"],
                   serde_json::json!({"c":3,"b":20}),
                   "nested includes merge before the including record is applied");
        assert_eq!(recursive["leaf"], serde_json::json!(true), "includes resolve recursively");
        assert!(recursive.get(INCLUDE).is_none(), "the include key does not survive");

        let cycle = document(&root.join("cycle-a.json"), &mut Vec::new()).unwrap_err().to_string();
        assert!(cycle.contains("platform include cycle") && cycle.contains("cycle-a.json ->"),
                "{cycle}");
        let missing = document(&root.join("absent.json"), &mut Vec::new()).unwrap_err().to_string();
        assert!(missing.contains("cannot read platform file") && missing.contains("absent.json"),
                "{missing}");
        let dangling = root.join("dangling.json");
        fs::write(&dangling, br#"{"include":"nowhere.json"}"#).unwrap();
        let dangling = document(&dangling, &mut Vec::new()).unwrap_err().to_string();
        assert!(dangling.contains("includes 'nowhere.json'")
                && dangling.contains("cannot read platform file"),
                "{dangling}");
        fs::remove_dir_all(&root).unwrap();
    }
}


/// Board terminal failure/restart signals emitted from one checked record.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FatalShutdown
{
    device:        String,
    /// The reset/control register's offset inside the named MMIO device. A
    /// device whose finisher is not at its base declares it; zero keeps the
    /// original whole-window behaviour.
    #[serde(default)]
    offset:        u32,
    failure_value: u32,
    restart_value: u32
}
/// Firmware only halts on fatal failure; resident System also requests restart.
pub fn firmware_artifacts() -> Result<(String, String)>
{
    guest_artifacts_for(false)
}

pub fn guest_artifacts() -> Result<(String, String)>
{
    guest_artifacts_for(true)
}

fn guest_artifacts_for(include_restart: bool) -> Result<(String, String)>
{
    let (rust, mut linker) = current().device_artifacts(include_restart)?;
    linker.push_str(&current().memory()?.linker_symbols());
    linker.push_str(&current().virtual_memory()?.linker_symbols());
    Ok((rust, linker))
}
impl Platform
{
    pub fn scheduling(&self) -> Result<scheduling::Checked>
    {
        let scheduling = self.scheduling.as_ref().ok_or_else(|| missing(self, "scheduling"))?;
        // The policy caps how many harts may schedule at once, so the check
        // needs the machine's hart count. A platform that declares no emulation
        // has no emulated hart count to compare against, and a cap that cannot
        // be compared is not a cap: the number is still required to be nonzero,
        // and the record is told to declare its machine rather than have the
        // comparison silently skipped.
        let harts =
            self.emulation().map(|emulation| emulation.harts).ok_or_else(|| {
                error(format!("platform '{}' declares scheduling limits but no emulation, so \
                              scheduling.maximum_active_harts has no machine hart count to \
                              validate against",
                             self.name))
            })?;
        scheduling.check(harts)
    }

    fn device_artifacts(&self, include_restart: bool) -> Result<(String, String)>
    {
        let devices = devices::check(&self.mmio_devices, &self.memory()?)?;
        let Some(fatal_shutdown) = &self.fatal_shutdown
        else
        {
            if !devices.is_empty()
            {
                return Err(error(format!("platform '{}' declares MMIO devices but no \
                                          fatal_shutdown device",
                                         self.name)));
            }
            // A machine with no power-management device still has to compile:
            // the terminal path asks for these constants unconditionally. Zero
            // is the absence marker, matching how a record without bootstrap IO
            // reports its windows, so the finisher path can report the gap
            // instead of writing to an address that does not exist.
            let absent = "pub const FATAL_FINISHER_ADDRESS: u64 = 0;\npub const \
                          FATAL_FINISHER_FAILURE: u32 = 0;\npub const FATAL_FINISHER_RESTART: u32 = \
                          0;\n";
            return Ok((format!("{absent}{}", devices::constants(&devices)), String::new()));
        };
        let (mut rust, linker) = artifacts_for(fatal_shutdown, &devices, include_restart)?;
        rust.push_str(&devices::constants(&devices));
        Ok((rust, linker))
    }

    pub fn virtual_memory(&self) -> Result<virtual_memory::Checked>
    {
        self.virtual_memory
            .as_ref()
            .ok_or_else(|| missing(self, "virtual_memory"))?
            .check(&self.rust_target, &self.memory()?)
    }

    pub fn memory(&self) -> Result<memory::Checked>
    {
        self.memory.as_ref().ok_or_else(|| missing(self, "memory"))?.check()
    }

    /// The emulation description, if this platform declares one.
    pub fn emulation(&self) -> Option<&emulation::Definition>
    {
        self.emulation.as_ref()
    }

    /// The emulation description, or an actionable refusal. A platform that
    /// only describes hardware cannot be emulated, and no emulator is invented
    /// for it.
    pub fn require_emulation(&self) -> Result<&emulation::Definition>
    {
        self.emulation.as_ref().ok_or_else(|| {
                                   let hint = match &self.hardware
                                   {
                                       Some(hardware) =>
                                       {
                                           let medium = match &hardware.boot_image
                                           {
                                               Some(image) =>
                                               {
                                                   format!("; write the '{image}' storage image \
                                                            to it with {}",
                                                           hardware.tooling.join(", "))
                                               }
                                               None => String::new()
                                           };
                                           format!(": it targets the board '{}' and cannot be \
                                                    emulated{medium}",
                                                   hardware.board)
                                       }
                                       None => ": it declares neither emulation nor hardware".into()
                                   };
                                   error(format!("platform '{}' declares no emulation{hint}",
                                                 self.name))
                               })
    }

    /// Build-time origins are not runtime destinations. Every independently
    /// loadable definition/driver/extension shares the code-resource range; apps
    /// share the checked private Process image origin.
    pub fn link_range(&self, role: mac_formats::native::Role) -> Result<std::ops::Range<u64>>
    {
        match role
        {
            mac_formats::native::Role::System => Ok(self.virtual_memory()?.system_image_range()),
            mac_formats::native::Role::CodeResource => Ok(self.memory()?.code_link_range()),
            mac_formats::native::Role::Application =>
            {
                Ok(self.virtual_memory()?.application_image_range())
            }
        }
    }

    /// Link/execute policy differs from physical System storage. The checked
    /// virtual record derives the System extent from its physical reservation.
    pub fn validate_image(&self, role: mac_formats::native::Role, base: u64, limit: u64)
                          -> Result<()>
    {
        if role == mac_formats::native::Role::System
        {
            let expected = self.virtual_memory()?.system_image_range();
            if base != expected.start || limit != expected.end
            {
                return Err(error(format!("native System execution range {base:#x}..{limit:#x} \
                                          must match selected platform {:#x}..{:#x}; use the \
                                          platform image symbols and rebuild ROM/System \
                                          together",
                                         expected.start, expected.end)));
            }
            Ok(())
        }
        else if role == mac_formats::native::Role::CodeResource
        {
            let expected = self.link_range(role)?;
            if base != expected.start || limit != expected.end
            {
                return Err(error("code-resource link geometry differs from selected platform \
                                  policy; rebuild all modules and definitions"));
            }
            Ok(())
        }
        else
        {
            let expected = self.virtual_memory()?.application_image_range();
            if base != expected.start || limit != expected.end
            {
                return Err(error("application link geometry differs from the checked private \
                                  Process layout; rebuild with the current SDK"));
            }
            Ok(())
        }
    }
}

fn missing(platform: &Platform, section: &str) -> Box<dyn std::error::Error>
{
    error(format!("platform '{}' declares no {section} section", platform.name))
}

pub fn memory_artifacts() -> Result<(String, String)>
{
    let checked = current().memory()?;
    Ok((checked.system_constants(), checked.boot_constants()))
}
fn artifacts_for(device: &FatalShutdown, devices: &[devices::Checked], include_restart: bool)
                 -> Result<(String, String)>
{
    let hardware = devices.iter()
                          .find(|hardware| hardware.id == device.device)
                          .ok_or_else(|| {
                              error("platform fatal-shutdown device is absent from the MMIO \
                                     inventory")
                          })?;
    let address =
        hardware.address.checked_add(u64::from(device.offset)).ok_or_else(|| {
                                                                   error("platform fatal-shutdown \
                                                                          register address \
                                                                          overflows")
                                                               })?;
    if u64::from(device.offset).checked_add(4).is_none_or(|end| end > hardware.bytes)
    {
        return Err(error("platform fatal-shutdown offset must leave a u32 register inside the \
                          named MMIO window"));
    }
    if hardware.bytes < 4 || device.failure_value == 0
    {
        return Err(error("platform fatal-shutdown MMIO requires a nonzero aligned u32 register \
                          and failure value"));
    }
    if device.restart_value == 0 || device.restart_value == device.failure_value
    {
        return Err(error("platform fatal_shutdown.restart_value must be nonzero and distinct \
                          from failure_value; select the board's documented reset command"));
    }
    let mut rust = format!("// Generated from the validated platform definition.\npub const \
                            FATAL_FINISHER_ADDRESS: u64 = {address};\npub const \
                            FATAL_FINISHER_FAILURE: u32 = {};\n",
                           device.failure_value);
    if include_restart
    {
        rust.push_str(&format!("pub const FATAL_FINISHER_RESTART: u32 = {};\n",
                               device.restart_value));
    }
    let linker = format!("\nPROVIDE(__platform_fatal_finisher = \
                          0x{address:x});\nPROVIDE(__platform_fatal_failure = \
                          0x{:x});\nPROVIDE(__platform_fatal_restart = 0x{:x});\n",
                         device.failure_value, device.restart_value);
    Ok((rust, linker))
}

#[cfg(test)]
mod artifact_tests
{
    use super::*;
    #[test]
    fn named_platform_generates_one_consistent_device_record_and_rejects_bad_selection()
    {
        let platform = load("qemu_riscv").unwrap();
        let (rust, linker) = platform.device_artifacts(true).unwrap();
        let address =
            devices::check(&platform.mmio_devices, &platform.memory().unwrap()).unwrap()[0].address;
        assert!(rust.contains(&format!("FATAL_FINISHER_ADDRESS: u64 = {address}")));
        assert!(linker.contains(&format!("__platform_fatal_finisher = 0x{address:x}")));
        assert!(rust.contains(&format!("FATAL_FINISHER_FAILURE: u32 = {}",
                                       platform.fatal_shutdown.as_ref().unwrap().failure_value)));
        assert!(linker.contains(&format!("__platform_fatal_failure = 0x{:x}",
                                         platform.fatal_shutdown.as_ref().unwrap().failure_value)));
        assert_eq!(platform.fatal_shutdown.as_ref().unwrap().restart_value, 0x7777);
        assert!(rust.contains("FATAL_FINISHER_RESTART: u32 = 30583;"));
        assert!(linker.contains("__platform_fatal_restart = 0x7777"));
        for name in ["../qemu_riscv", "qemu_riscv/other", ""]
        {
            assert!(load(name).err().unwrap().to_string().contains("platform name must contain"));
        }
        let error = load("NoSuchBoard").err().unwrap().to_string();
        assert!(error.contains("cannot select platform 'NoSuchBoard'")
                && error.contains("known platforms: qemu_arm64, qemu_riscv, raspi3b"));
        for text in ["0x0", "0x3", "0xffffffffffffffff", "0xgarbage", "1234", "0x80000000"]
        {
            let mut bad = load("qemu_riscv").unwrap();
            bad.mmio_devices[0].address = text.into();
            assert!(bad.device_artifacts(true)
                       .err()
                       .unwrap()
                       .to_string()
                       .contains("platform MMIO"));
        }
        let mut bad = load("qemu_riscv").unwrap();
        bad.fatal_shutdown.as_mut().unwrap().failure_value = 0;
        assert!(bad.device_artifacts(true).is_err());
        bad.fatal_shutdown.as_mut().unwrap().failure_value = 1;
        bad.fatal_shutdown.as_mut().unwrap().device = "absent".into();
        assert!(bad.device_artifacts(true).err().unwrap().to_string().contains("absent"));
        let mut bad = load("qemu_riscv").unwrap();
        bad.mmio_devices.push(bad.mmio_devices[0].clone());
        assert!(bad.device_artifacts(true).err().unwrap().to_string().contains("non-overlapping"));
    }
    #[test]
    fn a_peripheral_window_and_the_page_a_driver_claims_cannot_describe_different_devices()
    {
        let platform = load("raspi3b").unwrap();
        platform.check_peripheral_windows().unwrap();
        // The mailbox registers start part way into the ARM control page the
        // record claims, so one device is declared at two granularities on
        // purpose: the window in the tree, the page in the inventory.
        let mailbox = platform.declared_devices()
                              .unwrap()
                              .into_iter()
                              .find(|device| device.compatible == "bcm2835-mailbox")
                              .expect("the record declares the mailbox window");
        assert_eq!((mailbox.base, mailbox.bytes), (0x3f00b880, 0x40));
        // A window moved out of the page that claims it is refused.
        let mut drifted = load("raspi3b").unwrap();
        drifted.hardware
               .as_mut()
               .unwrap()
               .devices
               .iter_mut()
               .find(|device| device.model == "bcm2835-mailbox")
               .unwrap()
               .base = Some("0x3f00c880".into());
        let error = drifted.check_peripheral_windows().unwrap_err().to_string();
        assert!(error.contains("bcm2835-mailbox"), "{error}");
        assert!(error.contains("outside every declared MMIO range"), "{error}");
        // A claimed page moved away from the window it is supposed to hold is
        // refused in the same way.
        let mut drifted = load("raspi3b").unwrap();
        drifted.mmio_devices
               .iter_mut()
               .find(|device| device.id == "bcm2835-mbox")
               .unwrap()
               .address = "0x3f00c000".into();
        let error = drifted.check_peripheral_windows().unwrap_err().to_string();
        assert!(error.contains("outside every declared MMIO range"), "{error}");
        // Two windows in one claimed page would make a single claim cover two
        // devices, which is the same defect in the other direction.
        let mut crowded = load("raspi3b").unwrap();
        crowded.hardware
               .as_mut()
               .unwrap()
               .devices
               .iter_mut()
               .find(|device| device.model == "pl011")
               .unwrap()
               .base = Some("0x3f00b000".into());
        let error = crowded.check_peripheral_windows().unwrap_err().to_string();
        assert!(error.contains("contains 2 declared peripheral windows"), "{error}");
    }

    #[test]
    fn emergency_restart_is_required_checked_and_generated_from_the_selected_record()
    {
        for restart in [0, 78643]
        {
            let mut platform = load("qemu_riscv").unwrap();
            platform.fatal_shutdown.as_mut().unwrap().restart_value = restart;
            let error = platform.device_artifacts(true).unwrap_err().to_string();
            assert!(error.contains("fatal_shutdown.restart_value"), "{error}");
            assert!(error.contains("nonzero and distinct from failure_value"), "{error}");
            assert!(error.contains("board's documented reset command"), "{error}");
        }
        let mut platform = load("qemu_riscv").unwrap();
        platform.fatal_shutdown.as_mut().unwrap().restart_value = 0x1234;
        let (rust, linker) = platform.device_artifacts(true).unwrap();
        assert!(rust.contains("FATAL_FINISHER_RESTART: u32 = 4660;"));
        assert!(linker.contains("__platform_fatal_restart = 0x1234"));
        let error = serde_json::from_str::<FatalShutdown>(r#"{"device":"sifive-test","failure_value":78643}"#)
            .err().unwrap().to_string();
        assert!(error.contains("missing field `restart_value`"), "{error}");
    }
}

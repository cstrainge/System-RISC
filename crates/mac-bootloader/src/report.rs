//! The conditions every bootstrap reports, in one vocabulary.
//!
//! Two bootstraps answer the same questions - was there a blessed folder, was the
//! startup file in it, did the volume carry a native System - and those answers
//! mean the same thing on every machine. A board has no opinion about whether it
//! found a System, so the words are not per-machine and must not drift into
//! per-machine spelling: a gate that has to know two vocabularies cannot compare
//! two boots.
//!
//! This is the shared half. Stage markers stay with the architecture that owns
//! the stage - `BOOT_AARCH64_MMU_READY` is a fact about one instruction set - and
//! are not defined here. The line is the same one the rest of the lane draws:
//! shared outcomes get shared words, mechanism keeps its own names.

use alloc::string::String;

/// The banner a bootstrap prints once, followed by its own entry address.
///
/// The address is the bootloader's, not the condition's, so it is appended rather
/// than built in: the same words have to come out of a ROM at 0x200 and one at
/// 0x40080000.
pub const BANNER: &str = "System RISC Boot ROM v1 at 0x";

/// A native volume was found and its blessed folder admitted.
pub fn blessed_folder(name: &str, cnid: u32) -> String
{
    alloc::format!("{BLESSED_FOLDER_PREFIX}{name}' CNID={cnid}")
}

/// The part of [`blessed_folder`] that does not depend on the machine's folder
/// name, so a check can assert the condition without hardcoding a volume's
/// contents.
pub const BLESSED_FOLDER_PREFIX: &str = "HFS-B: blessed folder '";

/// No disk carried a bootable native System.
pub fn no_system(disks: usize) -> String
{
    alloc::format!("{NO_SYSTEM_PREFIX}searched {disks} disk(s); no bootable native System found")
}

/// The part of [`no_system`] that does not depend on how many disks were tried.
pub const NO_SYSTEM_PREFIX: &str = "BOOT_NO_SYSTEM: ";

/// A candidate startup was refused, and the cause follows.
///
/// One volume failing to yield a bootable System is one fact, however it failed:
/// a blessed folder that is not there, a startup file that is not there, a
/// native image the shared reader will not decode, or a record the kernel's own
/// admission checks reject. Each bootstrap states that fact with this prefix and
/// the reason the shared stack produced, and the terminal [`no_system`] line
/// says that no candidate worked, so a refusal is reported once in a vocabulary
/// the gate can read on either machine.
pub const REFUSED_PREFIX: &str = "BOOT_REFUSED: ";

/// A refusal in the shared words, with the cause that follows.
pub fn refused(cause: impl core::fmt::Display) -> String
{
    alloc::format!("{REFUSED_PREFIX}{cause}")
}

/// The volume carried a native System that states no ABI resource.
pub const NO_NATIVE_ABI: &str = "System has no native ABI resource";

/// The volume carried a native System that states no code resource.
pub const NO_CODE_RESOURCE: &str = "System has no native code resource";

/// The volume carried a System this build cannot start.
pub const NOT_COMPATIBLE_SYSTEM: &str = "startup executable is not a compatible System";

/// A volume was found but is not in the format native boot requires.
///
/// Not a per-machine judgement: the answer is the same whichever medium carried
/// it, which is why it is stated once here rather than phrased per bootloader.
pub const NOT_NATIVE_VOLUME: &str = "native boot requires HFS-B; recreate System disk";

/// The bootstrap is about to branch into the loaded native System.
///
/// Also not a per-machine judgement: whether control reached the kernel is the
/// same fact on every board, and a gate that looked for one board's spelling
/// would report a boot that reached the desktop as a ROM failure. Each
/// bootstrap appends its own entry address and privilege wording after this
/// prefix.
pub const ENTERING_SYSTEM: &str = "BOOT: entering System";

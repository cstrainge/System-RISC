//! Preferences classify names; drivers independently own open actions/icons.
#![forbid(unsafe_code)]
use mac_file_types::Registry;
use spinning_top::Spinlock;

#[cfg(target_arch = "riscv64")]
use super::*;
static REGISTRY: Spinlock<Option<Registry>> = Spinlock::new(None);
pub(crate) fn classify(name: &str, file_type: [u8; 4], creator: [u8; 4]) -> ([u8; 4], [u8; 4])
{
    REGISTRY.lock().as_ref().map_or((file_type, creator), |r| r.classify(name, file_type, creator))
}
#[cfg(target_arch = "riscv64")]
pub(crate) fn initialize() -> Result<(), Status>
{
    let bytes=crate::kernel::with_files(|m|m.startup_preferences("File Types"))
        .map_err(crate::state::AccessError::status)??;
    let registry = Registry::decode(&bytes).map_err(|_| CORRUPT_DATA)?;
    *REGISTRY.lock() = Some(registry);
    Ok(())
}

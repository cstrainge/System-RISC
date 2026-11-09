//! The execution window a platform reserves for the resident System.
//!
//! System is linked for one address: the virtual window the platform record
//! declares. A bootstrap that entered a System built for another window would
//! hand the kernel a record and a code image that disagree, so the descriptor is
//! checked against the record before any of the image is read. Both numbers are
//! the caller's platform's - a board has no opinion about where another board
//! puts its kernel - so the caller states the window and this module states the
//! refusal, once, in the words every bootstrap uses.

use mac_formats::native::Descriptor;

/// The virtual window a platform reserves for the resident System.
pub struct Window
{
    /// The virtual address System is linked to run at.
    pub base:  u64,
    /// The exclusive end of that window.
    pub limit: u64
}

/// Refuses a descriptor that does not occupy exactly `window`.
pub fn system(descriptor: &Descriptor, window: Window) -> Result<(), mac_formats::Error>
{
    if descriptor.load_base != window.base || descriptor.load_limit != window.limit
    {
        return Err(mac_formats::Error::Malformed(crate::report::SYSTEM_RESERVATION_MISMATCH));
    }
    Ok(())
}

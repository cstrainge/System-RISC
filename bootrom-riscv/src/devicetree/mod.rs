//! RISC-V device tree consumers.
//!
//! The reader itself is `mac-devicetree`, shared with the other bootloaders. What
//! remains here is this platform's own interpretation: monitor route admission,
//! which derives a CLINT `msip` cell per hart and is RISC-V mechanism the shared
//! reader has no business knowing.

pub use mac_devicetree::Tree;
pub use mac_formats::Error;

pub(crate) type Result<T> = core::result::Result<T, Error>;

/// Maps a reader refusal onto this bootloader's error.
///
/// The cause is kept rather than flattened: "the blob is not a device tree" and
/// "its blocks overlap" are different defects, and a boot log that says only
/// "invalid device tree" wastes the one chance to report which.
pub fn envelope(cause: mac_devicetree::Error) -> Error
{
    Error::Malformed(match cause
    {
        mac_devicetree::Error::Magic => "device tree has wrong magic",
        mac_devicetree::Error::Size => "device tree total size is invalid",
        mac_devicetree::Error::Version => "device tree version is unsupported",
        mac_devicetree::Error::Bounds => "device tree block lies outside the blob",
        mac_devicetree::Error::Alignment => "device tree block is not aligned",
        mac_devicetree::Error::Overlap => "device tree blocks overlap",
        mac_devicetree::Error::Reservation => "device tree reservation overflows",
        mac_devicetree::Error::Structure => "device tree structure is malformed",
        mac_devicetree::Error::Strings => "device tree string offset is invalid",
        mac_devicetree::Error::Cells => "device tree cell declaration is invalid",
        mac_devicetree::Error::Cpu => "device tree CPU node is not admissible",
        mac_devicetree::Error::Ranges => "device tree address translation is not readable",
        mac_devicetree::Error::DynamicReservation =>
        {
            "device tree reserved-memory asks for dynamic allocation"
        }
    })
}

pub mod monitor;

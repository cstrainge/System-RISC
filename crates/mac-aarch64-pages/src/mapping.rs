//! The AArch64 binding of the shared mapping walk.
//!
//! Same walk, this architecture's descriptor and layout.
pub use mac_kernel_model::pages::mapping::{Mapper, Tables};

use crate::{Entry, Error};

/// This architecture's layout, so the walk asks it for geometry and indices.
pub struct Layout;

impl mac_kernel_model::pages::PageLayout for Layout
{
    fn geometry() -> mac_kernel_model::pages::PageGeometry
    {
        crate::geometry()
    }

    fn leaf_sizes() -> &'static [u64]
    {
        crate::leaf_sizes()
    }

    fn valid_virtual_range(range: mac_kernel_model::policy::AddressRange) -> bool
    {
        crate::valid_virtual_range(range)
    }

    fn lower_half_end() -> u64
    {
        0x0000_0080_0000_0000
    }

    fn upper_half_start() -> u64
    {
        0xffff_ff80_0000_0000
    }
}

/// This architecture's encoding error is the one the walk reports.
pub type MapError<E> = mac_kernel_model::pages::mapping::MapError<E, Error>;

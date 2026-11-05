//! RISC-V page-table format, as the architecture's contribution to the shared
//! paging module.
//!
//! The shared paging code builds and admits address spaces without naming a
//! page format: it asks this module for the geometry, the leaves, the virtual
//! range rule and the admission check. Sv39's mode field, tag field and
//! physical-page-number width are stated once, here, and nowhere above the
//! selection point.

use mac_kernel_model::pages::{PageGeometry, PageLayout};
use mac_kernel_model::policy::AddressRange;

pub(crate) use mac_riscv_pages::Sv39;

/// Sv39 is mode 8 in the satp mode field, the tag is 16 bits wide, and the
/// physical page number occupies the low 44 bits.
const MODE: usize = 8;
const MODE_SHIFT: u32 = 60;
const TAG_SHIFT: u32 = 44;
const TAG_MASK: usize = (1 << 16) - 1;
const PPN_MASK: usize = (1 << 44) - 1;

pub(crate) fn geometry() -> PageGeometry
{
    Sv39::geometry()
}

pub(crate) fn leaf_sizes() -> &'static [u64]
{
    Sv39::leaf_sizes()
}

pub(crate) fn valid_virtual_range(range: AddressRange) -> bool
{
    Sv39::valid_virtual_range(range)
}

/// The name the admission diagnostic reports, so a refused root says which
/// format it was measured against.
pub(crate) const NAME: &str = "sv39";

/// Whether `root` is the satp value this architecture expects for the record's
/// root page: the Sv39 mode field, no address-space tag, and the physical page
/// number the record names. A stale tag or a foreign mode is refused rather
/// than masked away.
pub(crate) fn validate_root(root: usize, expected_physical: u64) -> bool
{
    root >> MODE_SHIFT == MODE
    && (root >> TAG_SHIFT) & TAG_MASK == 0
    && root & PPN_MASK == expected_physical as usize >> 12
}

/// The register value that selects `physical` as the root table under `tag`.
/// The mode field, the tag shift and the page-number width are stated once,
/// here, so no caller re-derives an Sv39 encoding.
pub(crate) fn encode_root(physical: u64, tag: u64) -> usize
{
    (MODE << MODE_SHIFT) | ((tag as usize & TAG_MASK) << TAG_SHIFT)
    | (physical as usize >> 12 & PPN_MASK)
}

/// The admission check requires interrupts to be off, so that no handler can
/// observe a half-published address space. sstatus.SIE is bit 1.
pub(crate) fn admission_interrupts_clear(status: usize) -> bool
{
    status & 2 == 0
}

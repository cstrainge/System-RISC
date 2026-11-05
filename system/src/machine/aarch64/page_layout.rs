//! AArch64 page-table format, as the architecture's contribution to the shared
//! paging module.
//!
//! A 39-bit canonical address space with a 4 KiB granule is a *three*-level
//! walk: with TCR_EL1.T0SZ = 25 the level-1 table covers 1 GiB per entry, so the
//! leaves are the 1 GiB block, the 2 MiB block and the 4 KiB page. The shared
//! paging code asks this module for that geometry instead of assuming the
//! four-level shape a wider VA would have.

use mac_kernel_model::pages::{PageGeometry, PageLayout};
use mac_kernel_model::policy::AddressRange;

/// The base page. AArch64 also permits 16 KiB and 64 KiB granules; the platform
/// record fixes 4 KiB for this port, which is what the geometry below encodes.
const PAGE_BYTES: u64 = 4096;

/// 1 GiB, 2 MiB and 4 KiB: the level-1, level-2 and level-3 leaves of a 39-bit
/// walk at a 4 KiB granule.
const LEAF_BYTES: &[u64] = &[1 << 30, 1 << 21, 1 << 12];

/// The top of the lower half. A 39-bit space sign-extends from bit 38.
const LOWER_END: u64 = 0x0000_0080_0000_0000;
/// The bottom of the upper half.
const UPPER_START: u64 = 0xffff_ff80_0000_0000;

pub(crate) fn geometry() -> PageGeometry
{
    PageGeometry::new(PAGE_BYTES).expect("the aarch64 base page geometry is 4096 bytes")
}

pub(crate) fn leaf_sizes() -> &'static [u64]
{
    LEAF_BYTES
}

/// A range is valid when it lies wholly inside one canonical half. A range that
/// straddles the hole between them has no single valid encoding.
pub(crate) fn valid_virtual_range(range: AddressRange) -> bool
{
    let (start, end) = (range.start(), range.end());
    (start < LOWER_END && end <= LOWER_END) || start >= UPPER_START
}

/// The name the admission diagnostic reports, so a refused root says which
/// format it was measured against.
pub(crate) const NAME: &str = "aarch64-39";

/// Whether `root` is the TTBR0_EL1 value this architecture expects for the
/// record's root page.
///
/// TTBR0_EL1 carries the root table's physical base address in bits 47:1 and
/// the address-space tag in bits 63:48. There is no mode field: the translation
/// regime lives in TCR_EL1. Admission therefore requires the untagged root the
/// record names, so a stale tag or an unaligned base is refused rather than
/// masked away.
pub(crate) fn validate_root(root: usize, expected_physical: u64) -> bool
{
    root >> 48 == 0 && root & (PAGE_BYTES as usize - 1) == 0 && root as u64 == expected_physical
}

/// The register value that selects `physical` as the root table under `tag`.
/// TTBR0_EL1 has no mode field — the translation regime lives in TCR_EL1 — so
/// the tag is the only thing layered above the base address.
pub(crate) fn encode_root(physical: u64, tag: u64) -> usize
{
    physical as usize | ((tag as usize & 0xffff) << 48)
}

/// The admission check requires interrupts to be off, so that no handler can
/// observe a half-published address space. SPSR_EL1 bit 7 is PSTATE.I, which is
/// set when IRQ is masked.
pub(crate) fn admission_interrupts_clear(status: usize) -> bool
{
    status & (1 << 7) != 0
}

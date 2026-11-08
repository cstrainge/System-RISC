//! AArch64 page-table format, as the architecture's contribution to the shared
//! paging module.
//!
//! A 39-bit canonical address space with a 4 KiB granule is a *three*-level
//! walk: with TCR_EL1.T0SZ = 25 the level-1 table covers 1 GiB per entry, so the
//! leaves are the 1 GiB block, the 2 MiB block and the 4 KiB page. The shared
//! paging code asks this module for that geometry instead of assuming the
//! four-level shape a wider VA would have.

use mac_kernel_model::pages::{PageGeometry, PageLayout};

/// The encoder types shared paging stores and decodes, provided by this
/// architecture's descriptor encoder.
pub(crate) use mac_aarch64_pages::{Decoded, Entry, LeafSize};
pub(crate) use mac_aarch64_pages::mapping;
pub(crate) use mac_aarch64_pages::walk;
use mac_kernel_model::policy::AddressRange;

/// The base page. AArch64 also permits 16 KiB and 64 KiB granules; the platform
/// record fixes 4 KiB for this port, which is what the geometry below encodes.
const PAGE_BYTES: u64 = 4096;

/// The top of the lower half. A 39-bit space sign-extends from bit 38.
const LOWER_END: u64 = 0x0000_0080_0000_0000;
/// The bottom of the upper half.
const UPPER_START: u64 = 0xffff_ff80_0000_0000;

pub(crate) fn geometry() -> PageGeometry
{
    mac_aarch64_pages::geometry()
}

pub(crate) fn leaf_sizes() -> &'static [u64]
{
    mac_aarch64_pages::leaf_sizes()
}

/// A range is valid when it lies wholly inside one canonical half.
pub(crate) fn valid_virtual_range(range: AddressRange) -> bool
{
    mac_aarch64_pages::valid_virtual_range(range)
}

pub(crate) fn lower_half_end() -> u64
{
    0x0000_0080_0000_0000
}

pub(crate) fn upper_half_start() -> u64
{
    0xffff_ff80_0000_0000
}

/// The name the admission diagnostic reports, so a refused root says which
/// format it was measured against.
pub(crate) const NAME: &str = "aarch64-39";

/// This format as the shared `PageLayout` trait, so the geometry arithmetic is
/// reached through one interface rather than re-derived here.
struct Aarch64Layout;

impl mac_kernel_model::pages::PageLayout for Aarch64Layout
{
    fn geometry() -> mac_kernel_model::pages::PageGeometry
    {
        geometry()
    }

    fn leaf_sizes() -> &'static [u64]
    {
        leaf_sizes()
    }

    fn valid_virtual_range(range: AddressRange) -> bool
    {
        valid_virtual_range(range)
    }

    fn lower_half_end() -> u64
    {
        lower_half_end()
    }

    fn upper_half_start() -> u64
    {
        upper_half_start()
    }
}

/// The encoder's failure type, which is this architecture's descriptor error.
pub(crate) use mac_aarch64_pages::Error as EncodingError;

/// The table index one address selects at `level`, counting from the finest
/// leaf. The arithmetic is shared geometry; only the level-to-leaf mapping is
/// this format's.
///
/// `level` counts from the finest leaf, which is the shared convention and the
/// one every caller uses. `leaf_sizes` is written largest-first for this format
/// because the mapping plan splits a span by taking the largest leaf that fits,
/// so indexing that list with `level` would pick the wrong leaf — and did: the
/// root slot was written at the 4 KiB index and the leaves at the 1 GiB index,
/// so every dynamically mapped definition was published under an entry the
/// walk never reads. `leaf_at` derives the leaf from the geometry for exactly
/// this reason, and it is the one statement of the mapping.
pub(crate) fn index(address: u64, level: usize) -> Result<usize, EncodingError>
{
    use mac_kernel_model::pages::PageLayout;
    let leaf = <Aarch64Layout as PageLayout>::leaf_at(level)
        .ok_or(EncodingError::Level { level: level as u8 })?;
    let range = AddressRange::new(address, address.saturating_add(1))
        .map_err(|_| EncodingError::NonCanonical { address })?;
    if !Aarch64Layout::valid_virtual_range(range)
    {
        return Err(EncodingError::NonCanonical { address });
    }
    <Aarch64Layout as PageLayout>::index(address, leaf)
        .ok_or(EncodingError::Level { level: level as u8 })
}

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

/// The register values that select `physical` as the root table under `tag`,
/// as the primary and secondary root of this layout.
///
/// AArch64 addresses two halves of the address space through two base
/// registers, so this layout genuinely has two roots and a single value could
/// not express it. TTBR0_EL1 has no mode field — the translation regime lives
/// in TCR_EL1 — so the tag is the only thing layered above the base address,
/// and it belongs to the primary. The secondary selects the same physical root
/// for the higher half, untagged, because the physmap and the kernel half are
/// reached through TTBR1_EL1 and a tag there would have to be re-derived at
/// every switch.
pub(crate) fn encode_roots(physical: u64, tag: u64) -> (usize, usize)
{
    (physical as usize | ((tag as usize & 0xffff) << 48), physical as usize)
}

/// The admission check requires interrupts to be off, so that no handler can
/// observe a half-published address space. SPSR_EL1 bit 7 is PSTATE.I, which is
/// set when IRQ is masked.
pub(crate) fn admission_interrupts_clear(status: usize) -> bool
{
    status & (1 << 7) != 0
}

/// Whether `status` reports interrupts enabled.
///
/// The status word is the architecture's own: RISC-V's `sstatus` carries SIE in
/// bit 1, and AArch64's `DAIF` carries the IRQ mask in bit 7 with the opposite
/// sense. Shared code asks this question rather than testing a bit, so the bit
/// cannot be read with the wrong architecture's meaning.
pub(crate) fn interrupts_enabled(status: usize) -> bool
{
    status & (1 << 7) == 0
}

/// The ISA profile this kernel requires the bootstrap to have sealed.
///
/// Stated here, in the architecture adapter, rather than taken from the shared
/// ABI: the shared constant that used to hold it was RISC-V MISA bits, which no
/// other architecture can satisfy. The bootstrap seals its own statement of the
/// same profile, so this comparison is a check rather than a formality - if the
/// two disagree, the boot is refused.
///
/// AArch64 has no MISA. These bits state what this kernel's code actually
/// assumes: EL1 with a 4 KiB granule, the floating-point and NEON unit (the
/// compiler emits vector copies), and the generic timer as the clock source.
pub(crate) const REQUIRED_ISA: u64 = (1 << 0) | (1 << 1) | (1 << 2);

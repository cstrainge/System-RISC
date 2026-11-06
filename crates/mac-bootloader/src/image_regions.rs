//! The mapping record System is admitted with.
//!
//! The native image format states each segment's offset, length and access
//! flags; `BootInfo::execution` states the same thing in the wire form the kernel
//! validates. This is the one conversion between them, shared by every
//! bootstrap, because a second board that derived the record itself could round
//! an extent differently and produce a mapping the kernel accepts but does not
//! intend.
//!
//! Nothing here is architecture-specific: an offset into the image is the same
//! number on any machine, and the page size comes from the record rather than
//! from a granule this code assumes.

use alloc::vec::Vec;

use mac_abi::{BootMemoryRange,
              boot::execution::{EXECUTE, READ, Region, WRITE}};
use mac_formats::{Error as FormatError, native::image::Segment};

/// The mapping record for `segments`, loaded at `physical_base` for the window
/// `load_base..load_limit`.
///
/// The caller has already established that the descriptor's role is System; that
/// is a property of the container this is called for, not of a segment list.
///
/// Every segment must be page aligned, non-overlapping, ascending, inside the
/// declared window, and carry only the three access bits. A segment that is both
/// readable and executable is refused rather than mapped, because
/// writable-executable is a property the kernel's policy then has to undo.
pub fn system_regions(segments: &[Segment], load_base: u64, load_limit: u64, physical_base: u64,
                      page: u64)
                      -> Result<Vec<Region>, FormatError>
{
    let invalid = || {
        FormatError::Malformed("System mapping segments must be page aligned, nonoverlapping and \
                                inside the native reservation")
    };
    if !page.is_power_of_two() || page < 4096 || !physical_base.is_multiple_of(page)
    {
        return Err(invalid());
    }
    let extent = load_limit.checked_sub(load_base).ok_or_else(invalid)?;
    if extent == 0 || !extent.is_multiple_of(page) || !load_base.is_multiple_of(page)
       || physical_base.checked_add(extent).is_none()
    {
        return Err(invalid());
    }
    let mut regions = Vec::new();
    regions.try_reserve_exact(segments.len()).map_err(|_| {
        FormatError::AllocationFailed("System mapping record allocation failed")
    })?;
    let mut last = 0;
    for segment in segments
    {
        let end = segment.offset
                         .checked_add(segment.memory_length)
                         .and_then(|end| end.checked_add(page - 1))
                         .map(|end| end / page * page)
                         .ok_or_else(invalid)?;
        if segment.memory_length == 0
           || segment.length as u64 > segment.memory_length
           || segment.flags & !7 != 0
           || segment.flags & 3 == 3
           || !segment.offset.is_multiple_of(page)
           || segment.offset < last
           || end > extent
           || end <= segment.offset
        {
            return Err(invalid());
        }
        let flags = (if segment.flags & 4 != 0 { READ } else { 0 })
                    | (if segment.flags & 2 != 0 { WRITE } else { 0 })
                    | (if segment.flags & 1 != 0 { EXECUTE } else { 0 });
        regions.push(Region { physical:
                                  BootMemoryRange { base:
                                                        physical_base.checked_add(segment.offset)
                                                                     .ok_or_else(invalid)?,
                                                    length: end - segment.offset },
                              virtual_base: load_base.checked_add(segment.offset)
                                                      .ok_or_else(invalid)?,
                              flags,
                              reserved: 0 });
        last = end;
    }
    Ok(regions)
}

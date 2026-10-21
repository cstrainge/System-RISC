//! Current QEMU virt board's reserved RAM map. Firmware's pinned storage heap
//! remains live at870..878; QEMU's FDT reserves the high2MiB. Native modules use
//! only the6MiB gap between those regions, never app or definition reservations.
pub(crate) fn module_arena(ram_end: u64) -> Result<core::ops::Range<u64>, mac_abi::Status>
{
    let end = ram_end.checked_sub(2 * 1024 * 1024).ok_or(mac_abi::NO_MEMORY)?.min(0x87e00000);
    if end <= 0x87800000
    {
        return Err(mac_abi::NO_MEMORY);
    }
    Ok(0x87800000..end)
}

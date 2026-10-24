//! Build-defined placement policy; discovered RAM is supplied by the bootstrap.
include!(concat!(env!("OUT_DIR"), "/platform-memory.rs"));

pub(crate) fn module_arena(ram_end: u64) -> Result<core::ops::Range<u64>, mac_abi::Status>
{
    let usable_end = ram_end.checked_sub(FDT_TAIL_BYTES).ok_or(mac_abi::NO_MEMORY)?;
    if MODULE_LIMIT > usable_end
    {
        return Err(mac_abi::NO_MEMORY);
    }
    Ok(MODULE_BASE..MODULE_LIMIT)
}

/// # Safety
/// The trusted bootstrap lends valid arrays for the whole System lifetime.
/// This boundary checks lengths before creating slices; all extent validation
/// then runs in shared safe code, before any reservation is used.
pub(crate) unsafe fn validate_boot(info: &mac_abi::BootInfo)
                                   -> Result<(), mac_abi::boot::MemoryError>
{
    let valid = |pointer: *const mac_abi::BootMemoryRange, count: usize| {
        count <= isize::MAX as usize / core::mem::size_of::<mac_abi::BootMemoryRange>()
        && (count == 0 || (!pointer.is_null() && (pointer as usize).is_multiple_of(core::mem::align_of::<mac_abi::BootMemoryRange>())))
    };
    if !valid(info.ram_regions, info.ram_region_count)
       || !valid(info.reservations, info.reservation_count)
    {
        return Err(mac_abi::boot::MemoryError::InvalidArray);
    }
    let ram = if info.ram_region_count == 0
    {
        &[]
    }
    else
    {
        unsafe { core::slice::from_raw_parts(info.ram_regions, info.ram_region_count) }
    };
    let reservations = if info.reservation_count == 0
    {
        &[]
    }
    else
    {
        unsafe { core::slice::from_raw_parts(info.reservations, info.reservation_count) }
    };
    mac_abi::boot::Memory { ram,
                            reservations }.validate(PAGE_BYTES,
                                                    mac_abi::BootMemoryRange { base:
                                                                                   info.ram_base,
                                                                               length:
                                                                                   info.ram_size })
}

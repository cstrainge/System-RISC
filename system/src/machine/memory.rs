//! Build-defined placement policy; discovered RAM is supplied by the bootstrap.
include!(concat!(env!("OUT_DIR"), "/platform-memory.rs"));
include!(concat!(env!("OUT_DIR"), "/platform-virtual.rs"));

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
pub(crate) unsafe fn validate_boot(
    info: &mac_abi::BootInfo)
    -> Result<mac_abi::boot::CheckedMemory<'static>, mac_abi::boot::MemoryError>
{
    let ram = unsafe { primary_slice(info, info.ram_regions, info.ram_region_count) }?;
    let reservations = unsafe { primary_slice(info, info.reservations, info.reservation_count) }?;
    let memory = mac_abi::boot::Memory { ram,
                                         reservations };
    let primary = mac_abi::BootMemoryRange { base:   info.ram_base,
                                             length: info.ram_size };
    let mut memory = if info.flags & mac_abi::BOOT_GRAPHICAL != 0
    {
        memory.display_handoff(PAGE_BYTES,
                               primary,
                               info.display_scratch,
                               mac_abi::BootMemoryRange { base:   DISPLAY_SCRATCH_BASE,
                                                          length: DISPLAY_SCRATCH_BYTES })?
    }
    else
    {
        memory.checked(PAGE_BYTES, primary)?
    };
    memory = memory.exclude_mmio(mac_abi::BootMemoryRange { base:   FLASH_BASE,
                                                            length: FLASH_BYTES })?;
    for &(base, limit) in BOOT_MMIO_REGIONS
    {
        memory = memory.exclude_mmio(mac_abi::BootMemoryRange { base,
                                                                length: limit - base })?;
    }
    for &(_, _, base, length, _, _) in super::power::platform::PLATFORM_MMIO_DEVICES
    {
        memory = memory.exclude_mmio(mac_abi::BootMemoryRange { base,
                                                                length })?;
    }
    // Every entry mode protects the same build-defined firmware/platform
    // owners before a pool can allocate. Graphical only adds the scratch loan.
    for (base, end) in BOOT_RESERVED.into_iter().chain([(IMAGE_BASE, IMAGE_LIMIT)])
    {
        memory = memory.require_reservation(mac_abi::BootMemoryRange { base,
                                                                       length: end - base })?;
    }
    let cpu = core::ptr::from_ref(info) as u64;
    let info_physical = cpu.checked_sub(VM_PHYSMAP_START)
                           .and_then(|offset| VM_PHYSMAP_PHYSICAL_BASE.checked_add(offset))
                           .filter(|_| {
                               cpu.checked_add(size_of::<mac_abi::BootInfo>() as u64)
                                  .is_some_and(|end| end <= VM_PHYSMAP_END)
                           })
                           .ok_or(mac_abi::boot::MemoryError::InvalidArray)?;
    let execution = unsafe { execution(info) }?;
    for (physical, bytes) in
        [(info_physical, size_of::<mac_abi::BootInfo>()),
         (info.ram_regions as u64, info.ram_region_count * size_of::<mac_abi::BootMemoryRange>()),
         (info.reservations as u64, info.reservation_count * size_of::<mac_abi::BootMemoryRange>()),
         (info.execution as u64, size_of::<mac_abi::boot::execution::Execution>()),
         (info.cpus as u64,
          info.cpu_count
              .checked_mul(size_of::<mac_abi::boot::topology::Cpu>())
              .ok_or(mac_abi::boot::MemoryError::InvalidArray)?),
         (execution.regions as u64,
          execution.region_count
                   .checked_mul(size_of::<mac_abi::boot::execution::Region>())
                   .ok_or(mac_abi::boot::MemoryError::InvalidArray)?)]
    {
        if bytes == 0
        {
            continue;
        }
        let start = physical / PAGE_BYTES * PAGE_BYTES;
        let end = physical.checked_add(bytes as u64)
                          .and_then(|end| end.checked_add(PAGE_BYTES - 1))
                          .map(|end| end / PAGE_BYTES * PAGE_BYTES)
                          .ok_or(mac_abi::boot::MemoryError::InvalidArray)?;
        memory = memory.require_reservation(mac_abi::BootMemoryRange { base:   start,
                                                                       length: end - start })?;
    }
    Ok(memory)
}

/// # Safety
/// The admitted firmware root maps primary RAM in the permanent physmap; all
/// borrowed wire storage remains pinned until reset. Physical encodings are
/// checked before creating new mapped views. Existing references never move.
pub(crate) unsafe fn primary_slice<T>(info: &mac_abi::BootInfo, pointer: *const T, count: usize)
                                      -> Result<&'static [T], mac_abi::boot::MemoryError>
{
    use mac_abi::boot::MemoryError;
    if count == 0
    {
        return Ok(&[]);
    }
    let bytes = count.checked_mul(size_of::<T>())
                     .filter(|&bytes| bytes != 0 && bytes <= isize::MAX as usize)
                     .ok_or(MemoryError::InvalidArray)?;
    let physical = pointer as u64;
    let end = physical.checked_add(bytes as u64).ok_or(MemoryError::InvalidArray)?;
    let ram_end = info.ram_base.checked_add(info.ram_size).ok_or(MemoryError::InvalidRange)?;
    if pointer.is_null()
       || !physical.is_multiple_of(align_of::<T>() as u64)
       || physical < info.ram_base
       || end > ram_end
    {
        return Err(MemoryError::InvalidArray);
    }
    let offset = physical.checked_sub(VM_PHYSMAP_PHYSICAL_BASE).ok_or(MemoryError::InvalidArray)?;
    let cpu = VM_PHYSMAP_START.checked_add(offset)
                              .filter(|cpu| {
                                  cpu.checked_add(bytes as u64)
                                     .is_some_and(|end| end <= VM_PHYSMAP_END)
                              })
                              .ok_or(MemoryError::InvalidArray)?;
    Ok(unsafe { core::slice::from_raw_parts(cpu as *const T, count) })
}
/// # Safety
/// Same pinned physical wire contract as primary_slice, required for native boot.
pub(crate) unsafe fn execution(
    info: &mac_abi::BootInfo)
    -> Result<&'static mac_abi::boot::execution::Execution, mac_abi::boot::MemoryError>
{
    unsafe {primary_slice(info,info.execution,1)}?.first().ok_or(mac_abi::boot::MemoryError::InvalidArray)
}

//! Checked translation of a retained linear mapping's nonempty byte span.
//! These address facts do not issue ownership or establish a hardware mapping.
#![forbid(unsafe_code)]

pub(super) fn physical(cpu: u64, bytes: u64, virtual_base: u64, physical_base: u64,
                       usable_bytes: u64)
                       -> Option<u64>
{
    if bytes == 0
    {
        return None;
    }
    let offset = cpu.checked_sub(virtual_base)?;
    let limit = offset.checked_add(bytes)?;
    if limit > usable_bytes
    {
        return None;
    }
    let physical = physical_base.checked_add(offset)?;
    physical.checked_add(bytes)?;
    Some(physical)
}

#[cfg(test)]
mod tests
{
    use super::physical;
    #[test]
    fn heap_dma_spans_translate_both_ends_and_refuse_unmapped_padding()
    {
        let base = 0xffffffd040000000;
        let backing = 0xa0000000;
        let usable = (512 << 20) + 4096;
        let geometry = mac_kernel_model::pages::PageGeometry::new(4096).unwrap();
        assert_eq!(geometry.buddy_bytes(512 << 20).unwrap(), 512 << 20);
        assert_eq!(geometry.buddy_bytes(usable).unwrap(), 1 << 30);
        // This non-power-of-two request owns a full 1 GiB buddy block. Only the
        // requested usable window is mapped; no DMA permission follows from
        // knowing that a retained physical tail exists.
        assert_eq!(physical(base, usable, base, backing, usable), Some(backing));
        assert_eq!(physical(base, 1, base, backing, usable), Some(backing));
        assert_eq!(physical(base + usable - 1, 1, base, backing, usable),
                   Some(backing + usable - 1));
        assert_eq!(physical(base + 4096, 512, base, backing, usable), Some(backing + 4096));
        for (cpu, bytes) in [(base, 0),
                             (base - 1, 1),
                             (base - 1, 2),
                             (base + usable, 1),
                             (base + usable - 1, 2),
                             (base + (1 << 30) - 1, 1),
                             (base, u64::MAX)]
        {
            assert_eq!(physical(cpu, bytes, base, backing, usable),
                       None,
                       "invalid or unowned byte span {cpu:#x}+{bytes}");
        }
    }
    #[test]
    fn translation_refuses_cpu_and_bus_arithmetic_overflow()
    {
        assert_eq!(physical(u64::MAX - 1, 4, 0, 0, u64::MAX), None);
        assert_eq!(physical(7, 1, 0, u64::MAX - 4, 64), None);
        assert_eq!(physical(0, 2, 0, u64::MAX - 1, 64), None);
        assert_eq!(physical(63, 1, 0, 128, 64), Some(191));
    }
}

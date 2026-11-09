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


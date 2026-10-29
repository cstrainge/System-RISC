//! Checked DTSpec v17 memory discovery. No MMIO, pointers, allocation or recursion.
//! <https://devicetree-specification.readthedocs.io/en/stable/flattened-format.html>
use core::ops::Range;

pub use mac_formats::Error;
type Result<T> = core::result::Result<T, Error>;
fn word(bytes: &[u8], offset: usize) -> Result<u32>
{
    let end = offset.checked_add(4).ok_or(Error::Malformed("device tree offset overflow"))?;
    Ok(u32::from_be_bytes(bytes.get(offset..end)
                               .ok_or(Error::Malformed("truncated device tree"))?
                               .try_into()
                               .unwrap()))
}
fn wide(bytes: &[u8], offset: usize) -> Result<u64>
{
    Ok((u64::from(word(bytes, offset)?) << 32) | u64::from(word(bytes, offset + 4)?))
}
fn cstr(bytes: &[u8]) -> Result<&[u8]>
{
    let length = bytes.iter()
                      .position(|b| *b == 0)
                      .ok_or(Error::Malformed("unterminated device tree name"))?;
    Ok(&bytes[..length])
}
fn overlap(a: &Range<usize>, b: &Range<usize>) -> bool
{
    a.start < b.end && b.start < a.end
}
fn extent(base: u64, length: u64) -> Result<Range<u64>>
{
    let end =
        base.checked_add(length).ok_or(Error::Malformed("device tree memory extent overflows"))?;
    if length == 0
    {
        return Err(Error::Malformed("device tree memory extent is empty"));
    }
    Ok(base..end)
}
fn cells(bytes: &[u8], count: u32) -> Result<u64>
{
    match count
    {
        1 => Ok(u64::from(word(bytes, 0)?)),
        2 => wide(bytes, 0),
        _ => Err(Error::Malformed("device tree addresses require one or two cells"))
    }
}
fn set_cpu_cell(slot: &mut Option<u32>, value: &[u8]) -> Result<()>
{
    if value.len() != 4 || slot.is_some()
    {
        return Err(Error::Malformed("CPU cell declaration must be one nonrepeated u32"));
    }
    *slot = Some(word(value, 0)?);
    Ok(())
}
fn cpu_cells(address: Option<u32>, size: Option<u32>) -> Result<u32>
{
    match (address, size)
    {
        (Some(address @ (1 | 2)), Some(0)) => Ok(address),
        _ => Err(Error::Malformed("/cpus requires one or two address cells and zero size cells"))
    }
}
fn set_cpu_status(slot: &mut Option<bool>, value: &[u8]) -> Result<()>
{
    if slot.is_some()
    {
        return Err(Error::Malformed("device tree CPU repeats status"));
    }
    *slot = Some(match value
                 {
                     b"okay\0" | b"ok\0" => true,
                     b"disabled\0" | b"reserved\0" | b"fail\0" => false,
                     _ if value.starts_with(b"fail-")
                          && value.len() > 6
                          && value.last() == Some(&0)
                          && value[..value.len() - 1].iter()
                                                     .all(|byte| byte.is_ascii_graphic()) =>
                     {
                         false
                     }
                     _ =>
                     {
                         return Err(Error::Malformed("device tree CPU status is not a \
                                                      recognized single string"));
                     }
                 });
    Ok(())
}
#[derive(Clone, Copy)]
pub struct Tree<'a>
{
    bytes:     &'a [u8],
    structure: &'a [u8],
    strings:   &'a [u8],
    reserved:  &'a [u8]
}
impl<'a> Tree<'a>
{
    pub fn new(bytes: &'a [u8]) -> Result<Self>
    {
        if word(bytes, 0)? != 0xd00dfeed
        {
            return Err(Error::Malformed("invalid device tree magic"));
        }
        let total = word(bytes, 4)? as usize;
        if total < 40 || total > bytes.len()
        {
            return Err(Error::Malformed("truncated device tree total size"));
        }
        let bytes = &bytes[..total];
        if word(bytes, 20)? < 17 || word(bytes, 24)? > 17
        {
            return Err(Error::Malformed("device tree requires a version compatible with 17"));
        }
        let block = |base: usize, length: usize| -> Result<Range<usize>> {
            let end =
                base.checked_add(length).ok_or(Error::Malformed("device tree block overflows"))?;
            if base < 40 || end > total
            {
                return Err(Error::Malformed("device tree block is outside the blob"));
            }
            Ok(base..end)
        };
        let structure = block(word(bytes, 8)? as usize, word(bytes, 36)? as usize)?;
        let strings = block(word(bytes, 12)? as usize, word(bytes, 32)? as usize)?;
        let reserved_start = word(bytes, 16)? as usize;
        if structure.start % 4 != 0 || reserved_start % 8 != 0 || reserved_start < 40
        {
            return Err(Error::Malformed("device tree block alignment is invalid"));
        }
        let mut end = reserved_start;
        loop
        {
            let base = wide(bytes, end)?;
            let size = wide(bytes,
                            end.checked_add(8)
                               .ok_or(Error::Malformed("device tree reservation offset \
                                                        overflow"))?)?;
            end = end.checked_add(16)
                     .ok_or(Error::Malformed("device tree reservation offset overflow"))?;
            if base == 0 && size == 0
            {
                break;
            }
            extent(base, size)?;
        }
        let reserved = reserved_start..end;
        if overlap(&structure, &strings)
           || overlap(&structure, &reserved)
           || overlap(&strings, &reserved)
        {
            return Err(Error::Malformed("device tree blocks overlap"));
        }
        Ok(Self { bytes,
                  structure: &bytes[structure],
                  strings: &bytes[strings],
                  reserved: &bytes[reserved] })
    }

    pub fn total_size(&self) -> usize
    {
        self.bytes.len()
    }

    /// Reports all enabled root-level memory banks, not just the first bank.
    pub fn memory(&self, mut visit: impl FnMut(Range<u64>) -> Result<()>) -> Result<()>
    {
        self.walk(false, &mut visit)
    }

    /// Includes both the reservation block and static /reserved-memory children.
    /// Dynamic reserved-memory allocation is refused rather than ignored.
    pub fn reservations(&self, mut visit: impl FnMut(Range<u64>) -> Result<()>) -> Result<()>
    {
        for entry in self.reserved.chunks_exact(16)
        {
            let base = wide(entry, 0)?;
            let size = wide(entry, 8)?;
            if base == 0 && size == 0
            {
                break;
            }
            visit(extent(base, size)?)?;
        }
        self.walk(true, &mut visit)
    }

    /// Visit enabled /cpus CPU nodes. Status, register width and container cell
    /// declarations are checked even for disabled CPUs; only enabled identities
    /// enter the firmware handoff. Hardware IDs need not be dense or ordered.
    pub fn cpus(&self, mut visit: impl FnMut(mac_abi::boot::topology::Cpu) -> Result<()>)
                -> Result<()>
    {
        let (mut pos, mut depth) = (0usize, 0u32);
        let (mut root_seen, mut root_closed, mut cpus_seen) = (false, false, false);
        let mut root_children = false;
        let (mut cpus_parent, mut cpus_children, mut cpu_children) = (false, false, false);
        let (mut address_cells, mut size_cells) = (None, None);
        let mut parent_status = None;
        let mut status = None;
        let mut device_type: Option<&[u8]> = None;
        let mut reg: Option<&[u8]> = None;
        let mut cpu_name = false;
        loop
        {
            let token = word(self.structure, pos)?;
            pos =
                pos.checked_add(4).ok_or(Error::Malformed("device tree token offset overflow"))?;
            match token
            {
                1 =>
                {
                    if root_closed
                    {
                        return Err(Error::Malformed("device tree contains multiple roots"));
                    }
                    let name = cstr(self.structure
                                        .get(pos..)
                                        .ok_or(Error::Malformed("truncated device tree node"))?)?;
                    pos = pos.checked_add(name.len() + 1)
                             .and_then(|n| n.checked_add(3))
                             .map(|n| n & !3)
                             .ok_or(Error::Malformed("device tree node offset overflow"))?;
                    depth = depth.checked_add(1)
                                 .ok_or(Error::Malformed("device tree nesting overflow"))?;
                    if depth == 1
                    {
                        if root_seen || !name.is_empty()
                        {
                            return Err(Error::Malformed("device tree root must have an empty \
                                                         name"));
                        }
                        root_seen = true;
                    }
                    if depth == 2
                    {
                        root_children = true;
                        cpus_parent = name == b"cpus";
                        if cpus_parent
                        {
                            if cpus_seen
                            {
                                return Err(Error::Malformed("device tree repeats /cpus"));
                            }
                            cpus_seen = true;
                            cpus_children = false;
                            parent_status = None;
                            address_cells = None;
                            size_cells = None;
                        }
                    }
                    if depth == 3 && cpus_parent
                    {
                        cpu_cells(address_cells, size_cells)?;
                        cpus_children = true;
                        cpu_children = false;
                        cpu_name = name == b"cpu" || name.starts_with(b"cpu@");
                        status = None;
                        device_type = None;
                        reg = None;
                    }
                    if depth == 4 && cpus_parent
                    {
                        cpu_children = true;
                    }
                }
                2 =>
                {
                    if depth == 0
                    {
                        return Err(Error::Malformed("device tree has an unmatched node end"));
                    }
                    if depth == 3 && cpus_parent && (cpu_name || device_type == Some(&b"cpu\0"[..]))
                    {
                        if device_type != Some(&b"cpu\0"[..])
                        {
                            return Err(Error::Malformed("device tree CPU requires device_type \
                                                         cpu"));
                        }
                        let address_cells = cpu_cells(address_cells, size_cells)?;
                        let reg = reg.ok_or(Error::Malformed("device tree CPU has no reg"))?;
                        if reg.len() != address_cells as usize * 4
                        {
                            return Err(Error::Malformed("device tree CPU reg must contain \
                                                         exactly one hart ID"));
                        }
                        let hart = cells(reg, address_cells)?;
                        if parent_status.unwrap_or(true) && status.unwrap_or(true)
                        {
                            visit(mac_abi::boot::topology::Cpu { hart })?;
                        }
                    }
                    if depth == 2
                    {
                        if cpus_parent
                        {
                            cpu_cells(address_cells, size_cells)?;
                        }
                        cpus_parent = false;
                    }
                    depth -= 1;
                    if depth == 0
                    {
                        root_closed = true;
                    }
                }
                3 =>
                {
                    if depth == 0
                    {
                        return Err(Error::Malformed("device tree property is outside a node"));
                    }
                    let length = word(self.structure, pos)? as usize;
                    let name_offset = word(self.structure, pos + 4)? as usize;
                    pos = pos.checked_add(8)
                             .ok_or(Error::Malformed("device tree property offset overflow"))?;
                    let end = pos.checked_add(length)
                                 .ok_or(Error::Malformed("device tree property length overflow"))?;
                    let value = self.structure
                                    .get(pos..end)
                                    .ok_or(Error::Malformed("truncated device tree property"))?;
                    let name = cstr(self.strings
                                        .get(name_offset..)
                                        .ok_or(Error::Malformed("device tree property name \
                                                                 is outside strings"))?)?;
                    pos = end.checked_add(3)
                             .map(|n| n & !3)
                             .ok_or(Error::Malformed("device tree property alignment overflow"))?;
                    if depth == 1 && root_children
                    {
                        return Err(Error::Malformed("device tree root properties must precede \
                                                     children"));
                    }
                    if cpus_parent && depth == 2
                    {
                        if cpus_children
                        {
                            return Err(Error::Malformed("/cpus properties must precede children"));
                        }
                        match name
                        {
                            b"#address-cells" => set_cpu_cell(&mut address_cells, value)?,
                            b"#size-cells" => set_cpu_cell(&mut size_cells, value)?,
                            b"status" => set_cpu_status(&mut parent_status, value)?,
                            _ => ()
                        }
                    }
                    if cpus_parent && depth == 3
                    {
                        if cpu_children
                        {
                            return Err(Error::Malformed("CPU properties must precede children"));
                        }
                        match name
                        {
                            b"device_type" =>
                            {
                                if device_type.replace(value).is_some()
                                {
                                    return Err(Error::Malformed("device tree CPU repeats \
                                                                 device_type"));
                                }
                            }
                            b"reg" =>
                            {
                                if reg.replace(value).is_some()
                                {
                                    return Err(Error::Malformed("device tree CPU repeats reg"));
                                }
                            }
                            b"status" => set_cpu_status(&mut status, value)?,
                            _ => ()
                        }
                    }
                }
                4 => (),
                9 =>
                {
                    if !root_seen || !root_closed || depth != 0 || pos != self.structure.len()
                    {
                        return Err(Error::Malformed("device tree structure ended before root \
                                                     close or has trailing tokens"));
                    }
                    if !cpus_seen
                    {
                        return Err(Error::Malformed("device tree has no /cpus container"));
                    }
                    return Ok(());
                }
                _ => return Err(Error::Malformed("unknown device tree structure token"))
            }
        }
    }

    /// Allocation-free structural/count/primary preflight. Uniqueness is checked
    /// after exact fallible metadata admission and in-place sorting, before any
    /// handoff is published; this avoids a quadratic sequence of DT scans.
    pub fn cpu_count(&self, primary: u64) -> Result<usize>
    {
        let mut count = 0usize;
        let mut present = false;
        self.cpus(|cpu| {
                count = count.checked_add(1).ok_or(Error::Malformed("CPU count overflow"))?;
                present |= cpu.hart == primary;
                Ok(())
            })?;
        if !present
        {
            return Err(Error::Malformed("device tree primary hart is absent or disabled"));
        }
        Ok(count)
    }

    fn walk(&self, reservations: bool, visit: &mut impl FnMut(Range<u64>) -> Result<()>)
            -> Result<()>
    {
        let (mut pos, mut depth) = (0usize, 0u32);
        let (mut root_seen, mut root_closed) = (false, false);
        let (mut root_children, mut reserved_children) = (false, false);
        let (mut address_cells, mut size_cells) = (2, 1);
        let (mut reserved_address_cells, mut reserved_size_cells) = (2, 1);
        let (mut reserved_parent, mut candidate, mut memory_name) = (false, false, false);
        let (mut memory_type, mut enabled, mut dynamic) = (false, true, false);
        let mut reg = None;
        loop
        {
            let token = word(self.structure, pos)?;
            pos =
                pos.checked_add(4).ok_or(Error::Malformed("device tree token offset overflow"))?;
            match token
            {
                1 =>
                {
                    if root_closed
                    {
                        return Err(Error::Malformed("device tree contains multiple roots"));
                    }
                    let name = cstr(self.structure
                                        .get(pos..)
                                        .ok_or(Error::Malformed("truncated device tree node"))?)?;
                    pos = pos.checked_add(name.len() + 1)
                             .and_then(|n| n.checked_add(3))
                             .map(|n| n & !3)
                             .ok_or(Error::Malformed("device tree node offset overflow"))?;
                    depth = depth.checked_add(1)
                                 .ok_or(Error::Malformed("device tree nesting overflow"))?;
                    if depth == 1
                    {
                        if root_seen || !name.is_empty()
                        {
                            return Err(Error::Malformed("device tree root must have an empty \
                                                         name"));
                        }
                        root_seen = true;
                    }
                    if depth == 2
                    {
                        root_children = true;
                        reserved_children = false;
                        reserved_parent = name == b"reserved-memory";
                        reserved_address_cells = address_cells;
                        reserved_size_cells = size_cells;
                    }
                    if depth == 3 && reserved_parent
                    {
                        reserved_children = true;
                    }
                    if (!reservations && depth == 2)
                       || (reservations && depth == 3 && reserved_parent)
                    {
                        candidate = true;
                        memory_name = name == b"memory" || name.starts_with(b"memory@");
                        memory_type = false;
                        enabled = true;
                        dynamic = false;
                        reg = None;
                    }
                }
                2 =>
                {
                    if depth == 0
                    {
                        return Err(Error::Malformed("device tree has an unmatched node end"));
                    }
                    if candidate && ((!reservations && depth == 2) || (reservations && depth == 3))
                    {
                        if enabled && (reservations || memory_name || memory_type)
                        {
                            if reservations && dynamic && reg.is_none()
                            {
                                return Err(Error::Malformed("dynamic reserved-memory \
                                                             allocation is not supported at \
                                                             boot"));
                            }
                            let values: &[u8] = reg.ok_or(Error::Malformed("device tree memory \
                                                                            node has no reg \
                                                                            property"))?;
                            let (a, s) = if reservations
                            {
                                (reserved_address_cells, reserved_size_cells)
                            }
                            else
                            {
                                (address_cells, size_cells)
                            };
                            if !(1..=2).contains(&a) || !(1..=2).contains(&s)
                            {
                                return Err(Error::Malformed("device tree memory requires one \
                                                             or two address and size cells"));
                            }
                            let stride = ((a + s) * 4) as usize;
                            if values.is_empty() || values.len() % stride != 0
                            {
                                return Err(Error::Malformed("device tree memory reg has an \
                                                             invalid cell count"));
                            }
                            for value in values.chunks_exact(stride)
                            {
                                visit(extent(cells(value, a)?,
                                             cells(&value[(a * 4) as usize..], s)?)?)?;
                            }
                        }
                        candidate = false;
                    }
                    if depth == 2
                    {
                        reserved_parent = false;
                    }
                    depth -= 1;
                    if depth == 0
                    {
                        root_closed = true;
                    }
                }
                3 =>
                {
                    if depth == 0
                    {
                        return Err(Error::Malformed("device tree property is outside a node"));
                    }
                    let length = word(self.structure, pos)? as usize;
                    let name_offset = word(self.structure, pos + 4)? as usize;
                    pos = pos.checked_add(8)
                             .ok_or(Error::Malformed("device tree property offset overflow"))?;
                    let end = pos.checked_add(length)
                                 .ok_or(Error::Malformed("device tree property length overflow"))?;
                    let value = self.structure
                                    .get(pos..end)
                                    .ok_or(Error::Malformed("truncated device tree property"))?;
                    let name = cstr(self.strings
                                        .get(name_offset..)
                                        .ok_or(Error::Malformed("device tree property name \
                                                                 is outside strings"))?)?;
                    pos = end.checked_add(3)
                             .map(|n| n & !3)
                             .ok_or(Error::Malformed("device tree property alignment overflow"))?;
                    if depth == 1
                    {
                        if root_children
                        {
                            return Err(Error::Malformed("device tree root properties must \
                                                         precede children"));
                        }
                        if matches!(name, b"#address-cells" | b"#size-cells") && value.len() != 4
                        {
                            return Err(Error::Malformed("device tree cell counts must be one \
                                                         u32"));
                        }
                        match name
                        {
                            b"#address-cells" => address_cells = word(value, 0)?,
                            b"#size-cells" => size_cells = word(value, 0)?,
                            _ =>
                            {}
                        }
                    }
                    if reservations && depth == 2 && reserved_parent
                    {
                        if reserved_children
                        {
                            return Err(Error::Malformed("reserved-memory properties must \
                                                         precede children"));
                        }
                        if matches!(name, b"#address-cells" | b"#size-cells") && value.len() != 4
                        {
                            return Err(Error::Malformed("device tree cell counts must be one \
                                                         u32"));
                        }
                        match name
                        {
                            b"#address-cells" => reserved_address_cells = word(value, 0)?,
                            b"#size-cells" => reserved_size_cells = word(value, 0)?,
                            b"ranges" if !value.is_empty() =>
                            {
                                return Err(Error::Malformed("translated reserved-memory ranges \
                                                             are not supported at boot"));
                            }
                            _ =>
                            {}
                        }
                    }
                    if candidate && ((!reservations && depth == 2) || (reservations && depth == 3))
                    {
                        match name
                        {
                            b"reg" =>
                            {
                                if reg.replace(value).is_some()
                                {
                                    return Err(Error::Malformed("device tree memory has \
                                                                 duplicate reg properties"));
                                }
                            }
                            b"device_type" => memory_type = value == b"memory\0",
                            b"status" => enabled = value == b"okay\0" || value == b"ok\0",
                            b"size" => dynamic = true,
                            _ =>
                            {}
                        }
                    }
                }
                4 =>
                {}
                9 =>
                {
                    if !root_seen || !root_closed || depth != 0 || pos != self.structure.len()
                    {
                        return Err(Error::Malformed("device tree structure ended before the \
                                                     root closed or has trailing tokens"));
                    }
                    return Ok(());
                }
                _ => return Err(Error::Malformed("unknown device tree structure token"))
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests
{
    extern crate std;
    use std::{string::ToString, vec, vec::Vec};

    use super::*;
    struct Builder
    {
        structure:    Vec<u8>,
        strings:      Vec<u8>,
        reservations: Vec<u8>
    }
    impl Builder
    {
        fn new() -> Self
        {
            Self { structure:    Vec::new(),
                   strings:      Vec::new(),
                   reservations: Vec::new() }
        }

        fn token(&mut self, n: u32)
        {
            self.structure.extend_from_slice(&n.to_be_bytes());
        }

        fn begin(&mut self, name: &[u8])
        {
            self.token(1);
            self.structure.extend_from_slice(name);
            self.structure.push(0);
            while self.structure.len() % 4 != 0
            {
                self.structure.push(0);
            }
        }

        fn end(&mut self)
        {
            self.token(2);
        }

        fn prop(&mut self, name: &[u8], value: &[u8])
        {
            self.token(3);
            self.token(value.len() as u32);
            self.token(self.strings.len() as u32);
            self.strings.extend_from_slice(name);
            self.strings.push(0);
            self.structure.extend_from_slice(value);
            while self.structure.len() % 4 != 0
            {
                self.structure.push(0);
            }
        }

        fn finish(mut self) -> Vec<u8>
        {
            self.token(9);
            self.reservations.extend_from_slice(&[0; 16]);
            let structure_offset = 40 + self.reservations.len();
            let strings_offset = structure_offset + self.structure.len();
            let words = [0xd00dfeed,
                         (strings_offset + self.strings.len()) as u32,
                         structure_offset as u32,
                         strings_offset as u32,
                         40,
                         17,
                         16,
                         0,
                         self.strings.len() as u32,
                         self.structure.len() as u32];
            let mut bytes = Vec::new();
            for word in words
            {
                bytes.extend_from_slice(&word.to_be_bytes());
            }
            bytes.extend(self.reservations);
            bytes.extend(self.structure);
            bytes.extend(self.strings);
            bytes
        }
    }
    pub(crate) fn blob(banks: &[(u64, u64)], dynamic: bool) -> Vec<u8>
    {
        let mut b = Builder::new();
        b.begin(b"");
        b.prop(b"#address-cells", &2u32.to_be_bytes());
        b.prop(b"#size-cells", &2u32.to_be_bytes());
        b.begin(b"memory@80000000");
        b.prop(b"device_type", b"memory\0");
        let mut reg = Vec::new();
        for (base, len) in banks
        {
            reg.extend_from_slice(&base.to_be_bytes());
            reg.extend_from_slice(&len.to_be_bytes());
        }
        b.prop(b"reg", &reg);
        b.end();
        b.begin(b"reserved-memory");
        b.prop(b"#address-cells", &2u32.to_be_bytes());
        b.prop(b"#size-cells", &2u32.to_be_bytes());
        b.prop(b"ranges", &[]);
        b.begin(b"firmware@87000000");
        if dynamic
        {
            b.prop(b"size", &0x1000u64.to_be_bytes());
        }
        else
        {
            let mut reg = Vec::new();
            reg.extend_from_slice(&0x87000000u64.to_be_bytes());
            reg.extend_from_slice(&0x800000u64.to_be_bytes());
            b.prop(b"reg", &reg);
        }
        b.end();
        b.end();
        b.end();
        b.finish()
    }
    #[test]
    fn discovers_all_banks_above_four_gib_and_static_reservations()
    {
        let bytes = blob(&[(0x80000000, 0x200000000), (0x400000000, 0x10000000)], false);
        let tree = Tree::new(&bytes).unwrap();
        let mut ram = Vec::new();
        tree.memory(|range| {
                ram.push(range);
                Ok(())
            })
            .unwrap();
        assert_eq!(ram, vec![0x80000000..0x280000000, 0x400000000..0x410000000]);
        let mut reserved = Vec::new();
        tree.reservations(|range| {
                reserved.push(range);
                Ok(())
            })
            .unwrap();
        assert_eq!(reserved, vec![0x87000000..0x87800000]);
    }
    #[test]
    fn refuses_truncation_corruption_and_unimplemented_dynamic_reservations()
    {
        let bytes = blob(&[(0x80000000, 0x8000000)], false);
        for length in 0..bytes.len()
        {
            assert!(Tree::new(&bytes[..length]).is_err());
        }
        let mut bad = bytes.clone();
        bad[0] = 0;
        assert!(Tree::new(&bad).err().unwrap().to_string().contains("magic"));
        let mut bad = bytes.clone();
        bad[8..12].copy_from_slice(&40u32.to_be_bytes());
        assert!(Tree::new(&bad).err().unwrap().to_string().contains("overlap"));
        let mut bad = bytes.clone();
        let structure = u32::from_be_bytes(bad[8..12].try_into().unwrap()) as usize;
        bad[structure..structure + 4].copy_from_slice(&99u32.to_be_bytes());
        assert!(Tree::new(&bad).unwrap()
                               .memory(|_| Ok(()))
                               .unwrap_err()
                               .to_string()
                               .contains("unknown device tree structure token"));
        let bad = blob(&[(u64::MAX - 4095, 8192)], false);
        assert!(Tree::new(&bad).unwrap()
                               .memory(|_| Ok(()))
                               .unwrap_err()
                               .to_string()
                               .contains("overflows"));
        let bad = blob(&[(0x80000000, 0x8000000)], true);
        assert!(Tree::new(&bad).unwrap()
                               .reservations(|_| Ok(()))
                               .unwrap_err()
                               .to_string()
                               .contains("dynamic reserved-memory"));
    }
    #[test]
    fn supports_32bit_cells_and_skips_disabled_banks()
    {
        let mut b = Builder::new();
        b.begin(b"");
        b.prop(b"#address-cells", &1u32.to_be_bytes());
        b.prop(b"#size-cells", &1u32.to_be_bytes());
        b.begin(b"memory@80000000");
        b.prop(b"reg", &[0x80, 0, 0, 0, 0x08, 0, 0, 0]);
        b.end();
        b.begin(b"memory@90000000");
        b.prop(b"status", b"disabled\0");
        b.end();
        b.end();
        let bytes = b.finish();
        let mut ram = Vec::new();
        Tree::new(&bytes).unwrap()
                         .memory(|range| {
                             ram.push(range);
                             Ok(())
                         })
                         .unwrap();
        assert_eq!(ram, vec![0x80000000..0x88000000]);
    }
    #[test]
    fn rejects_late_or_missized_cell_contracts_and_missing_memory_extents()
    {
        let mut b = Builder::new();
        b.begin(b"");
        b.begin(b"memory@80000000");
        b.prop(b"reg", &[0x80, 0, 0, 0, 0, 0, 0, 0, 0x08, 0, 0, 0]);
        b.end();
        b.prop(b"#address-cells", &1u32.to_be_bytes());
        b.end();
        let bytes = b.finish();
        assert!(Tree::new(&bytes).unwrap()
                                 .memory(|_| Ok(()))
                                 .unwrap_err()
                                 .to_string()
                                 .contains("root properties must precede children"));
        let mut b = Builder::new();
        b.begin(b"");
        b.prop(b"#address-cells", &2u64.to_be_bytes());
        b.end();
        let bytes = b.finish();
        assert!(Tree::new(&bytes).unwrap()
                                 .memory(|_| Ok(()))
                                 .unwrap_err()
                                 .to_string()
                                 .contains("cell counts must be one u32"));
        let mut b = Builder::new();
        b.begin(b"");
        b.begin(b"memory@80000000");
        b.end();
        b.end();
        let bytes = b.finish();
        assert!(Tree::new(&bytes).unwrap()
                                 .memory(|_| Ok(()))
                                 .unwrap_err()
                                 .to_string()
                                 .contains("memory node has no reg property"));
    }
}

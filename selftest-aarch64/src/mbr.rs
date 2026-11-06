//! The allocation-free MBR decode this report needs. `mac-block` owns the
//! checked reader for the OS, but it builds a vector of entries, and this
//! binary deliberately has no allocator. Only the fields the report prints are
//! decoded; the signature is checked by the caller before this runs.
pub struct Entry
{
    pub index: usize,
    pub kind:  u8,
    pub first: u32
}

pub fn entries(sector: &[u8; 512]) -> [Option<Entry>; 4]
{
    let mut out = [None, None, None, None];
    for index in 0..4
    {
        let at = 0x1be + index * 16;
        let kind = sector[at + 4];
        let first =
            u32::from_le_bytes([sector[at + 8], sector[at + 9], sector[at + 10], sector[at + 11]]);
        if kind != 0 || first != 0
        {
            out[index] = Some(Entry { index,
                                      kind,
                                      first });
        }
    }
    out
}

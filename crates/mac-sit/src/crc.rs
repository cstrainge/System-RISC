//! Reflected CRC-16/IBM, initial zero; used by headers and uncompressed forks.
const fn table() -> [u16; 256]
{
    let mut table = [0; 256];
    let mut i = 0;
    while i < table.len()
    {
        let mut value = i as u16;
        let mut bit = 0;
        while bit < 8
        {
            value = (value >> 1) ^ if value & 1 != 0 { 0xa001 } else { 0 };
            bit += 1;
        }
        table[i] = value;
        i += 1;
    }
    table
}
const TABLE: [u16; 256] = table();
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Crc16(u16);
impl Crc16
{
    pub const fn new() -> Self
    {
        Self(0)
    }

    pub fn update(&mut self, bytes: &[u8])
    {
        for byte in bytes
        {
            self.0 = (self.0 >> 8) ^ TABLE[usize::from((self.0 ^ u16::from(*byte)) & 255)];
        }
    }

    pub const fn value(self) -> u16
    {
        self.0
    }

    pub fn checksum(bytes: &[u8]) -> u16
    {
        let mut crc = Self::new();
        crc.update(bytes);
        crc.value()
    }
}

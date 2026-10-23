//! Allocation-free native resource wire contract, shared by inspectors and loaders.
#![no_std]
#![forbid(unsafe_code)]
pub mod image;
pub mod code_types;

pub const FORMAT_VERSION: u16 = 1;
pub const ABI_VERSION: u32 = 11;
pub const DESCRIPTOR_SIZE: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Role
{
    System       = 1,
    Application  = 2,
    CodeResource = 3
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descriptor
{
    pub role:        Role,
    pub abi_version: u32,
    pub load_base:   u64,
    pub load_limit:  u64,
    pub entry:       u64
}

impl Descriptor
{
    pub fn encode(&self) -> Result<[u8; DESCRIPTOR_SIZE]>
    {
        if self.abi_version != ABI_VERSION
           || self.load_base >= self.load_limit
           || self.entry < self.load_base
           || self.entry >= self.load_limit
           || self.entry & 1 != 0
        {
            return Err(Error("invalid native ABI descriptor"));
        }
        let mut bytes = [0; DESCRIPTOR_SIZE];
        bytes[..4].copy_from_slice(b"RVN1");
        bytes[4..6].copy_from_slice(&FORMAT_VERSION.to_be_bytes());
        bytes[6..8].copy_from_slice(&(self.role as u16).to_be_bytes());
        bytes[8..12].copy_from_slice(&self.abi_version.to_be_bytes());
        bytes[16..24].copy_from_slice(&self.load_base.to_be_bytes());
        bytes[24..32].copy_from_slice(&self.load_limit.to_be_bytes());
        bytes[32..40].copy_from_slice(&self.entry.to_be_bytes());
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self>
    {
        if bytes.len() == 32 && bytes.starts_with(b"RVN1")
        {
            return Err(Error("obsolete ELF-backed NABI layout; rebuild the system"));
        }
        if bytes.len() != DESCRIPTOR_SIZE
           || slice(bytes, 0, 4)? != b"RVN1"
           || be16(bytes, 4)? != FORMAT_VERSION
           || be32(bytes, 12)? != 0
        {
            return Err(Error("invalid native ABI descriptor header"));
        }
        let role = match be16(bytes, 6)?
        {
            1 => Role::System,
            2 => Role::Application,
            3 => Role::CodeResource,
            _ => return Err(Error("unknown native executable role"))
        };
        let result = Self { role,
                            abi_version: be32(bytes, 8)?,
                            load_base: u64::from_be_bytes(bytes[16..24].try_into().unwrap()),
                            load_limit: u64::from_be_bytes(bytes[24..32].try_into().unwrap()),
                            entry: u64::from_be_bytes(bytes[32..40].try_into().unwrap()) };
        result.encode()?;
        Ok(result)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(pub &'static str);
impl core::fmt::Display for Error
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        f.write_str(self.0)
    }
}
impl core::error::Error for Error
{
}
pub type Result<T> = core::result::Result<T, Error>;
pub(crate) fn slice(bytes: &[u8], offset: usize, length: usize) -> Result<&[u8]>
{
    let end = offset.checked_add(length).ok_or(Error("offset overflow"))?;
    bytes.get(offset..end).ok_or(Error("truncated data"))
}
pub(crate) fn be16(bytes: &[u8], at: usize) -> Result<u16>
{
    Ok(u16::from_be_bytes(slice(bytes, at, 2)?.try_into().unwrap()))
}
pub(crate) fn be32(bytes: &[u8], at: usize) -> Result<u32>
{
    Ok(u32::from_be_bytes(slice(bytes, at, 4)?.try_into().unwrap()))
}

#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn descriptor_checks_versions_and_roles()
    {
        let d = Descriptor { role:        Role::System,
                             abi_version: ABI_VERSION,
                             load_base:   0x80200000,
                             load_limit:  0x81000000,
                             entry:       0x80200000 };
        let bytes = d.encode().unwrap();
        assert_eq!(Descriptor::decode(&bytes).unwrap(), d);
        for (offset, value) in [(4, 2), (7, 4), (8, 2), (12, 1)]
        {
            let mut bad = bytes;
            bad[offset] = value;
            assert!(Descriptor::decode(&bad).is_err());
        }
    }
}

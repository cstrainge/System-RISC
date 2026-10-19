//! Our new NABI resource, alongside RV64 resource 0 containing an ELF image.
use crate::{Error, Result, be16, be32, slice};

pub const FORMAT_VERSION: u16 = 1;
pub const ABI_VERSION: u32 = 5;

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
    pub load_limit:  u64
}

impl Descriptor
{
    pub fn encode(&self) -> Result<[u8; 32]>
    {
        if self.abi_version != ABI_VERSION || self.load_base >= self.load_limit
        {
            return Err(Error("invalid native ABI descriptor"));
        }
        let mut bytes = [0; 32];
        bytes[..4].copy_from_slice(b"RVN1");
        bytes[4..6].copy_from_slice(&FORMAT_VERSION.to_be_bytes());
        bytes[6..8].copy_from_slice(&(self.role as u16).to_be_bytes());
        bytes[8..12].copy_from_slice(&self.abi_version.to_be_bytes());
        bytes[16..24].copy_from_slice(&self.load_base.to_be_bytes());
        bytes[24..32].copy_from_slice(&self.load_limit.to_be_bytes());
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self>
    {
        if bytes.len() != 32
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
                            load_limit: u64::from_be_bytes(bytes[24..32].try_into().unwrap()) };
        result.encode()?;
        Ok(result)
    }
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
                             load_limit:  0x81000000 };
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

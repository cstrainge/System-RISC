//! Read-only, positional access to a resident-owned backing-file lease.
//! Filesystem providers receive a source capability during mount; it is bound
//! to their admitted module incarnation, never to a host path or app pointer.
#![no_std]
#![forbid(unsafe_code)]
pub const CLASS: mac_module::ClassId = mac_module::ClassId(*b"FORK");
pub const VERSION: u32 = 1;
pub const ARGUMENT_BYTES: usize = 24;
pub const INFO_BYTES: usize = 8;
pub const MAX_TRANSFER: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Operation
{
    Info = 1,
    Read = 2
}
impl TryFrom<u32> for Operation
{
    type Error = Error;

    fn try_from(value: u32) -> Result<Self, Error>
    {
        match value
        {
            1 => Ok(Self::Info),
            2 => Ok(Self::Read),
            _ => Err(Error::Invalid)
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Length,
    Invalid
}

/// Integers are little endian. Bytes 20..24 are reserved zero. Source is a native u64 capability.
/// Only READ accepts a nonzero offset/count; each operation has exact length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arguments
{
    pub source: u64,
    pub offset: u64,
    pub length: u32
}
impl Arguments
{
    pub fn validate(self, operation: Operation) -> Result<(), Error>
    {
        if self.source <= u32::MAX as u64
           || self.length as usize > MAX_TRANSFER
           || self.offset.checked_add(u64::from(self.length)).is_none()
           || operation == Operation::Info && (self.offset != 0 || self.length != 0)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }

    pub fn encode(self, operation: Operation) -> Result<[u8; ARGUMENT_BYTES], Error>
    {
        self.validate(operation)?;
        let mut bytes = [0; ARGUMENT_BYTES];
        bytes[..8].copy_from_slice(&self.source.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.offset.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.length.to_le_bytes());
        Ok(bytes)
    }

    pub fn decode(operation: Operation, bytes: &[u8]) -> Result<Self, Error>
    {
        let bytes: &[u8; ARGUMENT_BYTES] = bytes.try_into().map_err(|_| Error::Length)?;
        if bytes[20..24] != [0; 4]
        {
            return Err(Error::Invalid);
        }
        let result = Self { source: u64::from_le_bytes(bytes[..8].try_into()
                                                                 .map_err(|_| Error::Length)?),
                            offset: u64::from_le_bytes(bytes[8..16].try_into()
                                                                   .map_err(|_| Error::Length)?),
                            length: u32::from_le_bytes(bytes[16..20].try_into()
                                                                    .map_err(|_| Error::Length)?) };
        result.validate(operation)?;
        Ok(result)
    }
}
/// One leased fork's logical byte length, with no block-device geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Info
{
    pub length: u64
}
impl Info
{
    pub fn encode(self) -> [u8; INFO_BYTES]
    {
        self.length.to_le_bytes()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Error>
    {
        Ok(Self { length: u64::from_le_bytes(bytes.try_into().map_err(|_| Error::Length)?) })
    }
}

#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn positional_request_preserves_large_offsets_and_rejects_noncanonical_headers()
    {
        let bytes = [0x23, 1, 2, 0, 1, 0, 0, 0, 7, 0, 0, 0, 1, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0];
        let args = Arguments { source: 0x1_0002_0123,
                               offset: 0x1_0000_0007,
                               length: 512 };
        assert_eq!(Arguments::decode(Operation::Read, &bytes), Ok(args));
        assert_eq!(args.encode(Operation::Read), Ok(bytes));
        assert_eq!(Arguments::decode(Operation::Info, &bytes), Err(Error::Invalid));
        assert_eq!(Arguments::decode(Operation::Read, &bytes[..23]), Err(Error::Length));
        for at in [20, 23]
        {
            let mut bad = bytes;
            bad[at] = 1;
            assert_eq!(Arguments::decode(Operation::Read, &bad), Err(Error::Invalid));
        }
        for bad in [Arguments { source: 0,
                                ..args },
                    Arguments { length: 65537,
                                ..args },
                    Arguments { offset: u64::MAX,
                                ..args }]
        {
            assert_eq!(bad.encode(Operation::Read), Err(Error::Invalid));
        }
        assert_eq!(Info::decode(&u64::MAX.to_le_bytes()), Ok(Info { length: u64::MAX }));
    }
}

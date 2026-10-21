//! Allocation-free contracts for independently loaded native kernel modules.
//! Classes are open identifiers: the loader does not know filesystem, block,
//! input or network request layouts. Their consumers own those contracts.
#![no_std]
#![forbid(unsafe_code)]
pub mod abi;
mod contract;
pub use contract::*;
mod encode;
pub use encode::Spec;
mod policy;
pub use policy::*;

pub const SCHEMA: u16 = 1;
pub const BROKER_ABI: u32 = 1;
pub const HEADER_BYTES: usize = 128;
pub const OFFER_BYTES: usize = 40;
pub const DEPENDENCY_BYTES: usize = 32;
pub const SERVICE_BYTES: usize = 16;
pub const MATCH_BYTES: usize = 40;
pub const MAX_RECORDS: usize = 32;
pub const MAX_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Malformed,
    Version,
    Limit,
    Target,
    Contract,
    Duplicate,
    MissingService,
    MissingDependency,
    WrongLocation
}
pub type Result<T> = core::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClassId(pub [u8; 4]);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageKind
{
    Driver,
    Extension
}
impl PackageKind
{
    pub const fn file_type(self) -> [u8; 4]
    {
        match self
        {
            Self::Driver => *b"DRVR",
            Self::Extension => *b"INIT"
        }
    }

    pub const fn metadata_type(self) -> [u8; 4]
    {
        match self
        {
            Self::Driver => *b"DABI",
            Self::Extension => *b"EABI"
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target
{
    pub architecture: u16,
    pub pointer_bits: u8,
    pub endian:       u8,
    pub convention:   u32,
    pub features:     u64
}
impl Target
{
    pub const BIG: u8 = 2;
    pub const C_ABI: u32 = 1;
    pub const LITTLE: u8 = 1;
    pub const RISCV64: u16 = 1;
    pub const RV64IMAC: Self =
        Self { architecture: Self::RISCV64,
               pointer_bits: 64,
               endian:       Self::LITTLE,
               convention:   Self::C_ABI,
               features:     Self::RV_I | Self::RV_M | Self::RV_A | Self::RV_C };
    pub const RV_A: u64 = 4;
    pub const RV_C: u64 = 8;
    pub const RV_I: u64 = 1;
    pub const RV_M: u64 = 2;
    pub const X86_64: u16 = 2;

    pub fn supports(self, required: Self) -> bool
    {
        self.architecture == required.architecture
        && self.pointer_bits == required.pointer_bits
        && self.endian == required.endian
        && self.convention == required.convention
        && self.features & required.features == required.features
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Offer
{
    pub class:          ClassId,
    pub version:        u32,
    pub request_schema: u32,
    pub reply_schema:   u32,
    pub max_request:    u32,
    pub max_reply:      u32,
    pub flags:          u64,
    pub capabilities:   u64
}
impl Offer
{
    pub const EXCLUSIVE: u64 = 1;
    pub const KERNEL_ONLY: u64 = 4;
    pub const SERIALIZED: u64 = 2;

    pub fn validate(self) -> Result<()>
    {
        if self.class.0 == [0; 4]
           || self.version == 0
           || self.request_schema == 0
           || self.reply_schema == 0
           || self.max_request > 1024 * 1024
           || self.max_reply > 1024 * 1024
           || self.flags & !(Self::EXCLUSIVE | Self::SERIALIZED | Self::KERNEL_ONLY) != 0
        {
            return Err(Error::Contract);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dependency
{
    pub module:   [u8; 16],
    pub class:    ClassId,
    pub version:  u32,
    pub optional: bool
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Service
{
    pub service:  ClassId,
    pub version:  u32,
    pub optional: bool
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceMatch
{
    pub bus:   ClassId,
    pub value: [u8; 16],
    pub mask:  [u8; 16]
}
impl DeviceMatch
{
    pub fn matches(self, bus: ClassId, key: [u8; 16]) -> bool
    {
        self.bus == bus
        && self.value
               .iter()
               .zip(self.mask)
               .zip(key)
               .all(|((&value, mask), key)| value == key & mask)
    }
}

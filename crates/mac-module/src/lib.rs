//! Allocation-free contracts for independently loaded native kernel modules.
//! Classes are open identifiers: the loader does not know filesystem, block,
//! input or network request layouts. Their consumers own those contracts.
#![no_std]
#![forbid(unsafe_code)]
pub mod abi;
pub mod lifecycle;
pub mod preferences;
pub mod file_handlers;
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
/// Existing encoded UTF-8 module-name bound.
pub const MAX_NAME_BYTES: usize = 128;
/// Current native module admission profile. Stored code and zeroed runtime
/// workspace have separate limits: a decoder's BSS is not executable payload.
pub const MAX_CODE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_LOAD_BYTES: u64 = 6 * 1024 * 1024;
pub const MAX_HEAP_BYTES: u32 = 4 * 1024 * 1024;
pub const MAX_STACK_BYTES: u32 = 64 * 1024;

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
/// Extension execution domain is selected by resource ID, never by the optional
/// resource name. A bundle may carry both parts; each loader admits only its
/// own target. This selector does not provide Finder execution machinery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionTarget
{
    System,
    Finder
}
impl ExtensionTarget
{
    pub const ALL: [Self; 2] = [Self::System, Self::Finder];
    pub const RESOURCE_TYPE: [u8; 4] = *b"RV64";

    pub const fn resource_id(self) -> i16
    {
        match self
        {
            Self::System => 0,
            Self::Finder => 1
        }
    }

    pub const fn from_resource_id(id: i16) -> Option<Self>
    {
        match id
        {
            0 => Some(Self::System),
            1 => Some(Self::Finder),
            _ => None
        }
    }

    pub const fn conventional_name(self) -> &'static str
    {
        match self
        {
            Self::System => "System",
            Self::Finder => "Finder"
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageKind
{
    Driver,
    Extension
}
impl PackageKind
{
    pub const fn code_type(self) -> [u8; 4]
    {
        match self
        {
            Self::Driver => *b"DRVR",
            Self::Extension => ExtensionTarget::RESOURCE_TYPE
        }
    }

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
    /// Complete mandatory profile, including V/F/D/B and its Z extensions.
    pub const PROFILE_RVA23U64: u64 = 1 << 4;
    pub const RISCV64: u16 = 1;
    pub const RVA23U64: Self = Self { architecture: Self::RISCV64,
                                      pointer_bits: 64,
                                      endian:       Self::LITTLE,
                                      convention:   Self::C_ABI,
                                      features:     Self::RV_I
                                                    | Self::RV_M
                                                    | Self::RV_A
                                                    | Self::RV_C
                                                    | Self::PROFILE_RVA23U64 };
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

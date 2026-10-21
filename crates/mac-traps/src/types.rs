//! Allocation-free native SDK vocabulary, independent of wire-record layouts.
#![forbid(unsafe_code)]
use crate::{Error, Result};

macro_rules! identity {
    ($name:ident, $word:ty) => {
        #[repr(transparent)]
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub $word);
        impl core::fmt::Display for $name
        {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
            {
                self.0.fmt(f)
            }
        }
    };
}
identity!(VolumeId, u32);
identity!(Cnid, u32);
identity!(ProcessId, u64);
identity!(ThreadId, u64);
identity!(MenuId, i16);
identity!(MenuHandle, u32);
identity!(FileHandle, u32);
identity!(FileWriteHandle, u32);
identity!(ResourceFileHandle, u32);
identity!(MemoryHandleId, u32);
identity!(DialogId, u32);
impl Cnid
{
    pub const ROOT: Self = Self(mac_abi::files::ROOT_ID);
    /// Standard File uses this identity for a destination not created yet.
    pub const UNSAVED: Self = Self(0);
}
impl VolumeId
{
    pub const NONE: Self = Self(0);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeCode([u8; 4]);
impl TypeCode
{
    pub const ZERO: Self = Self([0; 4]);

    pub const fn new(bytes: [u8; 4]) -> Self
    {
        Self(bytes)
    }

    pub const fn bytes(self) -> [u8; 4]
    {
        self.0
    }

    pub const fn as_bytes(&self) -> &[u8; 4]
    {
        &self.0
    }
}
impl From<[u8; 4]> for TypeCode
{
    fn from(bytes: [u8; 4]) -> Self
    {
        Self::new(bytes)
    }
}
impl TryFrom<&[u8]> for TypeCode
{
    type Error = Error;

    fn try_from(bytes: &[u8]) -> Result<Self>
    {
        Ok(Self::new(bytes.try_into().map_err(|_| Error::BadArgument)?))
    }
}
impl From<TypeCode> for [u8; 4]
{
    fn from(code: TypeCode) -> Self
    {
        code.bytes()
    }
}
impl core::fmt::Display for TypeCode
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        for &byte in &self.0
        {
            if (b' '..=b'~').contains(&byte) && byte != b'\\'
            {
                write!(f, "{}", byte as char)?;
            }
            else
            {
                write!(f, "\\x{byte:02x}")?;
            }
        }
        Ok(())
    }
}

/// Immutable, checked UTF-8 owned by an information snapshot. Inline storage
/// keeps native clients usable without an application allocator. Padding and
/// transport lengths are private; equality compares only the actual string.
#[derive(Clone, Copy)]
pub struct InlineText<const N: usize>
{
    bytes:  [u8; N],
    length: usize
}
impl<const N: usize> InlineText<N>
{
    pub const EMPTY: Self = Self { bytes:  [0; N],
                                   length: 0 };

    pub fn from_reply(bytes: [u8; N], length: u32) -> Result<Self>
    {
        let length = length as usize;
        core::str::from_utf8(bytes.get(..length).ok_or(Error::BadAbi)?).map_err(|_| Error::BadAbi)?;
        Ok(Self { bytes,
                  length })
    }

    pub fn as_str(&self) -> &str
    {
        core::str::from_utf8(&self.bytes[..self.length]).expect("InlineText constructor validates \
                                                                 UTF-8")
    }

    pub fn as_bytes(&self) -> &[u8]
    {
        &self.bytes[..self.length]
    }

    pub const fn len(&self) -> usize
    {
        self.length
    }

    pub const fn is_empty(&self) -> bool
    {
        self.length == 0
    }
}
impl<const N: usize> Default for InlineText<N>
{
    fn default() -> Self
    {
        Self { bytes:  [0; N],
               length: 0 }
    }
}
impl<const N: usize> PartialEq for InlineText<N>
{
    fn eq(&self, other: &Self) -> bool
    {
        self.as_bytes() == other.as_bytes()
    }
}
impl<const N: usize> Eq for InlineText<N>
{
}
impl<const N: usize> core::fmt::Debug for InlineText<N>
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        self.as_str().fmt(f)
    }
}
impl<const N: usize> core::fmt::Display for InlineText<N>
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        f.write_str(self.as_str())
    }
}
impl<const N: usize> AsRef<str> for InlineText<N>
{
    fn as_ref(&self) -> &str
    {
        self.as_str()
    }
}

/// Define distinct native flag vocabularies without exposing transport records.
#[macro_export]
macro_rules! native_flags {
    ($name:ident, $word:ty; $($flag:ident = $value:expr),* $(,)?) => {
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
        pub struct $name($word);
        impl $name {
            $(pub const $flag: Self = Self($value);)*
            pub const NONE: Self = Self(0);
            pub const fn from_bits_retain(bits: $word) -> Self { Self(bits) }
            pub const fn bits(self) -> $word { self.0 }
            pub const fn is_empty(self) -> bool { self.0 == 0 }
            pub const fn contains(self, other: Self) -> bool { self.0 & other.0 == other.0 }
            pub const fn intersects(self, other: Self) -> bool { self.0 & other.0 != 0 }
        }
        impl core::ops::BitOr for $name { type Output = Self; fn bitor(self, rhs: Self) -> Self { Self(self.0 | rhs.0) } }
        impl core::ops::BitAnd for $name { type Output = Self; fn bitand(self, rhs: Self) -> Self { Self(self.0 & rhs.0) } }
        impl core::ops::BitXor for $name { type Output = Self; fn bitxor(self, rhs: Self) -> Self { Self(self.0 ^ rhs.0) } }
        impl core::ops::Not for $name { type Output = Self; fn not(self) -> Self { Self(!self.0) } }
        impl core::ops::BitOrAssign for $name { fn bitor_assign(&mut self, rhs: Self) { self.0 |= rhs.0; } }
        impl core::ops::BitAndAssign for $name { fn bitand_assign(&mut self, rhs: Self) { self.0 &= rhs.0; } }
        impl core::fmt::LowerHex for $name { fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result { core::fmt::LowerHex::fmt(&self.0, f) } }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageScope(i16);
impl ImageScope
{
    pub const fn from_raw(value: i16) -> Result<Self>
    {
        if value >= mac_abi::resources::LOADED_IMAGE_BASE
        {
            Ok(Self(value))
        }
        else
        {
            Err(Error::BadArgument)
        }
    }

    pub const fn raw(self) -> i16
    {
        self.0
    }
}
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceScope
{
    SearchChain,
    System,
    Application,
    Image(ImageScope)
}
impl ResourceScope
{
    pub const fn raw(self) -> i16
    {
        match self
        {
            Self::SearchChain => mac_abi::resources::CHAIN,
            Self::System => mac_abi::resources::SYSTEM,
            Self::Application => mac_abi::resources::APPLICATION,
            Self::Image(scope) => scope.raw()
        }
    }

    pub const fn from_raw(value: i16) -> Result<Self>
    {
        match value
        {
            mac_abi::resources::CHAIN => Ok(Self::SearchChain),
            mac_abi::resources::SYSTEM => Ok(Self::System),
            mac_abi::resources::APPLICATION => Ok(Self::Application),
            _ =>
            {
                match ImageScope::from_raw(value)
                {
                    Ok(scope) => Ok(Self::Image(scope)),
                    Err(error) => Err(error)
                }
            }
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceKey
{
    pub kind: TypeCode,
    pub id:   i16
}
impl ResourceKey
{
    pub const fn new(kind: TypeCode, id: i16) -> Self
    {
        Self { kind,
               id }
    }

    /// Convenient for compile-time resource manifests and exact FourCC literals.
    pub const fn from_bytes(kind: [u8; 4], id: i16) -> Self
    {
        Self::new(TypeCode::new(kind), id)
    }
}
impl From<mac_abi::ResourceKey> for ResourceKey
{
    fn from(key: mac_abi::ResourceKey) -> Self
    {
        Self::from_bytes(key.kind, key.id)
    }
}
impl From<ResourceKey> for mac_abi::ResourceKey
{
    fn from(key: ResourceKey) -> Self
    {
        Self { kind: key.kind.bytes(),
               id:   key.id }
    }
}

/// The two compact bitmap-font selectors used by definition drawing surfaces.
/// General font selection uses the Font Manager's family/size/face interface.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SurfaceFont
{
    #[default]
    Chicago12 = 0,
    Geneva9   = 1
}
#[repr(u32)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SurfaceOperation
{
    #[default]
    PaintRect   = 0,
    FrameRect   = 1,
    FillRect    = 2,
    InvertRect  = 3,
    TextWidth   = 4,
    FontMetrics = 5,
    DrawText    = 6
}

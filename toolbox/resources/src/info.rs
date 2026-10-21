//! Native resource snapshots; the resource-fork and trap layouts stay in System.
use mac_abi::resources as wire;
pub use mac_traps::types::{ImageScope, ResourceKey, ResourceScope};
use mac_traps::{Error, Result, types::InlineText};
pub const SYSTEM: ResourceScope = ResourceScope::System;
pub const APPLICATION: ResourceScope = ResourceScope::Application;
pub const CHAIN: ResourceScope = ResourceScope::SearchChain;
mac_traps::native_flags!(ResourceAttributes, u16;);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceInfo
{
    pub key:        ResourceKey,
    pub origin:     ResourceScope,
    pub attributes: ResourceAttributes,
    pub length:     usize,
    pub name:       InlineText<{ mac_abi::INLINE_STRING_BYTES }>
}
impl ResourceInfo
{
    pub fn name(&self) -> &str
    {
        self.name.as_str()
    }
}
impl TryFrom<wire::ResourceInfo> for ResourceInfo
{
    type Error = Error;

    fn try_from(info: wire::ResourceInfo) -> Result<Self>
    {
        Ok(Self { key:        info.key.into(),
                  origin:     ResourceScope::from_raw(info.origin).map_err(|_| Error::BadAbi)?,
                  attributes: ResourceAttributes::from_bits_retain(info.attributes),
                  length:     info.length,
                  name:       InlineText::from_reply(info.name, info.name_length)? })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VersionInfo
{
    pub number:  [u8; 4],
    pub country: u16,
    pub short:   InlineText<{ mac_abi::INLINE_STRING_BYTES }>,
    pub long:    InlineText<{ mac_abi::INLINE_STRING_BYTES }>
}
impl VersionInfo
{
    pub fn short(&self) -> &str
    {
        self.short.as_str()
    }

    pub fn long(&self) -> &str
    {
        self.long.as_str()
    }
}
impl TryFrom<wire::VersionInfo> for VersionInfo
{
    type Error = Error;

    fn try_from(info: wire::VersionInfo) -> Result<Self>
    {
        Ok(Self { number:  info.number,
                  country: info.country,
                  short:   InlineText::from_reply(info.short, info.short_length)?,
                  long:    InlineText::from_reply(info.long, info.long_length)? })
    }
}

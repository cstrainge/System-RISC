//! Thin bindings to the resident shared Scrap Manager. No clipboard data is
//! retained in this crate; every operation enters the native System service.
#![no_std]
#![forbid(unsafe_code)]
use mac_abi::scrap::*;
pub use mac_abi::scrap::{MAX_BYTES, MAX_FLAVORS};
pub use mac_traps::TypeCode;
pub const TEXT: TypeCode = TypeCode::new(*b"TEXT");
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrapInfo
{
    pub size:    u32,
    pub count:   u16,
    pub flavors: u16
}
impl From<mac_abi::scrap::ScrapInfo> for ScrapInfo
{
    fn from(info: mac_abi::scrap::ScrapInfo) -> Self
    {
        Self { size:    info.size,
               count:   info.count,
               flavors: info.flavors }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlavorInfo
{
    pub kind:   TypeCode,
    pub length: u32,
    pub offset: u32
}
impl From<mac_abi::scrap::FlavorInfo> for FlavorInfo
{
    fn from(info: mac_abi::scrap::FlavorInfo) -> Self
    {
        Self { kind:   info.kind.into(),
               length: info.length,
               offset: info.offset }
    }
}
pub use mac_traps::{Client, Error, Result};
#[derive(Clone, Copy)]
pub struct Scrap<'a>
{
    client: Client<'a>
}
impl<'a> Scrap<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client }
    }

    pub fn info(self) -> Result<ScrapInfo>
    {
        let mut call = ScrapCall { operation: INFO,
                                   ..Default::default() };
        self.client.scrap(&mut call, &[], &mut [])?;
        Ok(call.info.into())
    }

    /// Start a new clipboard generation and discard every flavor record.
    pub fn clear(self) -> Result<()>
    {
        self.perform(CLEAR)
    }

    /// Append one binary flavor record. TEXT must use `put_text`, including
    /// when the caller already holds UTF-8 bytes. Repeated kinds remain records.
    pub fn put(self, kind: TypeCode, bytes: &[u8]) -> Result<()>
    {
        let mut call = ScrapCall { operation: PUT,
                                   kind: kind.bytes(),
                                   ..Default::default() };
        self.client.scrap(&mut call, bytes, &mut [])
    }

    pub fn put_text(self, text: &str) -> Result<()>
    {
        let mut call = ScrapCall { operation: PUT_TEXT,
                                   ..Default::default() };
        self.client.scrap(&mut call, text.as_bytes(), &mut [])
    }

    /// The first matching binary record is read, as in original GetScrap.
    /// Partial reads and a caller-owned buffer are native extensions. TEXT is
    /// accessible only through `text`, which returns a checked Rust string.
    pub fn read(self, kind: TypeCode, offset: usize, output: &mut [u8]) -> Result<usize>
    {
        let mut call =
            ScrapCall { operation: READ,
                        kind: kind.bytes(),
                        offset: u32::try_from(offset).map_err(|_| Error::BadArgument)?,
                        ..Default::default() };
        self.client.scrap(&mut call, &[], output)?;
        Ok(call.transferred)
    }

    /// Read a complete native UTF-8 text flavor into caller-owned scratch.
    /// An undersized buffer fails without copying a partial string.
    pub fn text(self, output: &mut [u8]) -> Result<&str>
    {
        let mut call = ScrapCall { operation: READ_TEXT,
                                   ..Default::default() };
        self.client.scrap(&mut call, &[], output)?;
        core::str::from_utf8(output.get(..call.transferred).ok_or(Error::BadArgument)?)
            .map_err(|_| Error::BadArgument)
    }

    /// Enumerate records in their stored order; duplicate kinds are visible.
    pub fn flavor_at(self, index: usize) -> Result<FlavorInfo>
    {
        let mut call = ScrapCall { operation: FLAVOR_AT,
                                   index: u32::try_from(index).map_err(|_| Error::BadArgument)?,
                                   ..Default::default() };
        self.client.scrap(&mut call, &[], &mut [])?;
        Ok(call.flavor.into())
    }

    /// Disk-backed Scrap.dat awaits general writable file/fork services.
    pub fn load(self) -> Result<()>
    {
        self.perform(LOAD)
    }

    pub fn unload(self) -> Result<()>
    {
        self.perform(UNLOAD)
    }

    fn perform(self, operation: u32) -> Result<()>
    {
        self.client.scrap(&mut ScrapCall { operation,
                                           ..Default::default() },
                          &[],
                          &mut [])
    }
}

//! Thin bindings to the resident shared Scrap Manager. No clipboard data is
//! retained in this crate; every operation enters the native System service.
#![no_std]
#![forbid(unsafe_code)]
pub use mac_abi::scrap::{FlavorInfo, MAX_BYTES, MAX_FLAVORS, NO_TYPE_ERR, ScrapInfo, TEXT};
use mac_abi::{BAD_ARGUMENT, scrap::*};
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
        Ok(call.info)
    }

    /// Start a new clipboard generation and discard every flavor record.
    pub fn clear(self) -> Result<()>
    {
        self.perform(CLEAR)
    }

    /// Append one binary flavor record. TEXT must use `put_text`, including
    /// when the caller already holds UTF-8 bytes. Repeated kinds remain records.
    pub fn put(self, kind: [u8; 4], bytes: &[u8]) -> Result<()>
    {
        let mut call = ScrapCall { operation: PUT,
                                   kind,
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
    pub fn read(self, kind: [u8; 4], offset: usize, output: &mut [u8]) -> Result<usize>
    {
        let mut call =
            ScrapCall { operation: READ,
                        kind,
                        offset: u32::try_from(offset).map_err(|_| Error(BAD_ARGUMENT))?,
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
        core::str::from_utf8(output.get(..call.transferred).ok_or(Error(BAD_ARGUMENT))?)
            .map_err(|_| Error(BAD_ARGUMENT))
    }

    /// Enumerate records in their stored order; duplicate kinds are visible.
    pub fn flavor_at(self, index: usize) -> Result<FlavorInfo>
    {
        let mut call = ScrapCall { operation: FLAVOR_AT,
                                   index: u32::try_from(index).map_err(|_| Error(BAD_ARGUMENT))?,
                                   ..Default::default() };
        self.client.scrap(&mut call, &[], &mut [])?;
        Ok(call.flavor)
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

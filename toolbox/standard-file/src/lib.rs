//! Typed StandardGetFile/StandardPutFile over System-owned modal dialogs.
#![no_std]
#![forbid(unsafe_code)]
pub use mac_abi::files::CatalogInfo;
use mac_abi::{BAD_ARGUMENT, standard_file::*};
pub use mac_traps::{Client, Error, Result};
#[derive(Clone, Copy)]
pub struct StandardFile<'a>
{
    client: Client<'a>
}
#[derive(Clone, Copy, Default)]
pub struct Location
{
    pub volume:    u32,
    pub directory: u32
}
#[derive(Clone, Copy)]
pub enum TypeFilter<'a>
{
    All,
    Types(&'a [[u8; 4]])
}
/// A completed FSSpec-style choice. New files have id=0 until saved. The catalog
/// name is UTF8; `replacing` is true only after System's replacement confirmation.
#[derive(Clone, Copy, Debug)]
pub struct Selection
{
    pub entry:     CatalogInfo,
    pub replacing: bool
}
impl Selection
{
    pub fn name(&self) -> &str
    {
        self.entry.name()
    }

    pub fn location(&self) -> Location
    {
        Location { volume:    self.entry.volume,
                   directory: self.entry.parent }
    }
}
// Inline reply keeps this no_std interface usable without an application allocator.
#[allow(clippy::large_enum_variant)]
pub enum Progress
{
    Pending,
    Cancelled,
    Selected(Selection)
}
pub struct Dialog<'a>
{
    client: Client<'a>,
    handle: u32
}
impl<'a> StandardFile<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client }
    }

    pub fn begin_get(self, location: Location, filter: TypeFilter<'_>) -> Result<Dialog<'a>>
    {
        let mut call = StandardFileCall { volume: location.volume,
                                          directory: location.directory,
                                          ..Default::default() };
        if let TypeFilter::Types(types) = filter
        {
            if types.len() > MAX_TYPES
            {
                return Err(Error(BAD_ARGUMENT));
            }
            call.type_count = types.len() as i32;
            call.types[..types.len()].copy_from_slice(types);
        }
        self.begin(call)
    }

    /// StandardPutFile is used for both initial Save and Save As. No file is
    /// created until the caller commits a File Manager data writer.
    pub fn begin_put(self, location: Location, prompt: &str, default_name: &str)
                     -> Result<Dialog<'a>>
    {
        let mut call = StandardFileCall { mode: PUT,
                                          volume: location.volume,
                                          directory: location.directory,
                                          ..Default::default() };
        if prompt.len() > call.prompt.len() || default_name.len() > call.name.len()
        {
            return Err(Error(BAD_ARGUMENT));
        }
        call.prompt[..prompt.len()].copy_from_slice(prompt.as_bytes());
        call.prompt_length = prompt.len() as u32;
        call.name[..default_name.len()].copy_from_slice(default_name.as_bytes());
        call.name_length = default_name.len() as u32;
        self.begin(call)
    }

    fn begin(self, mut call: StandardFileCall) -> Result<Dialog<'a>>
    {
        self.client.standard_file(&mut call)?;
        Ok(Dialog { client: self.client,
                    handle: call.handle })
    }

    pub fn standard_get_file(self, location: Location, filter: TypeFilter<'_>)
                             -> Result<Option<Selection>>
    {
        self.begin_get(location, filter)?.wait()
    }

    pub fn standard_put_file(self, location: Location, prompt: &str, default_name: &str)
                             -> Result<Option<Selection>>
    {
        self.begin_put(location, prompt, default_name)?.wait()
    }

    pub fn save_as(self, location: Location, prompt: &str, default_name: &str)
                   -> Result<Option<Selection>>
    {
        self.standard_put_file(location, prompt, default_name)
    }
}
impl Dialog<'_>
{
    pub fn poll(&self) -> Result<Progress>
    {
        let mut call = StandardFileCall { operation: POLL,
                                          handle: self.handle,
                                          ..Default::default() };
        self.client.standard_file(&mut call)?;
        match call.state
        {
            PENDING => Ok(Progress::Pending),
            CANCELLED => Ok(Progress::Cancelled),
            ACCEPTED =>
            {
                Ok(Progress::Selected(Selection { entry:     call.entry,
                                                  replacing: call.replacing != 0 }))
            }
            _ => Err(Error(BAD_ARGUMENT))
        }
    }

    pub fn wait(self) -> Result<Option<Selection>>
    {
        loop
        {
            match self.poll()?
            {
                Progress::Pending =>
                {
                    self.client.wait_event(0, 1)?;
                }
                Progress::Cancelled => return Ok(None),
                Progress::Selected(selection) => return Ok(Some(selection))
            }
        }
    }
}
impl Drop for Dialog<'_>
{
    fn drop(&mut self)
    {
        let _ = self.client.standard_file(&mut StandardFileCall { operation: DISPOSE,
                                                                  handle: self.handle,
                                                                  ..Default::default() });
    }
}

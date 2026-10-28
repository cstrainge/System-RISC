//! Authoritative provider name preparation. The wire checks bounds/UTF-8;
//! filesystem-specific normalization and creation rules belong to the provider.
use super::{Arguments, Error, SCHEMA, get16, name, name_input, put16};

pub const PREPARED_NAME_BYTES: usize = 8;
/// FSYS catalog names already have a 255-byte display-name limit.
pub const MAX_PREPARED_NAME_BYTES: usize = PREPARED_NAME_BYTES + 255;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrepareName<'a>
{
    pub volume: u32,
    pub name:   &'a str
}
impl<'a> PrepareName<'a>
{
    pub fn arguments(self) -> Result<Arguments<'a>, Error>
    {
        if self.volume == 0
        {
            return Err(Error::Invalid);
        }
        name_input(self.name.as_bytes())?;
        Ok(Arguments { volume: self.volume,
                       payload: self.name.as_bytes(),
                       ..Default::default() })
    }

    pub fn from_arguments(arguments: Arguments<'a>) -> Result<Self, Error>
    {
        let value = Self { volume: arguments.volume,
                           name:   name_input(arguments.payload)? };
        if arguments != value.arguments()?
        {
            return Err(Error::Invalid);
        }
        Ok(value)
    }
}

/// The provider's canonical display name for a new file/folder. Successful
/// preparation validates naming rules, not collision absence or a future write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreparedName<'a>
{
    pub name: &'a str
}
impl<'a> PreparedName<'a>
{
    pub fn encode(self, output: &mut [u8]) -> Result<usize, Error>
    {
        name(self.name.as_bytes())?;
        let count = PREPARED_NAME_BYTES + self.name.len();
        let bytes = output.get_mut(..count).ok_or(Error::Length)?;
        bytes[..PREPARED_NAME_BYTES].fill(0);
        put16(bytes, 0, SCHEMA);
        put16(bytes, 2, PREPARED_NAME_BYTES as u16);
        put16(bytes, 4, self.name.len() as u16);
        bytes[PREPARED_NAME_BYTES..].copy_from_slice(self.name.as_bytes());
        Ok(count)
    }

    pub fn decode(bytes: &'a [u8]) -> Result<Self, Error>
    {
        if bytes.len() < PREPARED_NAME_BYTES || bytes.len() > MAX_PREPARED_NAME_BYTES
        {
            return Err(Error::Length);
        }
        if get16(bytes, 0) != SCHEMA
        {
            return Err(Error::Version);
        }
        if get16(bytes, 2) as usize != PREPARED_NAME_BYTES || get16(bytes, 6) != 0
        {
            return Err(Error::Invalid);
        }
        let length = get16(bytes, 4) as usize;
        if bytes.len() != PREPARED_NAME_BYTES + length
        {
            return Err(Error::Length);
        }
        Ok(Self { name: name(&bytes[PREPARED_NAME_BYTES..])? })
    }
}

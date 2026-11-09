//! Typed control-instance properties shared by resident storage, definitions,
//! dialog resources and the public SDK. Decoding borrows; it never allocates.
#![no_std]
#![forbid(unsafe_code)]
pub mod messages;
mod inventory;
mod codec;
mod types;
mod models;
pub use codec::{Codec, Cursor, Error, List, Result, Sink, field, field_range};
pub use models::*;
pub use types::*;

/// A property's format is part of the native ABI fingerprint, not a private
/// CDEF convention. No decoder for an older unreleased format is retained.
pub const FORMAT: u32 = 1;

pub trait Record<'a>: Copy
{
    fn read(bytes: &'a [u8]) -> Result<Self>;
    fn write(&self, output: &mut dyn Sink) -> Result<()>;
    fn validate(&self) -> Result<()>;
}
pub trait Model: 'static
{
    type View<'a>: Record<'a> + Default;
    const ID: i16;
    const NAME: &'static str;
    const FIELDS: &'static [&'static str];
    fn instance<'a>(variation: i16, bytes: &'a [u8]) -> Result<Self::View<'a>>
    {
        if variation != 0 && !(Self::ID == 25 && variation == 1)
        {
            return Err(Error::BadArgument);
        }
        Self::View::read(bytes)
    }
    fn replace<'a>(original: &'a [u8], index: usize, input: &'a [u8], output: &mut dyn Sink)
                   -> Result<()>;
}

include!(concat!(env!("OUT_DIR"), "/fingerprint.rs"));

pub mod fields;
pub trait Field
{
    type Model: Model;
    type Value<'a>: Codec<'a>;
    const INDEX: usize;
}

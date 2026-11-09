//! Fallible owning counterpart of the borrowing property views. Immutable
//! resident instances use the same bytes and decoder as the public SDK.
#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;
use alloc::vec::Vec;
use core::marker::PhantomData;

use mac_control_model::{self as model, Codec, Error, Model, Record, Result, Sink, field,
                        field_names, field_range};
#[derive(Clone)]
pub struct Buffer
{
    bytes: Vec<u8>
}
impl Buffer
{
    pub fn new() -> Self
    {
        Self { bytes: Vec::new() }
    }

    pub fn as_slice(&self) -> &[u8]
    {
        &self.bytes
    }

    pub fn into_vec(self) -> Vec<u8>
    {
        self.bytes
    }

    pub fn copy(bytes: &[u8]) -> Result<Self>
    {
        let mut value = Self::new();
        value.bytes(bytes)?;
        Ok(value)
    }
}
impl Default for Buffer
{
    fn default() -> Self
    {
        Self::new()
    }
}
impl Sink for Buffer
{
    fn bytes(&mut self, bytes: &[u8]) -> Result<()>
    {
        self.bytes.try_reserve(bytes.len()).map_err(|_| Error::NoMemory)?;
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}
pub struct Properties<M: Model>
{
    bytes:  Buffer,
    marker: PhantomData<M>
}
impl<M: Model> Properties<M>
{
    /// Build a complete configuration atomically. This permits related fields
    /// (for example range and value) to change together without an invalid
    /// intermediate single-property update.
    pub fn from_view(value: &M::View<'_>) -> Result<Self>
    {
        value.validate()?;
        let mut bytes = Buffer::new();
        value.write(&mut bytes)?;
        Ok(Self { bytes,
                  marker: PhantomData })
    }

    pub fn defaults() -> Result<Self>
    {
        let mut bytes = Buffer::new();
        M::View::default().write(&mut bytes)?;
        Ok(Self { bytes,
                  marker: PhantomData })
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self>
    {
        let view = M::View::read(bytes)?;
        let mut encoded = Buffer::new();
        view.write(&mut encoded)?;
        Ok(Self { bytes:  encoded,
                  marker: PhantomData })
    }

    pub fn view(&self) -> Result<M::View<'_>>
    {
        M::View::read(self.bytes.as_slice())
    }

    pub fn as_bytes(&self) -> &[u8]
    {
        self.bytes.as_slice()
    }

    pub fn set<'a, T: Codec<'a>>(&mut self, index: usize, value: &T) -> Result<()>
    {
        let mut encoded = Buffer::new();
        field(value, &mut encoded)?;
        self.bytes = replace(M::ID, self.as_bytes(), index, encoded.as_slice())?;
        Ok(())
    }
}
/// Encoded *field*, including its length prefix. This is a value, not a client
/// shadow of resident control state. SET merges against the current resident
/// record; unrelated concurrent property changes are not overwritten.
pub fn replace(id: i16, original: &[u8], index: usize, replacement: &[u8]) -> Result<Buffer>
{
    let mut bytes = Buffer::new();
    model::replace_property(id, original, index, replacement, &mut bytes)?;
    Ok(bytes)
}

/// Immutable validated instance storage. Canonical CTRL owns this value; async
/// drawing/reflow only clone its Arc. No caller pointer survives admission.
pub struct State
{
    kind:  i16,
    bytes: Buffer
}
impl State
{
    pub fn new(kind: i16, bytes: &[u8]) -> Result<Self>
    {
        model::validate(kind, bytes)?;
        let bytes = if bytes.is_empty()
        {
            let mut bytes = Buffer::new();
            model::write_defaults(kind, &mut bytes)?;
            bytes
        }
        else
        {
            Buffer::copy(bytes)?
        };
        Ok(Self { kind,
                  bytes })
    }

    pub fn defaults(kind: i16) -> Result<Self>
    {
        Self::new(kind, &[])
    }

    pub fn kind(&self) -> i16
    {
        self.kind
    }

    pub fn as_bytes(&self) -> &[u8]
    {
        self.bytes.as_slice()
    }

    pub fn replaced(&self, index: usize, value: &[u8]) -> Result<Self>
    {
        Ok(Self { kind:  self.kind,
                  bytes: replace(self.kind, self.as_bytes(), index, value)? })
    }

    pub fn field(&self, index: usize) -> Result<&[u8]>
    {
        let fields = field_names(self.kind).ok_or(Error::BadArgument)?;
        let range = field_range(self.as_bytes(), index, fields.len())?;
        Ok(&self.as_bytes()[range])
    }
}

#[path = "owned_accessors.rs"]
mod accessors;

/// Owned canonical list bytes for nested property arrays. Decoding borrows
/// this buffer and validates the selected element type; no source reference is
/// retained after construction.
pub struct ListBuffer
{
    bytes: Buffer
}
impl ListBuffer
{
    pub fn from_slice<'a, T: Codec<'a>>(values: &[T]) -> Result<Self>
    {
        let count = u32::try_from(values.len()).map_err(|_| Error::BadArgument)?;
        let mut payload = Buffer::new();
        for value in values
        {
            value.encode(&mut payload)?;
        }
        let length = u32::try_from(payload.as_slice().len()).map_err(|_| Error::BadArgument)?;
        let mut bytes = Buffer::new();
        count.encode(&mut bytes)?;
        length.encode(&mut bytes)?;
        bytes.bytes(payload.as_slice())?;
        Ok(Self { bytes })
    }

    pub fn view<'a, T: Codec<'a>>(&'a self) -> Result<model::List<'a, T>>
    {
        model::List::from_encoded(self.bytes.as_slice())
    }
}

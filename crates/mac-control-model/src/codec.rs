//! Length-delimited field codec. Field replacement changes only that property;
//! decoded slices cannot outlive the original immutable owned record.
use core::marker::PhantomData;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    BadArgument,
    NoMemory
}
pub type Result<T> = core::result::Result<T, Error>;
pub trait Sink
{
    fn bytes(&mut self, bytes: &[u8]) -> Result<()>;
}
pub struct Cursor<'a>
{
    bytes:    &'a [u8],
    position: usize
}
impl<'a> Cursor<'a>
{
    pub fn new(bytes: &'a [u8]) -> Self
    {
        Self { bytes,
               position: 0 }
    }

    pub fn take(&mut self, count: usize) -> Result<&'a [u8]>
    {
        let end = self.position.checked_add(count).ok_or(Error::BadArgument)?;
        let bytes = self.bytes.get(self.position..end).ok_or(Error::BadArgument)?;
        self.position = end;
        Ok(bytes)
    }

    pub fn finish(self) -> Result<()>
    {
        if self.position == self.bytes.len() { Ok(()) } else { Err(Error::BadArgument) }
    }

    pub fn field<T: Codec<'a>>(&mut self) -> Result<T>
    {
        let length = u32::decode(self)? as usize;
        let mut field = Cursor::new(self.take(length)?);
        let value = T::decode(&mut field)?;
        field.finish()?;
        Ok(value)
    }
}
pub trait Codec<'a>: Copy
{
    fn decode(input: &mut Cursor<'a>) -> Result<Self>;
    fn encode(&self, output: &mut dyn Sink) -> Result<()>;
}
macro_rules! scalar {
    ($($ty:ty),*) => {$(impl<'a> Codec<'a> for $ty {
        fn decode(input: &mut Cursor<'a>) -> Result<Self> {
            Ok(Self::from_le_bytes(input.take(core::mem::size_of::<Self>())?.try_into().map_err(|_| Error::BadArgument)?))
        }
        fn encode(&self, output: &mut dyn Sink) -> Result<()> { output.bytes(&self.to_le_bytes()) }
    })*};
}
scalar!(u8, u16, u32, u64, i16, i32, i64);
impl<'a> Codec<'a> for bool
{
    fn decode(input: &mut Cursor<'a>) -> Result<Self>
    {
        match u8::decode(input)?
        {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::BadArgument)
        }
    }

    fn encode(&self, output: &mut dyn Sink) -> Result<()>
    {
        (*self as u8).encode(output)
    }
}
impl<'a> Codec<'a> for f64
{
    fn decode(input: &mut Cursor<'a>) -> Result<Self>
    {
        let value = Self::from_bits(u64::decode(input)?);
        if value.is_finite() { Ok(value) } else { Err(Error::BadArgument) }
    }

    fn encode(&self, output: &mut dyn Sink) -> Result<()>
    {
        if !self.is_finite()
        {
            return Err(Error::BadArgument);
        }
        self.to_bits().encode(output)
    }
}
impl<'a> Codec<'a> for &'a str
{
    fn decode(input: &mut Cursor<'a>) -> Result<Self>
    {
        let length = u32::decode(input)? as usize;
        core::str::from_utf8(input.take(length)?).map_err(|_| Error::BadArgument)
    }

    fn encode(&self, output: &mut dyn Sink) -> Result<()>
    {
        u32::try_from(self.len()).map_err(|_| Error::BadArgument)?.encode(output)?;
        output.bytes(self.as_bytes())
    }
}
impl<'a, T: Codec<'a>> Codec<'a> for Option<T>
{
    fn decode(input: &mut Cursor<'a>) -> Result<Self>
    {
        if bool::decode(input)? { Ok(Some(T::decode(input)?)) } else { Ok(None) }
    }

    fn encode(&self, output: &mut dyn Sink) -> Result<()>
    {
        self.is_some().encode(output)?;
        if let Some(value) = self
        {
            value.encode(output)?
        }
        Ok(())
    }
}
#[derive(Clone, Copy)]
pub struct List<'a, T>
{
    bytes:  &'a [u8],
    count:  u32,
    marker: PhantomData<T>
}
impl<'a, T: Codec<'a>> List<'a, T>
{
    pub const fn empty() -> Self
    {
        Self { bytes:  &[],
               count:  0,
               marker: PhantomData }
    }

    pub fn len(self) -> usize
    {
        self.count as usize
    }

    pub fn is_empty(self) -> bool
    {
        self.count == 0
    }

    pub fn iter(self) -> Items<'a, T>
    {
        Items { cursor:    Cursor::new(self.bytes),
                remaining: self.count,
                marker:    PhantomData }
    }

    /// Accept the canonical list encoding, validating every element before use.
    pub fn from_encoded(bytes: &'a [u8]) -> Result<Self>
    {
        let mut input = Cursor::new(bytes);
        let value = Self::decode(&mut input)?;
        input.finish()?;
        Ok(value)
    }
}
pub struct Items<'a, T>
{
    cursor:    Cursor<'a>,
    remaining: u32,
    marker:    PhantomData<T>
}
impl<'a, T: Codec<'a>> Iterator for Items<'a, T>
{
    type Item = T;

    fn next(&mut self) -> Option<T>
    {
        if self.remaining == 0
        {
            return None;
        }
        self.remaining -= 1;
        T::decode(&mut self.cursor).ok()
    }
}
impl<'a, T: Codec<'a>> Codec<'a> for List<'a, T>
{
    fn decode(input: &mut Cursor<'a>) -> Result<Self>
    {
        let count = u32::decode(input)?;
        let length = u32::decode(input)? as usize;
        let bytes = input.take(length)?;
        if count as usize > length
        {
            return Err(Error::BadArgument);
        }
        let mut scan = Cursor::new(bytes);
        for _ in 0..count
        {
            T::decode(&mut scan)?;
        }
        scan.finish()?;
        Ok(Self { bytes,
                  count,
                  marker: PhantomData })
    }

    fn encode(&self, output: &mut dyn Sink) -> Result<()>
    {
        self.count.encode(output)?;
        u32::try_from(self.bytes.len()).map_err(|_| Error::BadArgument)?.encode(output)?;
        output.bytes(self.bytes)
    }
}
pub fn field<'a, T: Codec<'a>>(value: &T, output: &mut dyn Sink) -> Result<()>
{
    let mut count = Count(0);
    value.encode(&mut count)?;
    u32::try_from(count.0).map_err(|_| Error::BadArgument)?.encode(output)?;
    value.encode(output)
}
pub struct Count(pub usize);
impl Sink for Count
{
    fn bytes(&mut self, bytes: &[u8]) -> Result<()>
    {
        self.0 = self.0.checked_add(bytes.len()).ok_or(Error::BadArgument)?;
        Ok(())
    }
}
pub fn field_range(bytes: &[u8], field: usize, count: usize) -> Result<core::ops::Range<usize>>
{
    let mut input = Cursor::new(bytes);
    let mut selected = None;
    for index in 0..count
    {
        let start = input.position;
        let length = u32::decode(&mut input)? as usize;
        input.take(length)?;
        if index == field
        {
            selected = Some(start..input.position)
        }
    }
    input.finish()?;
    selected.ok_or(Error::BadArgument)
}

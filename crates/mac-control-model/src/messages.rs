//! Borrowed control protocol. Stubs accept requests and deliberately do nothing.
//! Notifications use the normal decoded event model; no stub emits them.
use crate::{Codec, Cursor, Error, List, Result, Sink};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextDelta<'a>
{
    pub start:    u64,
    pub old_end:  u64,
    pub new_text: &'a str
}
impl TextDelta<'_>
{
    /// Offsets name bytes in the document before this delta, never UTF-16 units.
    pub fn validate_against(&self, text: &str) -> Result<()>
    {
        let a = usize::try_from(self.start).map_err(|_| Error::BadArgument)?;
        let b = usize::try_from(self.old_end).map_err(|_| Error::BadArgument)?;
        if a > b || !text.is_char_boundary(a) || !text.is_char_boundary(b)
        {
            return Err(Error::BadArgument);
        }
        Ok(())
    }
}
impl<'a> Codec<'a> for TextDelta<'a>
{
    fn decode(i: &mut Cursor<'a>) -> Result<Self>
    {
        let v = Self { start:    u64::decode(i)?,
                       old_end:  u64::decode(i)?,
                       new_text: <&str>::decode(i)? };
        if v.start > v.old_end
        {
            return Err(Error::BadArgument);
        }
        Ok(v)
    }

    fn encode(&self, o: &mut dyn Sink) -> Result<()>
    {
        if self.start > self.old_end
        {
            return Err(Error::BadArgument);
        }
        self.start.encode(o)?;
        self.old_end.encode(o)?;
        self.new_text.encode(o)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataRequest
{
    LineCount,
    Lines,
    Piece
}
impl<'a> Codec<'a> for DataRequest
{
    fn decode(i: &mut Cursor<'a>) -> Result<Self>
    {
        match u8::decode(i)?
        {
            0 => Ok(Self::LineCount),
            1 => Ok(Self::Lines),
            2 => Ok(Self::Piece),
            _ => Err(Error::BadArgument)
        }
    }

    fn encode(&self, o: &mut dyn Sink) -> Result<()>
    {
        (*self as u8).encode(o)
    }
}
/// Requests carry no app pointers, callbacks or implied source authority.
#[derive(Clone, Copy)]
pub enum Request<'a>
{
    SetSelection
    {
        anchor: u64,
        caret:  u64
    },
    Text(&'a str),
    ApplyDeltas
    {
        transaction: u64,
        deltas:      List<'a, TextDelta<'a>>
    },
    Undo,
    Redo,
    Data
    {
        kind:     DataRequest,
        start:    u64,
        end:      u64,
        revision: u64
    },
    Supply
    {
        kind:     DataRequest,
        start:    u64,
        revision: u64,
        text:     &'a str
    },
    EnableHighlight
    {
        file_type: u32
    }
}
impl<'a> Request<'a>
{
    pub fn read(bytes: &'a [u8]) -> Result<Self>
    {
        let mut i = Cursor::new(bytes);
        let v = Self::decode(&mut i)?;
        i.finish()?;
        Ok(v)
    }
}
impl<'a> Codec<'a> for Request<'a>
{
    fn decode(i: &mut Cursor<'a>) -> Result<Self>
    {
        Ok(match u8::decode(i)?
        {
            0 =>
            {
                Self::SetSelection { anchor: u64::decode(i)?,
                                     caret:  u64::decode(i)? }
            }
            1 => Self::Text(<&str>::decode(i)?),
            2 =>
            {
                Self::ApplyDeltas { transaction: u64::decode(i)?,
                                    deltas:      List::decode(i)? }
            }
            3 => Self::Undo,
            4 => Self::Redo,
            5 =>
            {
                let kind = DataRequest::decode(i)?;
                let start = u64::decode(i)?;
                let end = u64::decode(i)?;
                let revision = u64::decode(i)?;
                if start > end
                {
                    return Err(Error::BadArgument);
                }
                Self::Data { kind,
                             start,
                             end,
                             revision }
            }
            6 =>
            {
                Self::Supply { kind:     DataRequest::decode(i)?,
                               start:    u64::decode(i)?,
                               revision: u64::decode(i)?,
                               text:     <&str>::decode(i)? }
            }
            7 => Self::EnableHighlight { file_type: u32::decode(i)? },
            _ => return Err(Error::BadArgument)
        })
    }

    fn encode(&self, o: &mut dyn Sink) -> Result<()>
    {
        match *self
        {
            Self::SetSelection { anchor,
                                 caret } =>
            {
                0u8.encode(o)?;
                anchor.encode(o)?;
                caret.encode(o)
            }
            Self::Text(text) =>
            {
                1u8.encode(o)?;
                text.encode(o)
            }
            Self::ApplyDeltas { transaction,
                                deltas } =>
            {
                2u8.encode(o)?;
                transaction.encode(o)?;
                deltas.encode(o)
            }
            Self::Undo => 3u8.encode(o),
            Self::Redo => 4u8.encode(o),
            Self::Data { kind,
                         start,
                         end,
                         revision } =>
            {
                if start > end
                {
                    return Err(Error::BadArgument);
                }
                5u8.encode(o)?;
                kind.encode(o)?;
                start.encode(o)?;
                end.encode(o)?;
                revision.encode(o)
            }
            Self::Supply { kind,
                           start,
                           revision,
                           text } =>
            {
                6u8.encode(o)?;
                kind.encode(o)?;
                start.encode(o)?;
                revision.encode(o)?;
                text.encode(o)
            }
            Self::EnableHighlight { file_type } =>
            {
                7u8.encode(o)?;
                file_type.encode(o)
            }
        }
    }
}

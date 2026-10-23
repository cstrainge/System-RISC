//! Bounded group-placement requests. IDs are persistent catalog identities;
//! the request's separate resident token owns the live transaction.
use super::*;
pub const ICON_MOVE_BYTES: usize = 20;
pub const MAX_ICON_MOVES: usize = 1024;
pub const MAX_ICON_MOVE_BYTES: usize = 4 + ICON_MOVE_BYTES * MAX_ICON_MOVES;
pub const ICON_MOVE_PROGRESS_BYTES: usize = 16;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum IconMovePhase
{
    Applying  = 0,
    Committed = 1,
    Undoing   = 2,
    Aborted   = 3
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IconMoveProgress
{
    pub phase:     IconMovePhase,
    pub completed: u32,
    pub total:     u32,
    pub changed:   bool
}
impl IconMoveProgress
{
    pub fn from_words(words: [u32; 4]) -> Result<Self, Error>
    {
        let phase = match words[0]
        {
            0 => IconMovePhase::Applying,
            1 => IconMovePhase::Committed,
            2 => IconMovePhase::Undoing,
            3 => IconMovePhase::Aborted,
            _ => return Err(Error::Invalid)
        };
        if words[2] > MAX_ICON_MOVES as u32
           || words[1] > words[2]
           || words[3] > 1
           || (phase == IconMovePhase::Committed && words[1] != words[2])
           || (phase == IconMovePhase::Aborted && words[1] != 0)
           || (phase != IconMovePhase::Committed && words[3] != 0)
        {
            return Err(Error::Invalid);
        }
        Ok(Self { phase,
                  completed: words[1],
                  total: words[2],
                  changed: words[3] != 0 })
    }

    pub fn words(self) -> [u32; 4]
    {
        [self.phase as u32, self.completed, self.total, u32::from(self.changed)]
    }

    pub fn encode(self, output: &mut [u8]) -> Result<(), Error>
    {
        if output.len() != ICON_MOVE_PROGRESS_BYTES
        {
            return Err(Error::Length);
        }
        let words = self.words();
        Self::from_words(words)?;
        for (at, word) in words.into_iter().enumerate()
        {
            put32(output, at * 4, word);
        }
        Ok(())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Error>
    {
        if bytes.len() != ICON_MOVE_PROGRESS_BYTES
        {
            return Err(Error::Length);
        }
        Self::from_words([get32(bytes, 0), get32(bytes, 4), get32(bytes, 8), get32(bytes, 12)])
    }

    pub fn terminal(self) -> bool
    {
        matches!(self.phase, IconMovePhase::Committed | IconMovePhase::Aborted)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IconMove
{
    pub id:              u32,
    pub expected_parent: u32,
    pub parent:          u32,
    /// Bits0..1 keep/set/clear placement; bit2 records a Trash move.
    pub placement:       u32,
    pub horizontal:      i16,
    pub vertical:        i16
}
impl IconMove
{
    pub fn validate(&self) -> Result<(), Error>
    {
        if self.id <= 2
           || self.expected_parent < 2
           || self.parent < 2
           || self.placement & !7 != 0
           || self.placement & 3 > 2
           || self.placement & 3 == 1 && self.horizontal == -1 && self.vertical == -1
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }

    pub fn encode(&self, output: &mut [u8]) -> Result<(), Error>
    {
        self.validate()?;
        if output.len() != ICON_MOVE_BYTES
        {
            return Err(Error::Length);
        }
        put32(output, 0, self.id);
        put32(output, 4, self.expected_parent);
        put32(output, 8, self.parent);
        put32(output, 12, self.placement);
        put16(output, 16, self.horizontal as u16);
        put16(output, 18, self.vertical as u16);
        Ok(())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Error>
    {
        if bytes.len() != ICON_MOVE_BYTES
        {
            return Err(Error::Length);
        }
        let result = Self { id:              get32(bytes, 0),
                            expected_parent: get32(bytes, 4),
                            parent:          get32(bytes, 8),
                            placement:       get32(bytes, 12),
                            horizontal:      get16(bytes, 16) as i16,
                            vertical:        get16(bytes, 18) as i16 };
        result.validate()?;
        Ok(result)
    }
}
#[derive(Clone, Copy)]
pub struct IconMoves<'a>
{
    bytes: &'a [u8]
}
impl<'a> IconMoves<'a>
{
    pub fn decode(bytes: &'a [u8]) -> Result<Self, Error>
    {
        if bytes.len() < 4 || bytes.len() > MAX_ICON_MOVE_BYTES
        {
            return Err(Error::Length);
        }
        let count = get32(bytes, 0) as usize;
        if count > MAX_ICON_MOVES || bytes.len() != 4 + count * ICON_MOVE_BYTES
        {
            return Err(Error::Length);
        }
        let rows = Self { bytes: &bytes[4..] };
        for row in rows.bytes.chunks_exact(ICON_MOVE_BYTES)
        {
            IconMove::decode(row)?;
        }
        Ok(rows)
    }

    pub fn len(self) -> usize
    {
        self.bytes.len() / ICON_MOVE_BYTES
    }

    pub fn is_empty(self) -> bool
    {
        self.bytes.is_empty()
    }

    pub fn at(self, index: usize) -> Option<IconMove>
    {
        let at = index.checked_mul(ICON_MOVE_BYTES)?;
        IconMove::decode(self.bytes.get(at..at.checked_add(ICON_MOVE_BYTES)?)?).ok()
    }
}

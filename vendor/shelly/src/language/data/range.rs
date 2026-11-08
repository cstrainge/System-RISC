
use std::fmt::{ self, Display, Formatter };

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Range
{
    pub start: Option<i64>,
    pub end: Option<i64>,
    pub inclusive: bool
}


impl Range
{
    pub fn contains(&self, value: i64) -> bool
    {
           self.start.is_none_or(|start| value >= start)
        && self.end.is_none_or(|end| if self.inclusive { value <= end } else { value < end })
    }

    pub fn is_empty(&self) -> bool
    {
        match (self.start, self.end)
        {
            (Some(start), Some(end)) => if self.inclusive { start > end } else { start >= end },
            _ => false
        }
    }

    // Omitted bounds remain unspecified; only a consumer can supply their meaning.
    pub fn len(&self) -> Option<u128>
    {
        let (start, end) = (self.start?, self.end?);
        Some((end as i128 - start as i128 + i128::from(self.inclusive)).max(0) as u128)
    }

    pub fn iter(&self) -> Option<RangeIterator>
    {
        Some(RangeIterator { next: Some(self.start?), end: self.end, inclusive: self.inclusive })
    }
}


pub struct RangeIterator
{
    next: Option<i64>,
    end: Option<i64>,
    inclusive: bool
}


impl Iterator for RangeIterator
{
    type Item = i64;

    fn next(&mut self) -> Option<i64>
    {
        let value = self.next?;
        if self.end.is_some_and(|end| if self.inclusive { value > end } else { value >= end })
        {
            self.next = None;
            return None;
        }
        self.next = value.checked_add(1);
        Some(value)
    }
}


impl Display for Range
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result
    {
        if let Some(start) = self.start { write!(f, "{}", start)?; }
        write!(f, "{}", if self.inclusive { "..=" } else { ".." })?;
        if let Some(end) = self.end { write!(f, "{}", end)?; }
        Ok(())
    }
}

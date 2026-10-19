//! System 7 Standard File DLOG/DITL geometry; resource data owns the layout.
use alloc::{string::String, vec::Vec};

use mac_abi::{BAD_ARGUMENT, NO_MEMORY, Rect, Status};
pub struct Item
{
    pub bounds: Rect,
    pub kind:   u8,
    pub label:  String
}
pub struct Layout
{
    pub bounds:    Rect,
    pub procedure: i16,
    pub items:     Vec<Item>
}
fn word(b: &[u8], i: usize) -> Result<i16, Status>
{
    Ok(i16::from_be_bytes(b.get(i..i + 2)
                           .ok_or(BAD_ARGUMENT)?
                           .try_into()
                           .map_err(|_| BAD_ARGUMENT)?))
}
fn rect(b: &[u8], i: usize) -> Result<Rect, Status>
{
    Ok(Rect::new(word(b, i + 2)?, word(b, i)?, word(b, i + 6)?, word(b, i + 4)?))
}
impl Layout
{
    pub fn parse(dialog: &[u8], list: &[u8]) -> Result<Self, Status>
    {
        let bounds = rect(dialog, 0)?;
        let count = word(list, 0)?.checked_add(1).ok_or(BAD_ARGUMENT)?;
        if bounds.is_empty() || !(1..=32).contains(&count)
        {
            return Err(BAD_ARGUMENT);
        }
        let mut items = Vec::new();
        items.try_reserve_exact(count as usize).map_err(|_| NO_MEMORY)?;
        let mut at = 2;
        for _ in 0..count
        {
            let bounds = rect(list, at + 4)?;
            let kind = *list.get(at + 12).ok_or(BAD_ARGUMENT)?;
            let len = usize::from(*list.get(at + 13).ok_or(BAD_ARGUMENT)?);
            let data = list.get(at + 14..at + 14 + len).ok_or(BAD_ARGUMENT)?;
            let label = if matches!(kind & 0x7f, 4 | 8 | 16)
            {
                mac_formats::text::decode(data)
            }
            else
            {
                String::new()
            };
            items.push(Item { bounds,
                              kind,
                              label });
            at = (at + 14 + len).next_multiple_of(2);
        }
        Ok(Self { bounds,
                  procedure: word(dialog, 8)?,
                  items })
    }

    pub fn item(&self, n: usize) -> Rect
    {
        self.items.get(n - 1).map_or(Rect::ZERO, |i| i.bounds)
    }
}

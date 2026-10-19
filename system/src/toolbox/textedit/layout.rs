//! Chicago 12 layout. All offsets remain UTF-8 boundaries.
use alloc::vec::Vec;

use mac_abi::{NO_MEMORY, Status, textedit::LineInfo};

use crate::toolbox::quickdraw::font::CHICAGO_12;
pub(super) fn width(text: &str) -> i16
{
    CHICAGO_12.width(text)
}
pub(super) fn lines(text: &str, maximum: i32) -> Result<Vec<LineInfo>, Status>
{
    let mut result = Vec::new();
    result.try_reserve_exact(text.chars().count().saturating_add(1)).map_err(|_| NO_MEMORY)?;
    let mut start = 0;
    while start < text.len()
    {
        let mut end = start;
        let mut advance = 0i32;
        let mut last_space = None;
        for (relative, character) in text[start..].char_indices()
        {
            let at = start + relative;
            if character == '\r'
            {
                end = at + 1;
                break;
            }
            let next = advance + i32::from(width(character.encode_utf8(&mut [0; 4])));
            if next > maximum.max(1)
               && at > start
               && (character != ' ' || (advance > maximum.max(1) && last_space.is_none()))
            {
                end = last_space.unwrap_or(at);
                break;
            }
            advance = next;
            end = at + character.len_utf8();
            if character == ' '
            {
                last_space = Some(end);
            }
        }
        let visible = text[start..end].strip_suffix('\r').unwrap_or(&text[start..end]);
        result.push(LineInfo { start:    start as u32,
                               end:      end as u32,
                               width:    width(visible),
                               reserved: 0 });
        start = end;
    }
    Ok(result)
}

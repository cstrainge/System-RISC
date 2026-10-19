//! Per-editor resident font advances. All offsets remain UTF-8 boundaries.
use alloc::vec::Vec;

use mac_abi::{BAD_ARGUMENT, NO_MEMORY, Status,
              quickdraw::{FontMetrics, FontSelection},
              textedit::LineInfo};

use crate::toolbox::quickdraw::{font::CHICAGO_12, fonts::FontManager};
fn resident(selection: FontSelection) -> crate::toolbox::quickdraw::fonts::FontSelection
{
    crate::toolbox::quickdraw::fonts::FontSelection { family: selection.family,
                                                      size:   selection.size,
                                                      face:   selection.face }
}

/// The default path deliberately retains the original captured Chicago metrics.
/// Selected fonts resolve metrics once and cache only characters actually used.
#[derive(Default)]
pub(super) struct Font
{
    selected: Option<FontSelection>,
    metrics:  FontMetrics,
    advances: Vec<(char, i16)>
}
impl Font
{
    pub fn new(selection: FontSelection, text: &str, fonts: &FontManager) -> Result<Self, Status>
    {
        let measured = fonts.metrics(resident(selection))?;
        let metrics = FontMetrics { ascent:    measured.ascent,
                                    descent:   measured.descent,
                                    leading:   measured.leading,
                                    max_width: measured.max_width };
        let height =
            i32::from(metrics.ascent) + i32::from(metrics.descent) + i32::from(metrics.leading);
        if metrics.ascent < 0 || height <= 0 || height > i32::from(i16::MAX)
        {
            return Err(BAD_ARGUMENT);
        }
        let mut font = Self { selected: Some(selection),
                              metrics,
                              advances: Vec::new() };
        font.prepare(text, fonts)?;
        Ok(font)
    }

    pub fn selection(&self) -> FontSelection
    {
        self.selected.unwrap_or_default()
    }

    pub fn ascent(&self) -> i16
    {
        if self.selected.is_none() { 12 } else { self.metrics.ascent }
    }

    pub fn line_height(&self) -> i16
    {
        if self.selected.is_none()
        {
            16
        }
        else
        {
            (i32::from(self.metrics.ascent)
             + i32::from(self.metrics.descent)
             + i32::from(self.metrics.leading)) as i16
        }
    }

    pub fn prepare(&mut self, text: &str, fonts: &FontManager) -> Result<(), Status>
    {
        let Some(selection) = self.selected
        else
        {
            return Ok(());
        };
        let mut missing = Vec::new();
        for character in text.chars()
        {
            if character == '\r'
               || self.advances.binary_search_by_key(&character, |&(c, _)| c).is_ok()
            {
                continue;
            }
            missing.try_reserve(1).map_err(|_| NO_MEMORY)?;
            missing.push(character);
        }
        missing.sort_unstable();
        missing.dedup();
        let mut measured = Vec::new();
        measured.try_reserve_exact(missing.len()).map_err(|_| NO_MEMORY)?;
        for character in missing
        {
            let width = fonts.text_width(resident(selection), character.encode_utf8(&mut [0; 4]))?;
            measured.push((character, width));
        }
        self.advances.try_reserve_exact(measured.len()).map_err(|_| NO_MEMORY)?;
        self.advances.extend(measured);
        self.advances.sort_unstable_by_key(|&(c, _)| c);
        Ok(())
    }

    pub fn try_clone(&self) -> Result<Self, Status>
    {
        let mut advances = Vec::new();
        advances.try_reserve_exact(self.advances.len()).map_err(|_| NO_MEMORY)?;
        advances.extend_from_slice(&self.advances);
        Ok(Self { selected: self.selected,
                  metrics: self.metrics,
                  advances })
    }

    pub fn width(&self, text: &str) -> i16
    {
        if self.selected.is_none()
        {
            return CHICAGO_12.width(text);
        }
        text.chars().fold(0i16, |total, character| {
                        let width = self.advances
                                        .binary_search_by_key(&character, |&(c, _)| c)
                                        .ok()
                                        .map_or(0, |index| self.advances[index].1);
                        total.saturating_add(width)
                    })
    }
}
pub(super) fn lines(text: &str, maximum: i32, font: &Font) -> Result<Vec<LineInfo>, Status>
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
            let next = advance + i32::from(font.width(character.encode_utf8(&mut [0; 4])));
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
                               width:    font.width(visible),
                               reserved: 0 });
        start = end;
    }
    Ok(result)
}

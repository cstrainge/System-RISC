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
    selected:       Option<FontSelection>,
    metrics:        FontMetrics,
    advances:       Vec<(char, i16)>,
    pub monospaced: bool
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
                              advances: Vec::new(),
                              monospaced: fonts.is_monospaced(resident(selection))? };
        font.prepare(" 0123456789", fonts)?;
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
                  advances,
                  monospaced: self.monospaced })
    }

    pub fn width_wide(&self, text: &str) -> u32
    {
        text.chars().map(|c| u32::from(self.width(c.encode_utf8(&mut [0; 4])).max(0) as u16)).sum()
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
#[derive(Clone, Copy, Default)]
pub(in crate::toolbox) struct Line
{
    pub classic: LineInfo,
    pub extent:  u32,
    pub logical: u32,
    pub first:   bool
}
impl core::ops::Deref for Line
{
    type Target = LineInfo;

    fn deref(&self) -> &LineInfo
    {
        &self.classic
    }
}
impl core::ops::DerefMut for Line
{
    fn deref_mut(&mut self) -> &mut LineInfo
    {
        &mut self.classic
    }
}

/// A proposed splice is read without copying or mutating the document. Layout
/// allocations can fail without leaving the editor partly changed.
pub(super) struct Splice<'a>
{
    pub prefix: &'a str,
    pub input:  &'a str,
    pub suffix: &'a str
}
impl Splice<'_>
{
    fn len(&self) -> usize
    {
        self.prefix.len() + self.input.len() + self.suffix.len()
    }

    fn chars(&self, start: usize) -> impl Iterator<Item = (usize, char)> + '_
    {
        let middle = self.prefix.len();
        let tail = middle + self.input.len();
        fn part(text: &str, base: usize, start: usize) -> impl Iterator<Item = (usize, char)> + '_
        {
            let at = start.saturating_sub(base).min(text.len());
            text[at..].char_indices().map(move |(offset, ch)| (base + at + offset, ch))
        }
        part(self.prefix, 0, start).chain(part(self.input, middle, start))
                                   .chain(part(self.suffix, tail, start))
    }

    fn byte(&self, at: usize) -> Option<u8>
    {
        if at < self.prefix.len()
        {
            return self.prefix.as_bytes().get(at).copied();
        }
        let at = at - self.prefix.len();
        if at < self.input.len()
        {
            return self.input.as_bytes().get(at).copied();
        }
        self.suffix.as_bytes().get(at - self.input.len()).copied()
    }

    fn line(&self, start: usize, maximum: i32, font: &Font, logical: u32) -> Line
    {
        let mut end = start;
        let mut advance = 0i32;
        let mut last_space = None;
        for (at, character) in self.chars(start)
        {
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
                if let Some((offset, width)) = last_space
                {
                    end = offset;
                    advance = width;
                }
                else
                {
                    end = at;
                }
                break;
            }
            advance = next;
            end = at + character.len_utf8();
            if character == ' '
            {
                last_space = Some((end, advance));
            }
        }
        Line { classic: LineInfo { start:    start as u32,
                                   end:      end as u32,
                                   width:    advance.min(i32::from(i16::MAX)) as i16,
                                   reserved: 0 },
               extent: advance.max(0) as u32,
               logical,
               first: start == 0 || self.byte(start - 1) == Some(b'\r') }
    }
}
pub(super) fn lines(text: &str, maximum: i32, font: &Font) -> Result<Vec<Line>, Status>
{
    let source = Splice { prefix: text,
                          input:  "",
                          suffix: "" };
    let mut result = Vec::new();
    let mut start = 0;
    let mut logical = 1;
    while start < text.len()
    {
        result.try_reserve(1).map_err(|_| NO_MEMORY)?;
        let line = source.line(start, maximum, font, logical);
        logical += u32::from(source.byte(line.end as usize - 1) == Some(b'\r'));
        start = line.end as usize;
        result.push(line);
    }
    Ok(result)
}

/// Rewrap from the touched line until an unchanged line start resynchronizes.
/// Suffix glyphs are never measured. The caller shifts cached offsets only.
pub(super) fn replacement(text: &str, old: &[Line], start: usize, end: usize, input: &str,
                          maximum: i32, font: &Font)
                          -> Result<(usize, usize, Vec<Line>), Status>
{
    let source = Splice { prefix: &text[..start],
                          input,
                          suffix: &text[end..] };
    let mut first = old.partition_point(|line| (line.end as usize) < start);
    // The preceding soft row examined this row's first word before wrapping.
    // Shortening that word can pull it back, even when the edit lies inside it.
    if first > 0 && old.get(first).is_some_and(|line| !line.first)
    {
        first -= 1;
    }
    let mut at = old.get(first).map_or(0, |line| line.start as usize);
    let mut logical = old.get(first).map_or(1, |line| line.logical);
    let delta = input.len() as i64 - (end - start) as i64;
    let mut result = Vec::new();
    let mut resume = old.len();
    loop
    {
        if at >= start + input.len()
        {
            let original = (at as i64 - delta) as usize;
            if original >= end
            {
                let index = old.partition_point(|line| (line.start as usize) < original);
                if index > first
                   && old.get(index).is_some_and(|line| {
                                        line.start as usize == original
                                        && line.first
                                           == (at == 0 || source.byte(at - 1) == Some(b'\r'))
                                    })
                {
                    resume = index;
                    break;
                }
            }
        }
        if at >= source.len()
        {
            break;
        }
        result.try_reserve(1).map_err(|_| NO_MEMORY)?;
        let line = source.line(at, maximum, font, logical);
        logical += u32::from(source.byte(line.end as usize - 1) == Some(b'\r'));
        at = line.end as usize;
        result.push(line);
    }
    Ok((first, resume, result))
}

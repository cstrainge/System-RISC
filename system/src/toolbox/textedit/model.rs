use alloc::{string::String, vec::Vec};

use mac_abi::{textedit::*, *};

use super::layout;

pub struct Editor
{
    text:   String,
    lines:  Vec<LineInfo>,
    info:   EditorInfo,
    anchor: u32,
    blink:  Option<u32>
}
impl Editor
{
    pub fn new(dest: Rect, view: Rect) -> Result<Self, Status>
    {
        if dest.is_empty() || view.is_empty()
        {
            return Err(BAD_ARGUMENT);
        }
        let lines = layout::lines("", i32::from(dest.right) - i32::from(dest.left))?;
        Ok(Self { text: String::new(),
                  lines,
                  info: EditorInfo { dest,
                                     view,
                                     line_height: 16,
                                     ascent: 12,
                                     line_count: 0,
                                     ..Default::default() },
                  anchor: 0,
                  blink: None })
    }

    pub fn text(&self) -> &str
    {
        &self.text
    }

    pub fn lines(&self) -> &[LineInfo]
    {
        &self.lines
    }

    pub fn info(&self) -> EditorInfo
    {
        self.info
    }

    fn boundary(&self, index: u32) -> Result<usize, Status>
    {
        let at = index as usize;
        if at <= self.text.len() && self.text.is_char_boundary(at)
        {
            Ok(at)
        }
        else
        {
            Err(BAD_ARGUMENT)
        }
    }

    fn changed(&mut self)
    {
        self.info.revision = self.info.revision.wrapping_add(1);
        self.info.caret_visible = 0;
        self.blink = None;
    }

    pub fn select(&mut self, start: u32, end: u32) -> Result<(), Status>
    {
        let start = start.min(self.text.len() as u32);
        let end = end.min(self.text.len() as u32);
        self.boundary(start)?;
        self.boundary(end)?;
        self.info.selection_start = start.min(end);
        self.info.selection_end = end;
        self.anchor = start.min(end);
        self.changed();
        Ok(())
    }

    pub fn replace(&mut self, start: u32, end: u32, input: &str) -> Result<(), Status>
    {
        let start = self.boundary(start)?;
        let end = self.boundary(end)?;
        if start > end
        {
            return Err(BAD_ARGUMENT);
        }
        let length = self.text.len() - (end - start) + input.len();
        if length > MAX_BYTES
        {
            return Err(NO_MEMORY);
        }
        let mut text = String::new();
        text.try_reserve_exact(length).map_err(|_| NO_MEMORY)?;
        text.push_str(&self.text[..start]);
        text.push_str(input);
        text.push_str(&self.text[end..]);
        let lines =
            layout::lines(&text, i32::from(self.info.dest.right) - i32::from(self.info.dest.left))?;
        self.text = text;
        self.lines = lines;
        self.info.text_length = length as u32;
        self.info.line_count = self.lines.len() as u32;
        self.info.selection_start = (start + input.len()) as u32;
        self.info.selection_end = self.info.selection_start;
        self.anchor = self.info.selection_start;
        self.changed();
        Ok(())
    }

    pub fn prepared_replace(&self, input: &str) -> Result<Self, Status>
    {
        let mut next = Self { text:   String::new(),
                              lines:  Vec::new(),
                              info:   self.info,
                              anchor: self.anchor,
                              blink:  self.blink };
        next.text.try_reserve_exact(self.text.len()).map_err(|_| NO_MEMORY)?;
        next.text.push_str(&self.text);
        next.replace(self.info.selection_start, self.info.selection_end, input)?;
        Ok(next)
    }

    pub fn set_rects(&mut self, dest: Rect, view: Rect) -> Result<(), Status>
    {
        if dest.is_empty() || view.is_empty()
        {
            return Err(BAD_ARGUMENT);
        }
        let lines = layout::lines(&self.text, i32::from(dest.right) - i32::from(dest.left))?;
        self.lines = lines;
        self.info.dest = dest;
        self.info.view = view;
        self.info.line_count = self.lines.len() as u32;
        self.changed();
        Ok(())
    }

    pub fn set_text(&mut self, text: &str) -> Result<(), Status>
    {
        self.replace(0, self.text.len() as u32, text)?;
        Ok(())
    }

    pub fn insert(&mut self, text: &str) -> Result<(), Status>
    {
        let start = self.info.selection_start;
        let end = self.info.selection_end;
        self.replace(start, start, text)?;
        self.info.selection_end = end + text.len() as u32;
        Ok(())
    }

    pub fn delete(&mut self) -> Result<(), Status>
    {
        if self.info.selection_start == self.info.selection_end
        {
            return Ok(());
        }
        self.replace(self.info.selection_start, self.info.selection_end, "")
    }

    pub fn selection(&self) -> &str
    {
        &self.text[self.info.selection_start as usize..self.info.selection_end as usize]
    }

    pub fn point(&self, index: u32) -> Result<Point, Status>
    {
        let at = self.boundary(index)?;
        if self.lines.is_empty()
        {
            return Ok(Point { h: self.info.dest.left.saturating_add(1),
                              v: self.info.dest.top.saturating_add(self.info.line_height) });
        }
        let row = self.lines
                      .iter()
                      .position(|line| index < line.end || line.start == line.end)
                      .unwrap_or(self.lines.len() - 1);
        let line = self.lines[row];
        let text = &self.text[line.start as usize..at];
        let text = if text.ends_with('\r') { "" } else { text };
        Ok(Point { h: clamp(i32::from(self.info.dest.left) + 1 + i32::from(layout::width(text))),
                   v: clamp(i32::from(self.info.dest.top)
                            + (row as i32 + 1) * i32::from(self.info.line_height)) })
    }

    pub fn offset(&self, point: Point) -> u32
    {
        let relative = i32::from(point.v) - i32::from(self.info.dest.top);
        if relative < 0
        {
            return 0;
        }
        let row = (relative / i32::from(self.info.line_height)) as usize;
        let Some(line) = self.lines.get(row)
        else
        {
            return self.text.len() as u32;
        };
        let mut x = i32::from(self.info.dest.left) + 1;
        for (relative, c) in self.text[line.start as usize..line.end as usize].char_indices()
        {
            let at = line.start + relative as u32;
            if c == '\r'
            {
                return at;
            }
            let width = i32::from(layout::width(c.encode_utf8(&mut [0; 4])));
            if i32::from(point.h) <= x + width / 2
            {
                return at;
            }
            x += width;
        }
        if self.text.as_bytes().get(line.end.saturating_sub(1) as usize) == Some(&b'\r')
        {
            line.end - 1
        }
        else
        {
            line.end
        }
    }

    pub fn key(&mut self, c: char) -> Result<(), Status>
    {
        let start = self.info.selection_start;
        let end = self.info.selection_end;
        match c
        {
            '\u{8}' =>
            {
                let previous = if start == end && start > 0
                {
                    self.text[..start as usize].char_indices().last().map_or(0, |(at, _)| at as u32)
                }
                else
                {
                    start
                };
                self.replace(previous, end, "")
            }
            '\u{1c}' =>
            {
                let at = self.text[..start as usize].char_indices()
                                                    .last()
                                                    .map_or(0, |(at, _)| at as u32);
                self.select(at, at)
            }
            '\u{1d}' =>
            {
                let at =
                    end
                    + self.text[end as usize..].chars().next().map_or(0, |c| c.len_utf8() as u32);
                self.select(at, at)
            }
            '\u{1e}' | '\u{1f}' =>
            {
                let mut p = self.point(start)?;
                p.v = p.v.saturating_sub(1);
                p.v = p.v.saturating_add(if c == '\u{1e}'
                                         {
                                             -self.info.line_height
                                         }
                                         else
                                         {
                                             self.info.line_height
                                         });
                let at = self.offset(p);
                self.select(at, at)
            }
            _ => self.replace(start, end, c.encode_utf8(&mut [0; 4]))
        }
    }

    pub fn scroll(&mut self, delta: Point) -> Result<(), Status>
    {
        let d = self.info.dest;
        let left = d.left.checked_add(delta.h).ok_or(BAD_ARGUMENT)?;
        let right = d.right.checked_add(delta.h).ok_or(BAD_ARGUMENT)?;
        let top = d.top.checked_add(delta.v).ok_or(BAD_ARGUMENT)?;
        let bottom = d.bottom.checked_add(delta.v).ok_or(BAD_ARGUMENT)?;
        self.info.dest = Rect::new(left, top, right, bottom);
        self.changed();
        Ok(())
    }

    pub fn activate(&mut self, active: bool)
    {
        self.info.active = u16::from(active);
        self.changed();
    }

    pub fn idle(&mut self, ticks: u32)
    {
        if self.info.active == 0 || self.info.selection_start != self.info.selection_end
        {
            self.info.caret_visible = 0;
            return;
        }
        match self.blink
        {
            None =>
            {
                self.blink = Some(ticks);
                self.info.caret_visible = 1;
                self.info.revision = self.info.revision.wrapping_add(1);
            }
            Some(previous) if ticks.wrapping_sub(previous) >= 30 =>
            {
                self.info.caret_visible ^= 1;
                self.blink = Some(ticks);
                self.info.revision = self.info.revision.wrapping_add(1);
            }
            _ => ()
        }
    }

    pub fn click(&mut self, p: Point, extend: bool) -> Result<(), Status>
    {
        let at = self.offset(p);
        let anchor = if extend { self.anchor } else { at };
        self.select(anchor.min(at), anchor.max(at))?;
        self.anchor = anchor;
        Ok(())
    }

    pub fn caret_rect(&self) -> Rect
    {
        let p = self.point(self.info.selection_end).unwrap_or_default();
        Rect::new(p.h.saturating_sub(1), p.v.saturating_sub(self.info.line_height), p.h, p.v)
    }

    pub fn selection_rects(&self, mut emit: impl FnMut(Rect))
    {
        let start = self.info.selection_start;
        let end = self.info.selection_end;
        if start == end
        {
            return;
        }
        for (row, line) in self.lines.iter().enumerate()
        {
            if start >= line.end || end <= line.start
            {
                continue;
            }
            let a = start.max(line.start);
            let b = end.min(line.end);
            let left = i32::from(self.info.dest.left)
                       + 1
                       + i32::from(layout::width(&self.text[line.start as usize..a as usize]));
            let includes_cr =
                self.text.as_bytes().get(b.saturating_sub(1) as usize) == Some(&b'\r');
            let right = if includes_cr || (end >= line.end && line.end < self.text.len() as u32)
            {
                i32::from(self.info.view.right)
            }
            else
            {
                i32::from(self.info.dest.left)
                + 1
                + i32::from(layout::width(&self.text[line.start as usize..b as usize]))
            };
            let top = i32::from(self.info.dest.top) + row as i32 * i32::from(self.info.line_height);
            emit(Rect::new(clamp(left),
                           clamp(top),
                           clamp(right),
                           clamp(top + i32::from(self.info.line_height))));
        }
    }
}
fn clamp(value: i32) -> i16
{
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

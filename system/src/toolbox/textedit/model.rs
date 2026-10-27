use alloc::{string::String, sync::Arc, vec::Vec};

use mac_abi::{textedit::*, *};

use super::layout;

pub struct Editor
{
    font:          layout::Font,
    text:          String,
    lines:         Vec<layout::Line>,
    info:          EditorInfo,
    anchor:        u32,
    blink:         Option<u32>,
    preferred_x:   Option<i16>,
    affinity:      Option<usize>,
    options:       DisplayOptions,
    scroll_x:      u32,
    logical_lines: u32,
    widest:        u32,
    widest_count:  usize,
    snapshot:      Option<(u64, core::ops::Range<usize>, Arc<String>)>
}
pub(crate) struct IdlePlan<'a>
{
    editor: &'a mut Editor,
    info:   EditorInfo,
    blink:  Option<u32>
}
impl IdlePlan<'_>
{
    pub(crate) fn info(&self) -> EditorInfo
    {
        self.info
    }

    pub(crate) fn commit(self)
    {
        self.editor.info = self.info;
        self.editor.blink = self.blink;
    }
}
impl Editor
{
    pub fn new(dest: Rect, view: Rect) -> Result<Self, Status>
    {
        if dest.is_empty() || view.is_empty()
        {
            return Err(BAD_ARGUMENT);
        }
        let font = layout::Font::default();
        let lines = layout::lines("", i32::from(dest.right) - i32::from(dest.left), &font)?;
        Ok(Self { font,
                  text: String::new(),
                  lines,
                  info: EditorInfo { dest,
                                     view,
                                     line_height: 16,
                                     ascent: 12,
                                     line_count: 0,
                                     ..Default::default() },
                  anchor: 0,
                  blink: None,
                  preferred_x: None,
                  affinity: None,
                  options: DisplayOptions::default(),
                  scroll_x: 0,
                  logical_lines: 1,
                  widest: 0,
                  widest_count: 0,
                  snapshot: None })
    }

    pub fn font(&self) -> quickdraw::FontSelection
    {
        self.font.selection()
    }

    pub fn prepare_font_input(&mut self, input: &str,
                              fonts: &crate::toolbox::quickdraw::fonts::FontManager)
                              -> Result<(), Status>
    {
        self.font.prepare(input, fonts)
    }

    pub fn set_font(&mut self, selection: quickdraw::FontSelection,
                    fonts: &crate::toolbox::quickdraw::fonts::FontManager)
                    -> Result<(), Status>
    {
        let font = layout::Font::new(selection, &self.text, fonts)?;
        let gutter = Self::gutter_with(&font, self.options, self.logical_lines);
        let width = if self.options.wrap == 0
        {
            i32::MAX
        }
        else
        {
            (i32::from(self.info.dest.right) - i32::from(self.info.dest.left) - gutter as i32).max(1)
        };
        let lines = layout::lines(&self.text, width, &font)?;
        let old_height = self.info.line_height;
        let anchor = self.top_anchor();
        self.info.ascent = font.ascent();
        self.info.line_height = font.line_height();
        self.font = font;
        self.install_lines(lines, anchor, old_height);
        self.changed();
        Ok(())
    }

    pub fn text(&self) -> &str
    {
        &self.text
    }

    pub(in crate::toolbox) fn lines(&self) -> &[layout::Line]
    {
        &self.lines
    }

    /// One immutable visible-block allocation per content/range change. Line
    /// commands only clone the reference, never allocate their own strings.
    pub fn snapshot(&mut self) -> Result<(usize, Arc<String>), Status>
    {
        let visible = self.visible_lines();
        let start = self.lines.get(visible.start).map_or(0, |line| line.start as usize);
        let end = visible.end.checked_sub(1).and_then(|at| self.lines.get(at)).map_or(start,
                                                                                      |line| {
                                                                                          line.end
                                                                                          as usize
                                                                                      });
        if let Some((revision, range, source)) = &self.snapshot
           && *revision == self.info.content_revision
           && range.start <= start
           && range.end >= end
        {
            return Ok((range.start, source.clone()));
        }
        let mut text = String::new();
        text.try_reserve_exact(end - start).map_err(|_| NO_MEMORY)?;
        text.push_str(&self.text[start..end]);
        #[cfg(all(target_arch = "riscv64", feature = "event-trace"))]
        crate::trace::record(crate::trace::TEXTEDIT, 0, 1, text.len() as u64, 1, 0);
        let source = Arc::new(text);
        self.snapshot = Some((self.info.content_revision, start..end, source.clone()));
        Ok((start, source))
    }

    pub fn info(&self) -> EditorInfo
    {
        EditorInfo { scroll_max: self.maximum_scroll(),
                     ..self.info }
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
        self.preferred_x = None;
        self.affinity = None;
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
        if &self.text[start..end] == input
        {
            let end = (start + input.len()) as u32;
            return self.select(end, end);
        }
        let content_revision = self.info.content_revision.checked_add(1).ok_or(NO_MEMORY)?;
        let length = self.text.len() - (end - start) + input.len();
        if length > MAX_BYTES
        {
            return Err(NO_MEMORY);
        }
        let removed_crs = self.text[start..end].bytes().filter(|&b| b == b'\r').count() as i64;
        let inserted_crs = input.bytes().filter(|&b| b == b'\r').count() as i64;
        let logical_lines = (i64::from(self.logical_lines) + inserted_crs - removed_crs) as u32;
        let gutter_changed = self.gutter_for(logical_lines) != self.gutter_width();
        let width = self.layout_width_for(self.info.dest, logical_lines);
        let (first, resume, replacement) = if gutter_changed
        {
            let mut proposed = String::new();
            proposed.try_reserve_exact(length).map_err(|_| NO_MEMORY)?;
            proposed.push_str(&self.text[..start]);
            proposed.push_str(input);
            proposed.push_str(&self.text[end..]);
            (0, self.lines.len(), layout::lines(&proposed, width, &self.font)?)
        }
        else
        {
            layout::replacement(&self.text, &self.lines, start, end, input, width, &self.font)?
        };
        #[cfg(all(target_arch = "riscv64", feature = "event-trace"))]
        crate::trace::record(crate::trace::TEXTEDIT,
                             0,
                             0,
                             replacement.first()
                                        .zip(replacement.last())
                                        .map_or(0, |(a, b)| u64::from(b.end - a.start)),
                             replacement.len() as u64,
                             length as u64);
        let removed_widest =
            self.lines[first..resume].iter().filter(|line| line.extent == self.widest).count();
        let added_maximum = replacement.iter().map(|line| line.extent).max().unwrap_or(0);
        let added_widest = replacement.iter().filter(|line| line.extent == self.widest).count();
        let added_maximum_count =
            replacement.iter().filter(|line| line.extent == added_maximum).count();
        let old_count = self.lines.len();
        let count = first + replacement.len() + old_count - resume;
        if length > self.text.capacity()
        {
            // Keep amortized in-place editing without reserving beyond the
            // document's fixed byte ceiling when a near-cap buffer grows.
            let capacity = self.text.capacity().saturating_mul(2).max(length).max(8).min(MAX_BYTES);
            self.text.try_reserve_exact(capacity - self.text.len()).map_err(|_| NO_MEMORY)?;
        }
        self.lines.try_reserve(count.saturating_sub(old_count)).map_err(|_| NO_MEMORY)?;
        // All fallible work is complete. Neither splice nor table moves allocate.
        self.text.replace_range(start..end, input);
        if count > old_count
        {
            self.lines.resize(count, layout::Line::default());
        }
        self.lines.copy_within(resume..old_count, first + replacement.len());
        self.lines.truncate(count);
        self.lines[first..first + replacement.len()].copy_from_slice(&replacement);
        let delta = input.len() as i64 - (end - start) as i64;
        for line in &mut self.lines[first + replacement.len()..]
        {
            line.start = (i64::from(line.start) + delta) as u32;
            line.end = (i64::from(line.end) + delta) as u32;
            line.logical = (i64::from(line.logical) + inserted_crs - removed_crs) as u32;
        }
        self.logical_lines = logical_lines;
        if added_maximum > self.widest
        {
            self.widest = added_maximum;
            self.widest_count = added_maximum_count;
        }
        else
        {
            self.widest_count = self.widest_count - removed_widest + added_widest;
            // Only shortening the last widest line requires finding another
            // maximum; ordinary edits never rescan the untouched width table.
            if self.widest_count == 0
            {
                self.widest = self.lines.iter().map(|line| line.extent).max().unwrap_or(0);
                self.widest_count =
                    self.lines.iter().filter(|line| line.extent == self.widest).count();
            }
        }
        self.scroll_x = self.scroll_x.min(self.maximum_scroll_x());
        self.info.content_revision = content_revision;
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
        self.prepared_replace_with_fonts(input, None)
    }

    pub fn prepared_replace_with_fonts(&self, input: &str,
                                       fonts: Option<&crate::toolbox::quickdraw::fonts::FontManager>)
                                       -> Result<Self, Status>
    {
        let mut next = Self { font:          self.font.try_clone()?,
                              text:          String::new(),
                              lines:         Vec::new(),
                              info:          self.info,
                              anchor:        self.anchor,
                              blink:         self.blink,
                              preferred_x:   self.preferred_x,
                              affinity:      self.affinity,
                              options:       self.options,
                              scroll_x:      self.scroll_x,
                              logical_lines: self.logical_lines,
                              widest:        self.widest,
                              widest_count:  self.widest_count,
                              snapshot:      None };
        next.text.try_reserve_exact(self.text.len()).map_err(|_| NO_MEMORY)?;
        next.text.push_str(&self.text);
        next.lines.try_reserve_exact(self.lines.len()).map_err(|_| NO_MEMORY)?;
        next.lines.extend_from_slice(&self.lines);
        if let Some(fonts) = fonts
        {
            next.prepare_font_input(input, fonts)?;
        }
        next.replace(self.info.selection_start, self.info.selection_end, input)?;
        Ok(next)
    }

    pub fn set_rects(&mut self, dest: Rect, view: Rect) -> Result<(), Status>
    {
        if dest.is_empty() || view.is_empty()
        {
            return Err(BAD_ARGUMENT);
        }
        let lines =
            layout::lines(&self.text, self.layout_width_for(dest, self.logical_lines), &self.font)?;
        let anchor = self.top_anchor();
        self.info.dest = dest;
        self.info.view = view;
        self.install_lines(lines, anchor, self.info.line_height);
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
            return Ok(Point { h: clamp(self.origin_x() + 1),
                              v: clamp(self.origin_y() + i32::from(self.info.line_height)) });
        }
        let row = self.lines
                      .iter()
                      .position(|line| index < line.end || line.start == line.end)
                      .unwrap_or(self.lines.len() - 1);
        let line = self.lines[row];
        let text = &self.text[line.start as usize..at];
        let text = if text.ends_with('\r') { "" } else { text };
        Ok(Point { h: clamp(self.origin_x() + 1 + self.font.width_wide(text) as i32),
                   v: clamp(self.origin_y() + (row as i32 + 1) * i32::from(self.info.line_height)) })
    }

    pub fn offset(&self, point: Point) -> u32
    {
        let relative = i32::from(point.v) - self.origin_y();
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
        let mut x = self.origin_x() + 1;
        for (relative, c) in self.text[line.start as usize..line.end as usize].char_indices()
        {
            let at = line.start + relative as u32;
            if c == '\r'
            {
                return at;
            }
            let width = i32::from(self.font.width(c.encode_utf8(&mut [0; 4])));
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

    fn native_row(&self, at: u32) -> usize
    {
        if at as usize == self.text.len() && self.text.ends_with('\r')
        {
            return self.lines.len();
        }
        self.lines.partition_point(|line| line.end <= at).min(self.lines.len().saturating_sub(1))
    }

    fn row_bounds(&self, row: usize) -> (u32, u32)
    {
        self.lines.get(row).map_or((self.info.text_length, self.info.text_length), |line| {
                               let end = line.end
                                         - u32::from(self.text
                                                         .as_bytes()
                                                         .get(line.end.saturating_sub(1) as usize)
                                                     == Some(&b'\r'));
                               (line.start, end)
                           })
    }

    fn native_point(&self, at: u32, row: usize) -> Point
    {
        let (start, end) = self.row_bounds(row);
        Point { h: clamp(self.origin_x()
                         + 1
                         + self.font.width_wide(&self.text[start as usize..at.clamp(start, end) as usize]) as i32),
                v: clamp(self.origin_y() + (row as i32 + 1) * i32::from(self.info.line_height)) }
    }

    fn native_offset(&self, row: usize, horizontal: i16) -> u32
    {
        let (start, end) = self.row_bounds(row);
        let mut x = self.origin_x() + 1;
        for (relative, c) in self.text[start as usize..end as usize].char_indices()
        {
            let width = i32::from(self.font.width(c.encode_utf8(&mut [0; 4])));
            if i32::from(horizontal) <= x + width / 2
            {
                return start + relative as u32;
            }
            x += width;
        }
        end
    }

    /// Native navigation keeps a fixed anchor, soft-wrap affinity and desired
    /// horizontal position across vertical movement. Classic TEKey is separate.
    pub fn navigate(&mut self, movement: CursorMovement, extend: bool) -> Result<(), Status>
    {
        use CursorMovement::*;
        let start = self.info.selection_start;
        let end = self.info.selection_end;
        let focus = if self.anchor == end { start } else { end };
        let row = self.affinity.unwrap_or_else(|| self.native_row(focus));
        let mut goal = None;
        let mut affinity = None;
        let target = match movement
        {
            Left if !extend && start != end => start,
            Right if !extend && start != end => end,
            Left =>
            {
                self.text[..focus as usize].char_indices().last().map_or(0, |(at, _)| at as u32)
            }
            Right =>
            {
                focus
                + self.text[focus as usize..].chars().next().map_or(0, |c| c.len_utf8() as u32)
            }
            DocumentStart => 0,
            DocumentEnd => self.info.text_length,
            LineStart =>
            {
                affinity = Some(row);
                self.row_bounds(row).0
            }
            LineEnd =>
            {
                affinity = Some(row);
                self.row_bounds(row).1
            }
            Up | Down =>
            {
                let x = self.preferred_x.unwrap_or_else(|| self.native_point(focus, row).h);
                goal = Some(x);
                let last = self.lines.len().saturating_sub(usize::from(!self.text.ends_with('\r')));
                let target_row =
                    if movement == Up { row.saturating_sub(1) } else { (row + 1).min(last) };
                affinity = Some(target_row);
                // Hit the line directly; document row numbers can exceed the
                // signed16-bit QuickDraw coordinate range before scrolling.
                self.native_offset(target_row, x)
            }
        };
        let anchor = if extend { self.anchor } else { target };
        self.select(anchor.min(target), anchor.max(target))?;
        self.anchor = anchor;
        self.preferred_x = goal;
        self.affinity = Some(affinity.unwrap_or_else(|| self.native_row(target)));
        Ok(())
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
        self.prepare_idle(ticks).commit();
    }

    pub(crate) fn prepare_idle(&mut self, ticks: u32) -> IdlePlan<'_>
    {
        let mut info = self.info;
        let mut blink = self.blink;
        if info.active == 0 || info.selection_start != info.selection_end
        {
            info.caret_visible = 0;
            return IdlePlan { editor: self,
                              info,
                              blink };
        }
        match blink
        {
            None =>
            {
                blink = Some(ticks);
                info.caret_visible = 1;
                info.revision = info.revision.wrapping_add(1);
            }
            Some(previous) if ticks.wrapping_sub(previous) >= 30 =>
            {
                info.caret_visible ^= 1;
                blink = Some(ticks);
                info.revision = info.revision.wrapping_add(1);
            }
            _ => ()
        }
        IdlePlan { editor: self,
                   info,
                   blink }
    }

    pub fn click(&mut self, p: Point, extend: bool) -> Result<(), Status>
    {
        let at = self.offset(p);
        let anchor = if extend { self.anchor } else { at };
        self.select(anchor.min(at), anchor.max(at))?;
        self.anchor = anchor;
        Ok(())
    }

    pub fn display_options(&self) -> DisplayOptions
    {
        self.options
    }

    fn column_width(&self) -> u32
    {
        self.font.width(" ").max(1) as u32
    }

    fn gutter_with(font: &layout::Font, options: DisplayOptions, lines: u32) -> u32
    {
        if options.line_numbers == 0
        {
            return 0;
        }
        (lines.max(1).ilog10() + 1) * font.width("0").max(1) as u32 + 9
    }

    fn gutter_for(&self, lines: u32) -> u32
    {
        Self::gutter_with(&self.font, self.options, lines)
    }

    pub fn number_width(&self, text: &str) -> u32
    {
        self.font.width_wide(text)
    }

    pub fn gutter_width(&self) -> u32
    {
        self.gutter_for(self.logical_lines)
    }

    pub fn text_view(&self) -> Rect
    {
        let mut view = self.info.view;
        view.left = clamp(i32::from(view.left) + self.gutter_width() as i32).min(view.right);
        view
    }

    fn layout_width_for(&self, dest: Rect, lines: u32) -> i32
    {
        if self.options.wrap == 0
        {
            i32::MAX
        }
        else
        {
            (i32::from(dest.right) - i32::from(dest.left) - self.gutter_for(lines) as i32).max(1)
        }
    }

    fn layout_width(&self) -> i32
    {
        self.layout_width_for(self.info.dest, self.logical_lines)
    }

    pub fn origin_x(&self) -> i32
    {
        i32::from(self.info.dest.left) + self.gutter_width() as i32 - self.scroll_x as i32
    }

    pub fn maximum_scroll_x(&self) -> u32
    {
        if self.options.wrap != 0
        {
            return 0;
        }
        self.widest.saturating_add(1).saturating_sub((i32::from(self.text_view().right) - i32::from(self.text_view().left)).max(1) as u32)
    }

    pub fn display_geometry(&self) -> DisplayGeometry
    {
        let offset = match self.options.ruler_kind
        {
            1 => self.options.ruler_value.saturating_mul(self.column_width() as i32),
            2 => self.options.ruler_value,
            _ => 0
        };
        DisplayGeometry { text_view:     self.text_view(),
                          scroll_x:      self.scroll_x,
                          scroll_max_x:  self.maximum_scroll_x(),
                          logical_lines: self.logical_lines,
                          gutter_width:  self.gutter_width(),
                          ruler_x:       self.origin_x().saturating_add(offset),
                          column_width:  self.column_width(),
                          monospaced:    u32::from(self.font.monospaced),
                          features:      DISPLAY_FEATURES }
    }

    fn top_anchor(&self) -> u32
    {
        let top = (self.info.scroll_y / self.info.line_height.max(1) as u32) as usize;
        self.lines.get(top).map_or(self.info.text_length, |line| line.start)
    }

    fn install_lines(&mut self, lines: Vec<layout::Line>, anchor: u32, old_height: i16)
    {
        let fraction = self.info.scroll_y % old_height.max(1) as u32;
        let row = lines.partition_point(|line| line.end <= anchor).min(lines.len());
        self.widest = lines.iter().map(|line| line.extent).max().unwrap_or(0);
        self.widest_count = lines.iter().filter(|line| line.extent == self.widest).count();
        self.lines = lines;
        self.info.line_count = self.lines.len() as u32;
        self.info.scroll_y = (row as u32 * self.info.line_height.max(1) as u32 + fraction).min(self.maximum_scroll());
        self.scroll_x = self.scroll_x.min(self.maximum_scroll_x());
    }

    fn reflow(&mut self) -> Result<(), Status>
    {
        let anchor = self.top_anchor();
        let lines = layout::lines(&self.text, self.layout_width(), &self.font)?;
        self.install_lines(lines, anchor, self.info.line_height);
        Ok(())
    }

    /// Slice before handing text to the font renderer, so a far horizontal
    /// origin never overflows QuickDraw's signed coordinate or rasterizes hidden glyphs.
    pub(in crate::toolbox) fn visible_text(&self, line: &layout::Line)
                                           -> (core::ops::Range<usize>, i16)
    {
        let start = line.start as usize;
        let mut end = line.end as usize;
        if self.text.as_bytes().get(end.saturating_sub(1)) == Some(&b'\r')
        {
            end -= 1;
        }
        let mut left = start;
        let mut right = end;
        let mut x = self.origin_x() + 1;
        let view = self.text_view();
        if x >= i32::from(view.left) && x + line.extent as i32 <= i32::from(view.right)
        {
            return (start..end, clamp(x));
        }
        let mut baseline = x;
        for (at, ch) in self.text[start..end].char_indices()
        {
            let width = i32::from(self.font.width(ch.encode_utf8(&mut [0; 4])));
            if x + width < i32::from(view.left)
            {
                left = start + at + ch.len_utf8();
                baseline = x + width;
            }
            if x >= i32::from(view.right)
            {
                right = start + at;
                break;
            }
            x += width;
        }
        (left..right.max(left), clamp(baseline))
    }

    pub fn set_display(&mut self, options: DisplayOptions) -> Result<(), Status>
    {
        if options.version != DISPLAY_VERSION
           || options.wrap > 1
           || options.line_numbers > 1
           || options.ruler_kind > 2
        {
            return Err(NOT_SUPPORTED);
        }
        if match options.ruler_kind
        {
            0 => options.ruler_value != 0,
            1 => options.ruler_value <= 0 || options.ruler_value > i32::from(u16::MAX),
            2 => options.ruler_value < 0 || options.ruler_value > i32::from(i16::MAX),
            _ => true
        }
        {
            return Err(BAD_ARGUMENT);
        }
        if options == self.options
        {
            return Ok(());
        }
        let old = self.options;
        self.options = options;
        if (old.wrap != options.wrap || old.line_numbers != options.line_numbers)
           && let Err(error) = self.reflow()
        {
            self.options = old;
            return Err(error);
        }
        self.changed();
        Ok(())
    }

    pub fn scroll_axes(&mut self, x: u32, y: u32)
    {
        let x = x.min(self.maximum_scroll_x());
        if self.scroll_x != x
        {
            self.scroll_x = x;
            self.info.revision = self.info.revision.wrapping_add(1);
        }
        self.scroll_to(y);
    }

    pub(crate) fn origin_y(&self) -> i32
    {
        i32::from(self.info.dest.top) - self.info.scroll_y as i32
    }

    pub(crate) fn visible_lines(&self) -> core::ops::Range<usize>
    {
        let height = i32::from(self.info.line_height).max(1);
        let start =
            (i32::from(self.info.view.top) - self.origin_y()).div_euclid(height).max(0) as usize;
        let end =
            (i32::from(self.info.view.bottom) - self.origin_y() + height - 1).div_euclid(height)
                                                                             .max(0)
            as usize;
        start.min(self.lines.len())..end.min(self.lines.len())
    }

    fn maximum_scroll(&self) -> u32
    {
        let rows =
            self.lines.len() + usize::from(self.text.ends_with('\r') || self.lines.is_empty());
        let inset = i32::from(self.info.dest.top) - i32::from(self.info.view.top);
        let content =
            (rows as i64 * i64::from(self.info.line_height) + i64::from(inset)).max(0) as u32;
        let viewport =
            (i32::from(self.info.view.bottom) - i32::from(self.info.view.top)).max(1) as u32;
        content.saturating_sub(viewport)
    }

    pub fn scroll_to(&mut self, pixels: u32)
    {
        let next = pixels.min(self.maximum_scroll());
        if next != self.info.scroll_y
        {
            self.info.scroll_y = next;
            self.info.revision = self.info.revision.wrapping_add(1);
        }
    }

    pub fn reveal_selection(&mut self)
    {
        let focus = if self.anchor == self.info.selection_end
        {
            self.info.selection_start
        }
        else
        {
            self.info.selection_end
        };
        let row = self.affinity.unwrap_or_else(|| self.native_row(focus));
        let top = self.origin_y() + row as i32 * i32::from(self.info.line_height);
        let bottom = top + i32::from(self.info.line_height);
        let delta = if top < i32::from(self.info.view.top)
        {
            top - i32::from(self.info.view.top)
        }
        else if bottom > i32::from(self.info.view.bottom)
        {
            bottom - i32::from(self.info.view.bottom)
        }
        else
        {
            0
        };
        let (start, end) = self.row_bounds(row);
        let text_x =
            self.font.width_wide(&self.text[start as usize..focus.clamp(start, end) as usize]);
        let width =
            (i32::from(self.text_view().right) - i32::from(self.text_view().left)).max(1) as u32;
        let next_x = if text_x < self.scroll_x
        {
            text_x
        }
        else if text_x.saturating_add(1) >= self.scroll_x + width
        {
            text_x.saturating_add(2).saturating_sub(width)
        }
        else
        {
            self.scroll_x
        };
        self.scroll_axes(next_x,
                         (i64::from(self.info.scroll_y) + i64::from(delta)).clamp(0,
                                                                                  u32::MAX as i64)
                         as u32);
    }

    pub fn caret_rect(&self) -> Rect
    {
        let focus = if self.anchor == self.info.selection_end
        {
            self.info.selection_start
        }
        else
        {
            self.info.selection_end
        };
        // A final CR adds an empty insertion line even though the classic
        // TEGetPoint/line-count profile excludes that line. Draw the caret at
        // the visual insertion point, preserving explicit soft-wrap affinity.
        let row = self.affinity.unwrap_or_else(|| self.native_row(focus));
        let p = self.native_point(focus, row);
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
        let visible = self.visible_lines();
        for (row, line) in
            self.lines[visible.clone()].iter()
                                       .enumerate()
                                       .map(|(row, line)| (row + visible.start, line))
        {
            if start >= line.end || end <= line.start
            {
                continue;
            }
            let a = start.max(line.start);
            let b = end.min(line.end);
            let left = self.origin_x()
                       + 1
                       + self.font.width_wide(&self.text[line.start as usize..a as usize]) as i32;
            let includes_cr =
                self.text.as_bytes().get(b.saturating_sub(1) as usize) == Some(&b'\r');
            let right = if includes_cr || (end >= line.end && line.end < self.text.len() as u32)
            {
                i32::from(self.info.view.right)
            }
            else
            {
                self.origin_x()
                + 1
                + self.font.width_wide(&self.text[line.start as usize..b as usize]) as i32
            };
            let top = self.origin_y() + row as i32 * i32::from(self.info.line_height);
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

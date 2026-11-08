//! Retained terminal cells and the QuickDraw viewport.
use std::collections::VecDeque;

use mac_toolbox::{Error, Point, Rect,
                  quickdraw::{bitmaps::Bitmap,
                              fonts::FontSelection,
                              graphics::{GraphicsPort, TransferMode}}};

pub(super) const ROW_HEIGHT: i16 = 16;
// The scrollbar occupies 15 content pixels plus the window's right border.
pub(super) const BAR_WIDTH: i16 = 15;
pub(super) const FONT: FontSelection = FontSelection { family: 4,
                                                       size:   12,
                                                       face:   0 };
const HISTORY: usize = 2000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Style
{
    pub fg:      u32,
    pub bg:      u32,
    pub reverse: bool
}
impl Style
{
    pub const ERROR: Self = Self { fg:      0xff7777,
                                   bg:      0,
                                   reverse: false };
    pub const NORMAL: Self = Self { fg:      0xc0c0c0,
                                    bg:      0,
                                    reverse: false };
}

#[derive(Clone, Copy)]
struct Cell
{
    glyph: char,
    style: Style
}
impl Cell
{
    fn blank(style: Style) -> Self
    {
        Self { glyph: ' ',
               style }
    }
}

struct Grid
{
    rows:   Vec<Vec<Cell>>,
    x:      usize,
    y:      usize,
    saved:  (usize, usize),
    wrap:   bool,
    top:    usize,
    bottom: usize,
    image:  Option<(i16, i16, crate::sixel::Raster)>
}
impl Grid
{
    fn new(cols: usize, rows: usize) -> Self
    {
        Self { rows:   vec![vec![Cell::blank(Style::NORMAL); cols]; rows],
               x:      0,
               y:      0,
               saved:  (0, 0),
               wrap:   false,
               top:    0,
               bottom: rows - 1,
               image:  None }
    }

    fn resize(&mut self, cols: usize, rows: usize)
    {
        self.rows.resize_with(rows, || Vec::new());
        for row in &mut self.rows
        {
            row.resize(cols, Cell::blank(Style::NORMAL));
        }
        self.x = self.x.min(cols - 1);
        self.y = self.y.min(rows - 1);
        self.wrap = false;
        self.top = 0;
        self.bottom = rows - 1;
    }
}

pub(super) struct Screen
{
    main:                 Grid,
    alternate:            Option<Grid>,
    history:              VecDeque<Vec<Cell>>,
    view:                 usize,
    cols:                 usize,
    rows:                 usize,
    width:                i16,
    height:               i16,
    cell:                 i16,
    dirty:                Option<(usize, usize)>,
    draft:                String,
    cursor:               bool,
    pub show_cursor:      bool,
    pub raw_input:        bool,
    pub application_keys: bool,
    pub autowrap:         bool
}
impl Screen
{
    pub fn new(cell: i16, width: i16, height: i16) -> Self
    {
        let cell = cell.max(1);
        let (cols, rows) = dimensions(cell, width, height);
        Self { main: Grid::new(cols, rows),
               alternate: None,
               history: VecDeque::new(),
               view: 0,
               cols,
               rows,
               width,
               height,
               cell,
               dirty: Some((0, rows)),
               draft: String::new(),
               cursor: false,
               show_cursor: true,
               raw_input: false,
               application_keys: false,
               autowrap: true }
    }

    pub fn resize(&mut self, width: i16, height: i16)
    {
        if (width, height) == (self.width, self.height)
        {
            return;
        }
        (self.width, self.height) = (width, height);
        (self.cols, self.rows) = dimensions(self.cell, width, height);
        self.main.resize(self.cols, self.rows);
        if let Some(grid) = &mut self.alternate
        {
            grid.resize(self.cols, self.rows);
        }
        self.all();
    }

    fn grid(&self) -> &Grid
    {
        self.alternate.as_ref().unwrap_or(&self.main)
    }

    fn grid_mut(&mut self) -> &mut Grid
    {
        self.alternate.as_mut().unwrap_or(&mut self.main)
    }

    fn damage(&mut self, start: usize, end: usize)
    {
        if self.view != 0
        {
            self.all();
            return;
        }
        self.dirty = Some(self.dirty.map_or((start, end), |(a, b)| (a.min(start), b.max(end))));
    }

    fn all(&mut self)
    {
        self.dirty = Some((0, self.rows));
    }

    pub fn size(&self) -> (usize, usize)
    {
        (self.cols, self.rows)
    }

    pub fn cell_size(&self) -> (i16, i16)
    {
        (self.cell, ROW_HEIGHT)
    }

    pub fn position(&self) -> (usize, usize)
    {
        (self.grid().x, self.grid().y)
    }

    pub fn place(&mut self, x: usize, y: usize)
    {
        let old = self.grid().y;
        let (x, y) = (x.min(self.cols - 1), y.min(self.rows - 1));
        let grid = self.grid_mut();
        (grid.x, grid.y, grid.wrap) = (x, y, false);
        self.damage(old.min(y), old.max(y) + 1);
    }

    pub fn shift(&mut self, dx: isize, dy: isize)
    {
        let (x, y) = self.position();
        self.place(x.saturating_add_signed(dx), y.saturating_add_signed(dy));
    }

    pub fn save(&mut self)
    {
        self.grid_mut().saved = self.position();
    }

    pub fn restore(&mut self)
    {
        let (x, y) = self.grid().saved;
        self.place(x, y);
    }

    pub fn alternate(&self) -> bool
    {
        self.alternate.is_some()
    }

    pub fn switch(&mut self, enabled: bool)
    {
        if enabled == self.alternate()
        {
            return;
        }
        self.alternate = enabled.then(|| Grid::new(self.cols, self.rows));
        self.view = 0;
        self.all();
    }

    pub fn image(&mut self, image: crate::sixel::Raster)
    {
        let (x, y) = self.position();
        self.grid_mut().image = Some((x as i16 * self.cell, y as i16 * ROW_HEIGHT, image));
        self.all();
    }

    pub fn scroll_region(&mut self, top: usize, bottom: usize)
    {
        if top < bottom && bottom < self.rows
        {
            let grid = self.grid_mut();
            (grid.top, grid.bottom) = (top, bottom);
            self.place(0, 0);
        }
    }

    pub fn scroll(&mut self, up: bool, count: usize, from_cursor: bool, style: Style)
    {
        let grid = self.grid();
        let top = if from_cursor { grid.y } else { grid.top };
        let bottom = grid.bottom;
        if top < grid.top || top > bottom
        {
            return;
        }
        let cols = self.cols;
        let keep = !self.alternate() && !from_cursor && top == 0 && bottom + 1 == self.rows;
        for _ in 0..count.min(bottom - top + 1)
        {
            let grid = self.grid_mut();
            let mut row = grid.rows.remove(if up { top } else { bottom });
            if keep && up
            {
                if self.history.len() == HISTORY
                {
                    self.history.pop_front();
                }
                self.history.push_back(row);
                row = vec![Cell::blank(style); cols];
                if self.view != 0
                {
                    self.view = (self.view + 1).min(self.history.len());
                }
            }
            else
            {
                row.fill(Cell::blank(style));
            }
            self.grid_mut().rows.insert(if up { bottom } else { top }, row);
        }
        if self.grid().image.is_some()
        {
            self.grid_mut().image = None;
            self.all();
        }
        self.damage(top, bottom + 1);
    }

    pub fn index(&mut self, reverse: bool, style: Style)
    {
        let grid = self.grid();
        if grid.y == if reverse { grid.top } else { grid.bottom }
        {
            self.grid_mut().wrap = false;
            self.scroll(!reverse, 1, false, style);
        }
        else
        {
            self.shift(0, if reverse { -1 } else { 1 });
        }
    }

    pub fn put(&mut self, glyph: char, style: Style)
    {
        if self.grid().wrap
        {
            self.place(0, self.grid().y);
            self.index(false, style);
        }
        let cols = self.cols;
        let wrap = self.autowrap;
        let grid = self.grid_mut();
        let y = grid.y;
        grid.rows[y][grid.x] = Cell { glyph,
                                      style };
        if grid.x + 1 < cols
        {
            grid.x += 1;
        }
        else
        {
            grid.wrap = wrap;
        }
        self.damage(y, y + 1);
    }

    pub fn erase(&mut self, display: bool, mode: u16, style: Style)
    {
        let (x, y) = self.position();
        let cols = self.cols;
        let rows = self.rows;
        if display && mode == 3
        {
            self.history.clear();
            self.view = 0;
            self.all();
            return;
        }
        if display && self.grid().image.is_some()
        {
            self.grid_mut().image = None;
            self.all();
        }
        let (first, last) = match (display, mode)
        {
            (true, 0) => (y, rows - 1),
            (true, 1) => (0, y),
            (true, 2) => (0, rows - 1),
            (false, 0..=2) => (y, y),
            _ => return
        };
        for row in first..=last
        {
            let start = if mode == 0 && row == y { x } else { 0 };
            let end = if mode == 1 && row == y { x + 1 } else { cols };
            self.grid_mut().rows[row][start..end].fill(Cell::blank(style));
        }
        self.damage(first, last + 1);
    }

    pub fn edit_chars(&mut self, action: char, count: usize, style: Style)
    {
        let (x, y) = self.position();
        let cells = &mut self.grid_mut().rows[y][x..];
        let count = count.min(cells.len());
        match action
        {
            '@' =>
            {
                cells.rotate_right(count);
                cells[..count].fill(Cell::blank(style));
            }
            'P' =>
            {
                cells.rotate_left(count);
                let end = cells.len();
                cells[end - count..].fill(Cell::blank(style));
            }
            _ => cells[..count].fill(Cell::blank(style))
        }
        self.damage(y, y + 1);
    }

    pub fn line(&mut self, stderr: bool, text: &str)
    {
        let style = if stderr { Style::ERROR } else { Style::NORMAL };
        for ch in text.chars()
        {
            self.put(ch, style);
        }
        self.place(0, self.grid().y);
        self.index(false, style);
    }

    pub fn set_cursor_visible(&mut self, show: bool)
    {
        if self.show_cursor != show
        {
            self.show_cursor = show;
            let y = self.grid().y;
            self.damage(y, y + 1);
        }
    }

    pub fn input(&mut self, draft: &str, cursor: bool)
    {
        if self.draft != draft || self.cursor != cursor
        {
            self.draft.clear();
            self.draft.push_str(draft);
            self.cursor = cursor;
            let y = self.grid().y;
            self.damage(y, y + 1);
        }
    }

    pub fn history(&self) -> (i16, i16)
    {
        let max = if self.alternate() { 0 } else { self.history.len() };
        (max.saturating_sub(self.view) as i16, max as i16)
    }

    pub fn browse(&mut self, value: i16)
    {
        let (_, max) = self.history();
        let view = max.saturating_sub(value.clamp(0, max)) as usize;
        if self.view != view
        {
            self.view = view;
            self.all();
        }
    }

    pub fn take_damage(&mut self) -> Option<Rect>
    {
        let (top, bottom) = self.dirty.take()?;
        Some(Rect::new(0,
                       top as i16 * ROW_HEIGHT,
                       self.width,
                       if bottom == self.rows { self.height } else { bottom as i16 * ROW_HEIGHT }))
    }

    pub fn paint(&self, port: GraphicsPort<'_>, damage: Rect) -> Result<(), Error>
    {
        let right = self.width.saturating_sub(BAR_WIDTH);
        let clip = Rect::new(damage.left, damage.top, damage.right.min(right), damage.bottom);
        if clip.left >= clip.right || clip.top >= clip.bottom
        {
            return Ok(());
        }
        port.reset()?;
        port.clip_rect(clip)?;
        port.colors(0, 0)?;
        port.paint_rect(clip)?;
        let grid = self.grid();
        let cursor_x = (grid.x + self.draft.chars().count()).min(self.cols - 1);
        let cursor = self.cursor && self.show_cursor && self.view == 0;
        let draft: Vec<char> = self.draft.chars().collect();
        let mut text = String::with_capacity(self.cols);
        for y in 0..self.rows
        {
            let top = y as i16 * ROW_HEIGHT;
            if top >= clip.bottom || top + ROW_HEIGHT <= clip.top
            {
                continue;
            }
            let history = if self.alternate() { 0 } else { self.history.len() };
            let line = history - self.view + y;
            let row = if line < history { &self.history[line] } else { &grid.rows[line - history] };
            let cell_at = |x: usize| {
                let mut cell = row.get(x).copied().unwrap_or(Cell::blank(Style::NORMAL));
                if self.view == 0 && y == grid.y
                {
                    if let Some(&glyph) = x.checked_sub(grid.x).and_then(|at| draft.get(at))
                    {
                        cell = Cell { glyph,
                                      style: Style::NORMAL };
                    }
                    if cell.style.reverse ^ (cursor && x == cursor_x)
                    {
                        std::mem::swap(&mut cell.style.fg, &mut cell.style.bg);
                    }
                }
                cell
            };
            let mut start = 0;
            while start < self.cols
            {
                let style = cell_at(start).style;
                text.clear();
                let mut end = start;
                while end < self.cols
                {
                    let cell = cell_at(end);
                    if cell.style != style
                    {
                        break;
                    }
                    text.push(cell.glyph);
                    end += 1;
                }
                let left = start as i16 * self.cell;
                if style.bg != 0
                {
                    port.colors(style.bg, 0)?;
                    port.paint_rect(Rect::new(left,
                                              top,
                                              end as i16 * self.cell,
                                              top + ROW_HEIGHT))?;
                }
                if !text.trim_end_matches(' ').is_empty()
                {
                    port.text(FONT,
                              Point { h: left,
                                      v: top + 12 },
                              text.trim_end_matches(' '),
                              style.fg)?;
                }
                start = end;
            }
        }
        if self.view == 0
           && let Some((x, y, image)) = &grid.image
        {
            let source = Rect::new(0, 0, image.width as i16, image.height as i16);
            let dest = Rect::new(*x, *y, *x + source.right, *y + source.bottom);
            if dest.left < clip.right
               && dest.right > clip.left
               && dest.top < clip.bottom
               && dest.bottom > clip.top
            {
                port.copy_bits(&Bitmap::Rgb32 { pixels:     &image.pixels,
                                                row_pixels: image.width,
                                                bounds:     source },
                               source,
                               dest,
                               TransferMode::Copy,
                               &[])?;
            }
        }
        Ok(())
    }
}

fn dimensions(cell: i16, width: i16, height: i16) -> (usize, usize)
{
    (((width - BAR_WIDTH) / cell).clamp(1, 512) as usize,
     (height / ROW_HEIGHT).clamp(1, 256) as usize)
}

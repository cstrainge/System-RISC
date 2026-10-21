//! Finder's large-icon geometry and presentation. System supplies only primitives.
use mac_toolbox::{Cnid, Error, Point, Rect, VolumeId};
use unicode_segmentation::UnicodeSegmentation;

pub const GRID_WIDTH: i16 = 64;
pub const GRID_HEIGHT: i16 = 52;
pub const INFO_HEIGHT: i16 = 20;
pub const SCROLLBAR: i16 = 15;
pub const TRASH_BOUNDS: Rect = Rect::new(576, 420, 608, 452);

pub fn volume_bounds(index: u32) -> Rect
{
    let column = (index / 7) as i16;
    let row = (index % 7) as i16;
    let x = 584 - column * 64;
    let y = 28 + row * GRID_HEIGHT;
    Rect::new(x, y, x + 32, y + 32)
}

/// Maximum ink width of a filename caption, excluding its 2px selection inset.
pub const CAPTION_WIDTH: i16 = 96;
pub const LINE_HEIGHT: i16 = 12;

/// Byte-indexed line ends keep every legal 255-byte filename representable,
/// including unusual whitespace. Zero means no line end; otherwise width + 1.
/// This is all-zero BSS storage, with no allocation or traps during drawing.
#[derive(Clone, Copy)]
pub struct Caption
{
    ends:  [u8; 256],
    count: u8,
    width: u8
}
impl Caption
{
    pub const EMPTY: Self = Self { ends:  [0; 256],
                                   count: 0,
                                   width: 0 };

    pub fn measure(name: &str, mut measure: impl FnMut(&str) -> Result<i16, Error>)
                   -> Result<Self, Error>
    {
        if name.len() > 255
        {
            return Err(Error::BadArgument);
        }
        let mut result = Self::EMPTY;
        let mut start = 0;
        loop
        {
            let rest = &name[start..];
            let width = measure(rest)?.max(0);
            if width <= CAPTION_WIDTH
            {
                result.push(name.len(), width);
                break;
            }
            let mut ends = [0u8; 255];
            let mut count = 0;
            for (at, grapheme) in rest.grapheme_indices(true)
            {
                ends[count] = (at + grapheme.len()) as u8;
                count += 1;
            }
            let mut low = 0;
            let mut high = count;
            while low < high
            {
                let mid = low + (high - low) / 2;
                if measure(&rest[..ends[mid] as usize])? <= CAPTION_WIDTH
                {
                    low = mid + 1;
                }
                else
                {
                    high = mid;
                }
            }
            let mut end = if low == 0
            {
                // A pathological multi-scalar cluster can be wider than an entire
                // caption. Fall back to scalar boundaries only for that cluster.
                let cluster = &rest[..ends[0] as usize];
                let mut fit = cluster.chars().next().unwrap().len_utf8();
                for (at, ch) in cluster.char_indices().skip(1)
                {
                    let next = at + ch.len_utf8();
                    if measure(&cluster[..next])? > CAPTION_WIDTH
                    {
                        break;
                    }
                    fit = next;
                }
                fit
            }
            else
            {
                ends[low - 1] as usize
            };
            // Keep whitespace in the caption: concatenating lines is lossless.
            // Prefer word breaks, but a long unbroken name must make progress.
            if let Some((at, grapheme)) =
                rest[..end].grapheme_indices(true)
                           .rev()
                           .find(|(_, g)| g.chars().all(char::is_whitespace))
            {
                let word_end = at + grapheme.len();
                if word_end < end && !rest[..at].trim().is_empty()
                {
                    end = word_end;
                }
            }
            let width = measure(&rest[..end])?.max(0);
            // Geneva 9 and its missing-glyph fallback fit individual scalars.
            // Do not publish out-of-bounds geometry if that contract changes.
            if width > CAPTION_WIDTH
            {
                return Err(Error::BadArgument);
            }
            result.push(start + end, width);
            start += end;
        }
        Ok(result)
    }

    fn push(&mut self, end: usize, width: i16)
    {
        self.ends[end] = width as u8 + 1;
        self.count += 1;
        self.width = self.width.max(width as u8);
    }

    pub fn width(self) -> i16
    {
        self.width as i16
    }

    pub fn height(self) -> i16
    {
        i16::from(self.count) * LINE_HEIGHT
    }

    pub fn lines<'a>(&'a self, name: &'a str) -> impl Iterator<Item = (&'a str, i16)>
    {
        let mut start = 0;
        self.ends[..=name.len()].iter().enumerate().filter_map(move |(end, &value)| {
                                                       if value == 0
                                                       {
                                                           return None;
                                                       }
                                                       let line = &name[start..end];
                                                       start = end;
                                                       Some((line, i16::from(value - 1)))
                                                   })
    }

    pub fn bounds(self, icon: Rect) -> Rect
    {
        let mut bounds = label_bounds(icon, self.width());
        bounds.bottom = icon.bottom.saturating_add(self.height());
        bounds
    }
}

/// Automatic placement reserves the complete wrapped caption, with variable
/// row height. Saved catalog positions override placement without remeasurement.
pub struct Layout
{
    columns: i16,
    column:  i16,
    top:     i16,
    height:  i16,
    right:   i16
}
impl Layout
{
    pub fn new(width: i16) -> Self
    {
        Self { columns: ((width - SCROLLBAR) / GRID_WIDTH).max(1),
               column:  0,
               top:     INFO_HEIGHT + 8,
               height:  GRID_HEIGHT,
               right:   -8 }
    }

    fn next_row(&mut self)
    {
        self.column = 0;
        self.top = self.top.saturating_add(self.height);
        self.height = GRID_HEIGHT;
        self.right = -8;
    }

    pub fn place(&mut self, caption: Caption) -> Rect
    {
        let width = caption.width();
        if self.column >= self.columns
        {
            self.next_row();
        }
        while self.column + 1 < self.columns
              && self.column * GRID_WIDTH + 32 - (width + 1) / 2 - 2 < self.right
        {
            self.column += 1;
        }
        if self.column > 0 && self.column * GRID_WIDTH + 32 - (width + 1) / 2 - 2 < self.right
        {
            self.next_row();
        }
        let center = (self.column * GRID_WIDTH + 32).max((width + 1) / 2 + 2);
        self.right = center - (width + 1) / 2 + width + 2;
        self.height = self.height.max(32 + caption.height() + 8);
        self.column += 1;
        Rect::new(center - 16, self.top, center + 16, self.top.saturating_add(32))
    }
}

#[derive(Clone, Copy, Default)]
pub struct State
{
    pub selected: bool,
    pub open:     bool,
    pub active:   bool,
    pub desktop:  bool
}

pub trait Drawing
{
    fn fill(&mut self, bounds: Rect, color: u32) -> Result<(), Error>;
    fn text_width(&mut self, text: &str) -> Result<i16, Error>;
    fn text(&mut self, baseline: Point, text: &str, color: u32) -> Result<(), Error>;
    fn icon(&mut self, id: Icon, name: &str, bounds: Rect, state: State) -> Result<(), Error>;
}

#[derive(Clone, Copy)]
pub enum Icon
{
    Resource(i16),
    Application
    {
        volume:   VolumeId,
        cnid:     Cnid,
        fallback: i16
    },
    Associated
    {
        file_type: mac_toolbox::TypeCode,
        fallback:  i16
    }
}
impl From<i16> for Icon
{
    fn from(id: i16) -> Self
    {
        Self::Resource(id)
    }
}

pub fn label_bounds(icon: Rect, width: i16) -> Rect
{
    let center = ((i32::from(icon.left) + i32::from(icon.right)) / 2) as i16;
    let origin = center.saturating_sub((width + 1) / 2);
    Rect::new(origin.saturating_sub(2),
              icon.bottom,
              origin.saturating_add(width).saturating_add(2),
              icon.bottom.saturating_add(LINE_HEIGHT))
}

pub fn draw_item(draw: &mut impl Drawing, id: impl Into<Icon>, icon: Rect, name: &str,
                 state: State)
                 -> Result<Rect, Error>
{
    let caption = Caption::measure(name, |text| draw.text_width(text))?;
    draw_item_measured(draw, id, icon, name, caption, state)
}

/// Cached folder views measure and wrap captions once per snapshot.
pub fn draw_item_measured(draw: &mut impl Drawing, id: impl Into<Icon>, icon: Rect, name: &str,
                          caption: Caption, state: State)
                          -> Result<Rect, Error>
{
    draw.icon(id.into(), name, icon, state)?;
    let label = caption.bounds(icon);
    let selected = state.selected && state.active;
    if state.desktop || state.selected
    {
        draw.fill(label, if selected { 0 } else { 0xffffff })?;
    }
    for (row, (text, width)) in caption.lines(name).enumerate()
    {
        let line = label_bounds(icon, width);
        draw.text(Point { h: line.left + 2,
                          v: icon.bottom.saturating_add(row as i16 * LINE_HEIGHT + 10) },
                  text,
                  if selected { 0xffffff } else { 0 })?;
    }
    Ok(Rect::new(icon.left.min(label.left), icon.top, icon.right.max(label.right), label.bottom))
}

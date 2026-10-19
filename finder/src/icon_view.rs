//! Finder's large-icon geometry and presentation. System supplies only primitives.
use mac_toolbox::{Point, Rect, Status};

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

/// Fresh Finder folders allocate additional grid cells for a wide single-line
/// name. Catalog positions can override this automatic placement.
pub struct Layout
{
    columns: i16,
    column:  i16,
    row:     i16,
    right:   i16
}
impl Layout
{
    pub fn new(width: i16) -> Self
    {
        Self { columns: ((width - SCROLLBAR) / GRID_WIDTH).max(1),
               column:  0,
               row:     0,
               right:   -8 }
    }

    pub fn place(&mut self, name_width: i16) -> Rect
    {
        if self.column >= self.columns
        {
            self.column = 0;
            self.row += 1;
            self.right = -8;
        }
        while self.column + 1 < self.columns
              && self.column * GRID_WIDTH + 32 - (name_width + 1) / 2 - 2 < self.right
        {
            self.column += 1;
        }
        let center = self.column * GRID_WIDTH + 32;
        let top = INFO_HEIGHT + 8 + self.row * GRID_HEIGHT;
        self.right = center - (name_width + 1) / 2 + name_width + 2;
        self.column += 1;
        Rect::new(center - 16, top, center + 16, top + 32)
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
    fn fill(&mut self, bounds: Rect, color: u32) -> Result<(), Status>;
    fn text_width(&mut self, text: &str) -> Result<i16, Status>;
    fn text(&mut self, baseline: Point, text: &str, color: u32) -> Result<(), Status>;
    fn icon(&mut self, id: i16, bounds: Rect, state: State) -> Result<(), Status>;
}

pub fn label_bounds(icon: Rect, width: i16) -> Rect
{
    let center = (icon.left + icon.right) / 2;
    let origin = center - (width + 1) / 2;
    Rect::new(origin - 2, icon.bottom, origin + width + 2, icon.bottom + 12)
}

pub fn draw_item(draw: &mut impl Drawing, id: i16, icon: Rect, name: &str, state: State)
                 -> Result<Rect, Status>
{
    draw.icon(id, icon, state)?;
    let width = draw.text_width(name)?;
    let label = label_bounds(icon, width);
    let selected = state.selected && state.active;
    // Desktop captions have the same opaque background as window captions.
    if state.desktop || state.selected
    {
        draw.fill(label, if selected { 0 } else { 0xffffff })?;
    }
    draw.text(Point { h: label.left + 2,
                      v: icon.bottom + 10 },
              name,
              if selected { 0xffffff } else { 0 })?;
    Ok(Rect::new(icon.left.min(label.left), icon.top, icon.right.max(label.right), label.bottom))
}

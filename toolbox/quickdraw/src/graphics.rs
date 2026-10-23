//! Stateful ports backed by resident QuickDraw.
pub use mac_abi::quickdraw::PortState;
use mac_abi::quickdraw::*;
use mac_traps::GraphicsCommand;

use crate::{Client, Point, Rect, Result, WindowId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i16)]
pub enum TransferMode
{
    Copy           = 0,
    Or             = 1,
    Xor            = 2,
    Bic            = 3,
    NotCopy        = 4,
    NotOr          = 5,
    NotXor         = 6,
    NotBic         = 7,
    PatternCopy    = 8,
    PatternOr      = 9,
    PatternXor     = 10,
    PatternBic     = 11,
    NotPatternCopy = 12,
    NotPatternOr   = 13,
    NotPatternXor  = 14,
    NotPatternBic  = 15
}
#[derive(Clone, Copy)]
pub enum Shape<'a>
{
    Rect(Rect),
    Oval(Rect),
    RoundRect
    {
        bounds: Rect,
        corner: Point
    },
    Arc
    {
        bounds: Rect,
        start:  i16,
        sweep:  i16
    },
    Polygon(&'a [Point]),
    Region(&'a [u8])
}
#[derive(Clone, Copy)]
pub enum Verb
{
    Frame,
    Paint,
    Erase,
    Invert,
    Fill([u8; 8])
}
fn drawing(shape: Shape<'_>, verb: Verb) -> (GraphicsCommand<'_>, Option<[u8; 8]>)
{
    let mut c = GraphicsCommand { operation: QD_SHAPE,
                                  ..Default::default() };
    match shape
    {
        Shape::Rect(r) =>
        {
            c.shape = SHAPE_RECT;
            c.bounds = r;
        }
        Shape::Oval(r) =>
        {
            c.shape = SHAPE_OVAL;
            c.bounds = r;
        }
        Shape::RoundRect { bounds,
                           corner } =>
        {
            c.shape = SHAPE_ROUND_RECT;
            c.bounds = bounds;
            c.corner = corner;
        }
        Shape::Arc { bounds,
                     start,
                     sweep } =>
        {
            c.shape = SHAPE_ARC;
            c.bounds = bounds;
            c.start_angle = start;
            c.arc_angle = sweep;
        }
        Shape::Polygon(p) =>
        {
            c.shape = SHAPE_POLYGON;
            c.points = p;
        }
        Shape::Region(r) =>
        {
            c.shape = SHAPE_REGION;
            c.region = r;
        }
    }
    let pattern = match verb
    {
        Verb::Frame =>
        {
            c.verb = VERB_FRAME;
            None
        }
        Verb::Paint =>
        {
            c.verb = VERB_PAINT;
            None
        }
        Verb::Erase =>
        {
            c.verb = VERB_ERASE;
            None
        }
        Verb::Invert =>
        {
            c.verb = VERB_INVERT;
            None
        }
        Verb::Fill(p) =>
        {
            c.verb = VERB_FILL;
            Some(p)
        }
    };
    (c, pattern)
}

#[derive(Clone, Copy)]
pub struct GraphicsPort<'a>
{
    pub(crate) client: Client<'a>,
    pub(crate) window: WindowId
}
impl<'a> GraphicsPort<'a>
{
    /// Change the System-owned transient content outline. It damages just its
    /// old/new edges and keeps retained application drawing and cached pixels.
    /// During an update the property is staged and follows commit/cancellation.
    /// The Finder-owned desktop uses screen coordinates and sits below windows.
    pub fn content_outline(self, bounds: Option<Rect>) -> Result<()>
    {
        self.request(GraphicsCommand { operation: QD_WINDOW_OUTLINE,
                                       verb: u32::from(bounds.is_some()),
                                       bounds: bounds.unwrap_or(Rect::ZERO),
                                       ..Default::default() })
            .map(|_| ())
    }

    /// Begin one masked update for the exact union of changed rectangles.
    /// The bounded request avoids one region trap per selected item.
    /// Also accepts the Finder-owned desktop port, using screen coordinates.
    pub fn masked_rectangles(self, rectangles: &[Rect]) -> Result<MaskedUpdate<'a>>
    {
        if rectangles.is_empty() || rectangles.len() > 1056
        {
            return Err(crate::Error::BadArgument);
        }
        let mut bytes = [0u8; 1056 * 8];
        for (rect, output) in rectangles.iter().zip(bytes.as_chunks_mut::<8>().0.iter_mut())
        {
            for (value, pair) in
                [rect.left, rect.top, rect.right, rect.bottom].into_iter()
                                                              .zip(output.as_chunks_mut::<2>()
                                                                         .0
                                                                         .iter_mut())
            {
                pair.copy_from_slice(&value.to_le_bytes());
            }
        }
        self.request(GraphicsCommand { operation: QD_WINDOW_BEGIN_RECTS,
                                       region: &bytes[..rectangles.len() * 8],
                                       ..Default::default() })?;
        Ok(MaskedUpdate { port:   self,
                          active: true })
    }

    /// Stage a replacement for precisely this local-coordinate region. Drawing
    /// outside it cannot change the committed scene. Drop cancels the update.
    /// Desktop masks use screen coordinates and require the desktop owner.
    pub fn masked_update(self, region: &[u8]) -> Result<MaskedUpdate<'a>>
    {
        self.request(GraphicsCommand { operation: QD_WINDOW_BEGIN_MASK,
                                       region,
                                       ..Default::default() })?;
        Ok(MaskedUpdate { port:   self,
                          active: true })
    }

    pub const fn new(client: Client<'a>, window: WindowId) -> Self
    {
        Self { client,
               window }
    }

    pub const fn window_id(self) -> WindowId
    {
        self.window
    }

    fn request(self, c: GraphicsCommand<'_>) -> Result<PortState>
    {
        let mut state = PortState::default();
        self.client.graphics(self.window.0, &mut state, c)?;
        Ok(state)
    }

    pub fn state(self) -> Result<PortState>
    {
        self.request(GraphicsCommand { operation: QD_GET_STATE,
                                       ..Default::default() })
    }

    pub fn set_state(self, mut state: PortState) -> Result<()>
    {
        self.client.graphics(self.window.0,
                             &mut state,
                             GraphicsCommand { operation: QD_SET_STATE,
                                               ..Default::default() })
    }

    pub fn reset(self) -> Result<()>
    {
        self.request(GraphicsCommand { operation: QD_RESET,
                                       ..Default::default() })
            .map(|_| ())
    }

    fn point(self, operation: u32, point: Point) -> Result<()>
    {
        self.request(GraphicsCommand { operation,
                                       point,
                                       ..Default::default() })
            .map(|_| ())
    }

    pub fn move_to(self, p: Point) -> Result<()>
    {
        self.point(QD_MOVE_TO, p)
    }

    pub fn move_by(self, delta: Point) -> Result<()>
    {
        self.point(QD_MOVE, delta)
    }

    pub fn line_to(self, p: Point) -> Result<()>
    {
        self.point(QD_LINE_TO, p)
    }

    pub fn line(self, delta: Point) -> Result<()>
    {
        self.point(QD_LINE, delta)
    }

    pub fn pen_size(self, width: i16, height: i16) -> Result<()>
    {
        self.point(QD_PEN_SIZE,
                   Point { h: width,
                           v: height })
    }

    pub fn set_origin(self, p: Point) -> Result<()>
    {
        self.point(QD_ORIGIN, p)
    }

    pub fn clip_rect(self, bounds: Rect) -> Result<()>
    {
        self.request(GraphicsCommand { operation: QD_CLIP_RECT,
                                       bounds,
                                       ..Default::default() })
            .map(|_| ())
    }

    pub fn clip_region(self, bytes: &[u8]) -> Result<()>
    {
        self.request(GraphicsCommand { operation: QD_CLIP_REGION,
                                       region: bytes,
                                       ..Default::default() })
            .map(|_| ())
    }

    fn value(self, operation: u32, mut state: PortState) -> Result<()>
    {
        self.client.graphics(self.window.0,
                             &mut state,
                             GraphicsCommand { operation,
                                               ..Default::default() })
    }

    pub fn pen_mode(self, mode: TransferMode) -> Result<()>
    {
        let s = PortState { mode: mode as i16,
                            ..Default::default() };
        self.value(QD_PEN_MODE, s)
    }

    pub fn pen_pattern(self, pattern: [u8; 8]) -> Result<()>
    {
        self.value(QD_PEN_PATTERN,
                   PortState { pattern,
                               ..Default::default() })
    }

    pub fn background_pattern(self, background_pattern: [u8; 8]) -> Result<()>
    {
        self.value(QD_BACK_PATTERN,
                   PortState { background_pattern,
                               ..Default::default() })
    }

    pub fn colors(self, foreground: u32, background: u32) -> Result<()>
    {
        self.value(QD_COLORS,
                   PortState { foreground,
                               background,
                               ..Default::default() })
    }

    pub fn hide_pen(self) -> Result<()>
    {
        let mut s = self.state()?;
        s.visibility = s.visibility.saturating_sub(1);
        self.value(QD_PEN_VISIBILITY, s)
    }

    pub fn show_pen(self) -> Result<()>
    {
        let mut s = self.state()?;
        s.visibility = s.visibility.saturating_add(1).min(0);
        self.value(QD_PEN_VISIBILITY, s)
    }

    pub fn draw(self, shape: Shape<'_>, verb: Verb) -> Result<()>
    {
        let (command, pattern) = drawing(shape, verb);
        let mut state = PortState::default();
        if let Some(p) = pattern
        {
            state.pattern = p;
        }
        self.client.graphics(self.window.0, &mut state, command)
    }

    pub fn paint_rect(self, r: Rect) -> Result<()>
    {
        self.draw(Shape::Rect(r), Verb::Paint)
    }

    pub fn frame_rect(self, r: Rect) -> Result<()>
    {
        self.draw(Shape::Rect(r), Verb::Frame)
    }

    pub fn paint_oval(self, r: Rect) -> Result<()>
    {
        self.draw(Shape::Oval(r), Verb::Paint)
    }

    pub fn frame_oval(self, r: Rect) -> Result<()>
    {
        self.draw(Shape::Oval(r), Verb::Frame)
    }

    pub fn text(self, font: FontSelection, baseline: Point, text: &str, color: u32) -> Result<i16>
    {
        self.client.font_draw(self.window.0, font, baseline, text, color)
    }
}

#[must_use = "commit publishes this update; Drop cancels it"]
pub struct MaskedUpdate<'a>
{
    port:   GraphicsPort<'a>,
    active: bool
}
impl MaskedUpdate<'_>
{
    pub fn commit(mut self) -> Result<()>
    {
        self.port.request(GraphicsCommand { operation: QD_WINDOW_COMMIT_MASK,
                                             ..Default::default() })?;
        self.active = false;
        Ok(())
    }
}

impl Drop for MaskedUpdate<'_>
{
    fn drop(&mut self)
    {
        if self.active
        {
            let _ = self.port.request(GraphicsCommand { operation: QD_WINDOW_CANCEL_MASK,
                                                        ..Default::default() });
        }
    }
}

impl<'a> crate::QuickDraw<'a>
{
    pub fn graphics(self, window: WindowId) -> GraphicsPort<'a>
    {
        GraphicsPort::new(self.client, window)
    }
}

impl crate::SurfacePort<'_>
{
    pub fn draw_shape(&mut self, state: &mut PortState, shape: Shape<'_>, verb: Verb)
                      -> Result<()>
    {
        let (c, p) = drawing(shape, verb);
        let saved = state.pattern;
        if let Some(p) = p
        {
            state.pattern = p;
        }
        let result = self.connection.graphics(state, c);
        state.pattern = saved;
        result
    }

    pub fn font_text(&mut self, font: FontSelection, baseline: Point, text: &str, color: u32)
                     -> Result<i16>
    {
        self.connection.font_draw(font, baseline, text, color)
    }

    pub fn font_width(&mut self, font: FontSelection, text: &str) -> Result<i16>
    {
        self.connection.font_width(font, text)
    }

    pub fn selected_font_metrics(&mut self, font: FontSelection) -> Result<FontMetrics>
    {
        self.connection.selected_font_metrics(font)
    }

    pub fn font_text_clipped(&mut self, font: FontSelection, baseline: Point, text: &str,
                             limit: i16, color: u32)
                             -> Result<i16>
    {
        self.connection.font_draw_clipped(font, baseline, text, limit, color)
    }
}

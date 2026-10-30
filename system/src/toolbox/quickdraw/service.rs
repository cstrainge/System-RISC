//! Safe native-port service and retained drawing snapshots.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_abi::{quickdraw as abi, *};

use super::{Canvas,
            port::{self, Shape, Verb},
            raster::TransferMode,
            region::Region};
#[derive(Clone, Default)]
pub struct Port
{
    pub state:  port::PortState,
    pub origin: Point
}
impl Port
{
    pub fn wire(&self) -> abi::PortState
    {
        abi::PortState { location:           self.state.pen_location,
                         pen_size:           self.state.pen_size,
                         mode:               self.state.pen_mode.number() as i16,
                         visibility:         self.state.pen_visibility,
                         pattern:            self.state.pen_pattern,
                         background_pattern: self.state.background_pattern,
                         foreground:         self.state.foreground,
                         background:         self.state.background,
                         clip:
                             self.state
                                 .clip
                                 .as_ref()
                                 .map_or(abi::PortState::default().clip, Region::bounds),
                         origin:             self.origin }
    }

    pub fn from_wire(w: abi::PortState) -> Result<Self, Status>
    {
        if w.pen_size.h < 0 || w.pen_size.v < 0
        {
            return Err(BAD_ARGUMENT);
        }
        Ok(Self { state:  port::PortState { pen_location:       w.location,
                                            pen_size:           w.pen_size,
                                            pen_mode:
                                                TransferMode::new(w.mode).ok_or(NOT_SUPPORTED)?,
                                            pen_pattern:        w.pattern,
                                            background_pattern: w.background_pattern,
                                            foreground:         w.foreground & 0xffffff,
                                            background:         w.background & 0xffffff,
                                            pen_visibility:     w.visibility,
                                            clip:               Some(Region::from_rect(w.clip)) },
                  origin: w.origin })
    }
}
#[derive(Clone)]
pub enum Geometry
{
    Rect(Rect),
    Oval(Rect),
    RoundRect(Rect, Point),
    Arc(Rect, i16, i16),
    Polygon(Vec<Point>),
    Region(Region)
}
impl Geometry
{
    fn shape(&self) -> Shape<'_>
    {
        match self
        {
            Self::Rect(r) => Shape::Rect(*r),
            Self::Oval(r) => Shape::Oval(*r),
            Self::RoundRect(r, p) =>
            {
                Shape::RoundRect { bounds:      *r,
                                   oval_width:  p.h,
                                   oval_height: p.v }
            }
            Self::Arc(r, start, sweep) =>
            {
                Shape::Arc { bounds: *r,
                             start:  *start,
                             sweep:  *sweep }
            }
            Self::Polygon(p) => Shape::Polygon(p),
            Self::Region(r) => Shape::Region(r)
        }
    }
}
#[derive(Clone)]
pub struct Drawing
{
    pub port:     Port,
    pub geometry: Option<Geometry>,
    pub verb:     Verb,
    pub line_end: Point
}
impl Drawing
{
    pub fn paint(&self, canvas: &mut Canvas<'_>) -> Result<(), Status>
    {
        let old = canvas.origin();
        canvas.set_origin(old.h.wrapping_add(self.port.origin.h),
                          old.v.wrapping_add(self.port.origin.v));
        let result = if let Some(g) = &self.geometry
        {
            port::draw(canvas, &self.port.state, g.shape(), self.verb)
        }
        else
        {
            let mut state = self.port.state.clone();
            port::line_to(canvas, &mut state, self.line_end)
        };
        canvas.set_origin(old.h, old.v);
        result.map_err(super::regions_service::graphics_status)
    }
}
/// Retained commands are validated without assuming a particular future clip.
pub fn graphics(port: &mut Port, call: &mut abi::GraphicsCall, points: &[Point], bytes: &[u8])
                -> Result<Option<Drawing>, Status>
{
    let drawing = decode_graphics(port, call, points, bytes)?;
    if let Some(geometry) = drawing.as_ref().and_then(|d| d.geometry.as_ref())
       && !matches!(geometry, Geometry::Rect(_))
    {
        port::shape_region(geometry.shape()).map_err(super::regions_service::graphics_status)?;
    }
    Ok(drawing)
}

/// An immediate surface has a fixed visibility clip. The real rasterizer
/// validates and allocates its clipped region before touching pixels; building
/// an additional unbounded region here would reject large scrolled controls
/// and repeat scan conversion for pixels that cannot be displayed.
pub fn paint_graphics(port: &mut Port, call: &mut abi::GraphicsCall, points: &[Point],
                      bytes: &[u8], canvas: &mut Canvas<'_>)
                      -> Result<(), Status>
{
    if let Some(drawing) = decode_graphics(port, call, points, bytes)?
    {
        drawing.paint(canvas)?;
    }
    Ok(())
}

fn decode_graphics(port: &mut Port, call: &mut abi::GraphicsCall, points: &[Point], bytes: &[u8])
                   -> Result<Option<Drawing>, Status>
{
    use abi::*;
    let result = match call.operation
    {
        QD_RESET =>
        {
            *port = Port::default();
            None
        }
        QD_GET_STATE => None,
        QD_SET_STATE =>
        {
            *port = Port::from_wire(call.state)?;
            None
        }
        QD_MOVE_TO =>
        {
            port.state.move_to(call.point);
            None
        }
        QD_MOVE =>
        {
            port.state.move_by(call.point);
            None
        }
        QD_LINE_TO | QD_LINE =>
        {
            let mut end = call.point;
            if call.operation == QD_LINE
            {
                end.h = end.h.wrapping_add(port.state.pen_location.h);
                end.v = end.v.wrapping_add(port.state.pen_location.v);
            }
            let draw = Drawing { port:     port.clone(),
                                 geometry: None,
                                 verb:     Verb::Frame,
                                 line_end: end };
            port.state.move_to(end);
            Some(draw)
        }
        QD_PEN_SIZE =>
        {
            if call.point.h < 0 || call.point.v < 0
            {
                return Err(BAD_ARGUMENT);
            }
            port.state.pen_size = call.point;
            None
        }
        QD_PEN_MODE =>
        {
            port.state.pen_mode = TransferMode::new(call.state.mode).ok_or(NOT_SUPPORTED)?;
            None
        }
        QD_PEN_PATTERN =>
        {
            port.state.pen_pattern = call.state.pattern;
            None
        }
        QD_BACK_PATTERN =>
        {
            port.state.background_pattern = call.state.background_pattern;
            None
        }
        QD_COLORS =>
        {
            port.state.foreground = call.state.foreground & 0xffffff;
            port.state.background = call.state.background & 0xffffff;
            None
        }
        QD_ORIGIN =>
        {
            port.origin = call.point;
            None
        }
        QD_PEN_VISIBILITY =>
        {
            port.state.pen_visibility = call.state.visibility;
            None
        }
        QD_CLIP_RECT =>
        {
            port.state.clip = Some(Region::from_rect(call.bounds));
            None
        }
        QD_CLIP_REGION =>
        {
            port.state.clip =
                Some(Region::from_be_bytes(bytes).map_err(super::regions_service::status)?);
            None
        }
        QD_SHAPE =>
        {
            let geometry = match call.shape
            {
                SHAPE_RECT => Geometry::Rect(call.bounds),
                SHAPE_OVAL => Geometry::Oval(call.bounds),
                SHAPE_ROUND_RECT => Geometry::RoundRect(call.bounds, call.corner),
                SHAPE_ARC => Geometry::Arc(call.bounds, call.start_angle, call.arc_angle),
                SHAPE_POLYGON =>
                {
                    Geometry::Polygon(super::storage::copy(points).map_err(|_| NO_MEMORY)?)
                }
                SHAPE_REGION =>
                {
                    Geometry::Region(Region::from_be_bytes(bytes).map_err(super::regions_service::status)?)
                }
                _ => return Err(NOT_SUPPORTED)
            };
            let verb = match call.verb
            {
                VERB_FRAME => Verb::Frame,
                VERB_PAINT => Verb::Paint,
                VERB_ERASE => Verb::Erase,
                VERB_INVERT => Verb::Invert,
                VERB_FILL => Verb::Fill(call.state.pattern),
                _ => return Err(NOT_SUPPORTED)
            };
            Some(Drawing { port: port.clone(),
                           geometry: Some(geometry),
                           verb,
                           line_end: Point::default() })
        }
        _ => return Err(NOT_SUPPORTED)
    };
    call.state = port.wire();
    Ok(result)
}

/// Apply a port's origin and potentially nonrectangular clip without changing the
/// compositor's visibility clip. Text/bitmap services reuse these same port rules.
pub fn with_clip(canvas: &mut Canvas<'_>, port: &Port, mut paint: impl FnMut(&mut Canvas<'_>))
{
    with_clip_parts(canvas, port.origin, port.state.clip.as_ref(), &mut paint);
}

pub fn with_region_clip(canvas: &mut Canvas<'_>, region: &Region,
                        mut paint: impl FnMut(&mut Canvas<'_>))
{
    with_clip_parts(canvas, Point::default(), Some(region), &mut paint);
}

fn with_clip_parts(canvas: &mut Canvas<'_>, port_origin: Point, region: Option<&Region>,
                   paint: &mut impl FnMut(&mut Canvas<'_>))
{
    let old_origin = canvas.origin();
    let old_clip = canvas.clip();
    let origin = Point { h: old_origin.h.wrapping_add(port_origin.h),
                         v: old_origin.v.wrapping_add(port_origin.v) };
    let mut set = |canvas: &mut Canvas<'_>, rect: Rect| {
        let mut clip = Rect::ZERO;
        super::geometry::sect_rect(old_clip, rect, &mut clip);
        canvas.set_origin(0, 0);
        canvas.set_clip(clip);
        canvas.set_origin(origin.h, origin.v);
        if !clip.is_empty()
        {
            paint(canvas);
        }
    };
    if let Some(region) = region
    {
        for band in region.bands()
        {
            if i32::from(band.bottom) + i32::from(origin.v) <= i32::from(old_clip.top)
            {
                continue;
            }
            if i32::from(band.top) + i32::from(origin.v) >= i32::from(old_clip.bottom)
            {
                break;
            }
            for span in &band.spans
            {
                if i32::from(span.right) + i32::from(origin.h) <= i32::from(old_clip.left)
                {
                    continue;
                }
                if i32::from(span.left) + i32::from(origin.h) >= i32::from(old_clip.right)
                {
                    break;
                }
                let r = Rect::new(span.left.saturating_add(origin.h),
                                  band.top.saturating_add(origin.v),
                                  span.right.saturating_add(origin.h),
                                  band.bottom.saturating_add(origin.v));
                set(canvas, r);
            }
        }
    }
    else
    {
        set(canvas, old_clip);
    }
    canvas.set_origin(0, 0);
    canvas.set_clip(old_clip);
    canvas.set_origin(old_origin.h, old_origin.v);
}

fn region_storage(region: &Region) -> usize
{
    region.bands()
          .iter()
          .map(|b| {
              core::mem::size_of_val(b)
              + b.spans.len() * core::mem::size_of::<super::region::Span>()
          })
          .sum()
}
impl Port
{
    pub fn storage(&self) -> usize
    {
        core::mem::size_of::<Self>() + self.state.clip.as_ref().map_or(0, region_storage)
    }
}
impl Drawing
{
    pub fn storage(&self) -> usize
    {
        self.port.storage()
        + match &self.geometry
        {
            Some(Geometry::Polygon(p)) => p.len() * core::mem::size_of::<Point>(),
            Some(Geometry::Region(r)) => region_storage(r),
            _ => 32
        }
    }
}

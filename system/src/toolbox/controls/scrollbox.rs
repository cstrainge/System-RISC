//! Resident scroll boxes. Object identities, viewport pixels, dirty rows and
//! tracking policy stay in System; CDEF1 owns the scrollbar's pixels and geometry.
#![forbid(unsafe_code)]
use alloc::{sync::Arc, vec::Vec};

use mac_abi::{controls::*, scrollbox::*, *};

use crate::toolbox::{handles::{Kind, Table},
                     quickdraw::{Canvas, fonts::FontManager, fonts_service}};
const PIXEL_BUDGET: usize = 2 * 1024 * 1024;
struct Raster
{
    pixels: Vec<u32>,
    valid:  Vec<bool>
}
struct Object
{
    owner:     u64,
    window:    u32,
    spec:      Spec,
    top:       u32,
    raster:    Arc<Raster>,
    tracking:  Option<Tracking>,
    highlight: i16
}
struct Tracking
{
    part:   i16,
    anchor: Point,
    value:  i16,
    next:   u64
}
pub struct Manager
{
    objects: Table<Object>
}
impl Default for Manager
{
    fn default() -> Self
    {
        Self { objects: Table::new(Kind::ScrollBoxes) }
    }
}
pub struct Drawing
{
    pub id: u32,
    bounds: Rect,
    raster: Arc<Raster>,
    bar:    super::Drawing
}
impl Drawing
{
    pub fn storage(&self) -> usize
    {
        self.raster.pixels.capacity() * 4 + self.raster.valid.capacity() + self.bar.storage()
    }

    pub fn paint(&self, canvas: &mut Canvas<'_>)
    {
        let o = canvas.origin();
        let c = canvas.clip();
        let b = self.bounds;
        let left = (i32::from(b.left) + i32::from(o.h)).max(i32::from(c.left)).max(0);
        let top = (i32::from(b.top) + i32::from(o.v)).max(i32::from(c.top)).max(0);
        let right =
            (i32::from(b.right) + i32::from(o.h)).min(i32::from(c.right)).min(canvas.width as i32);
        let bottom = (i32::from(b.bottom) + i32::from(o.v)).min(i32::from(c.bottom))
                                                           .min(canvas.height as i32);
        if left < right && top < bottom
        {
            let width = (b.right - b.left) as usize;
            let n = (right - left) as usize;
            for y in top..bottom
            {
                let src = (y - i32::from(o.v) - i32::from(b.top)) as usize * width
                          + (left - i32::from(o.h) - i32::from(b.left)) as usize;
                let dest = y as usize * canvas.width + left as usize;
                canvas.pixels[dest..dest + n].copy_from_slice(&self.raster.pixels[src..src + n]);
            }
        }
        self.bar.paint(canvas);
    }
}
fn dimensions(spec: Spec) -> Result<(usize, usize, usize), Status>
{
    let w = i32::from(spec.viewport.right) - i32::from(spec.viewport.left);
    let h = i32::from(spec.viewport.bottom) - i32::from(spec.viewport.top);
    if w <= 0
       || h <= 0
       || w > 2048
       || h > 2048
       || spec.row_height <= 0
       || spec.row_height as i32 > h
       || spec.padding.h < 0
       || spec.padding.v < 0
       || spec.padding.v > spec.row_height
       || spec.reserved != 0
       || spec.scrollbar.right as i32 - spec.scrollbar.left as i32 != 16
       || spec.scrollbar.bottom as i32 - (spec.scrollbar.top as i32) < 32
    {
        return Err(BAD_ARGUMENT);
    }
    Ok((w as usize, h as usize, h as usize / spec.row_height as usize))
}
fn raster(spec: Spec) -> Result<Arc<Raster>, Status>
{
    let (w, h, rows) = dimensions(spec)?;
    let mut pixels = Vec::new();
    pixels.try_reserve_exact(w * h).map_err(|_| NO_MEMORY)?;
    pixels.resize(w * h, spec.background);
    let mut valid = Vec::new();
    valid.try_reserve_exact(rows).map_err(|_| NO_MEMORY)?;
    valid.resize(rows, false);
    Ok(Arc::new(Raster { pixels,
                         valid }))
}
fn writable(object: &mut Object) -> Result<&mut Raster, Status>
{
    if Arc::get_mut(&mut object.raster).is_none()
    {
        let source = &object.raster;
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(source.pixels.len()).map_err(|_| NO_MEMORY)?;
        pixels.extend_from_slice(&source.pixels);
        let mut valid = Vec::new();
        valid.try_reserve_exact(source.valid.len()).map_err(|_| NO_MEMORY)?;
        valid.extend_from_slice(&source.valid);
        object.raster = Arc::new(Raster { pixels,
                                          valid });
    }
    Arc::get_mut(&mut object.raster).ok_or(SERVICE_BUSY)
}
impl Object
{
    fn rows(&self) -> u32
    {
        self.raster.valid.len() as u32
    }

    fn maximum(&self) -> u32
    {
        self.spec.count.saturating_sub(self.rows())
    }

    fn state(&self) -> ScrollbarState
    {
        let maximum = self.maximum().min(i16::MAX as u32) as i16;
        ScrollbarState { bounds: self.spec.scrollbar,
                         value: if maximum == 0
                         {
                             0
                         }
                         else
                         {
                             (u64::from(self.top) * maximum as u64 / u64::from(self.maximum()))
                             as i16
                         },
                         maximum,
                         active: u32::from(maximum > 0),
                         highlighted: self.highlight,
                         reserved: 0 }
    }

    fn missing(&self) -> core::ops::Range<u32>
    {
        let end = self.rows().min(self.spec.count.saturating_sub(self.top)) as usize;
        let first = self.raster.valid[..end].iter().position(|v| !*v).unwrap_or(end);
        let last = self.raster.valid[..end].iter().rposition(|v| !*v).map_or(first, |i| i + 1);
        self.top + first as u32..self.top + last as u32
    }

    fn scroll(&mut self, top: u32) -> Result<(), Status>
    {
        let top = top.min(self.maximum());
        let delta = i64::from(top) - i64::from(self.top);
        if delta == 0
        {
            return Ok(());
        }
        let spec = self.spec;
        let (w, h, rows) = dimensions(spec)?;
        let data = writable(self)?;
        let distance = delta.unsigned_abs().min(rows as u64) as usize;
        let pixel_rows = rows * spec.row_height as usize;
        let shift = distance * spec.row_height as usize * w;
        if distance == rows
        {
            data.pixels.fill(spec.background);
            data.valid.fill(false);
        }
        else if delta > 0
        {
            data.pixels.copy_within(shift..pixel_rows * w, 0);
            data.pixels[pixel_rows * w - shift..].fill(spec.background);
            data.valid.copy_within(distance..rows, 0);
            data.valid[rows - distance..].fill(false);
        }
        else
        {
            data.pixels.copy_within(0..pixel_rows * w - shift, shift);
            data.pixels[..shift].fill(spec.background);
            data.valid.copy_within(0..rows - distance, distance);
            data.valid[..distance].fill(false);
            data.pixels[pixel_rows * w..w * h].fill(spec.background);
        }
        self.top = top;
        Ok(())
    }
}
impl Manager
{
    fn get(&self, id: u32, owner: u64) -> Result<&Object, Status>
    {
        let object = self.objects.get(id).ok_or(NOT_FOUND)?;
        if object.owner != owner
        {
            return Err(ACCESS_DENIED);
        }
        Ok(object)
    }

    fn get_mut(&mut self, id: u32, owner: u64) -> Result<&mut Object, Status>
    {
        self.get(id, owner)?;
        self.objects.get_mut(id).ok_or(NOT_FOUND)
    }

    pub fn window(&self, id: u32, owner: u64) -> Result<u32, Status>
    {
        Ok(self.get(id, owner)?.window)
    }

    /// Reserve both committed and staging rasters against the compositor's
    /// shared cache budget, so viewport acceleration cannot starve file I/O.
    pub fn raster_storage(&self) -> usize
    {
        self.objects.iter().map(|(_, object)| object.raster.pixels.capacity() * 8).sum()
    }

    pub fn remove_window(&mut self, window: u32)
    {
        let ids: Vec<_> =
            self.objects.iter().filter(|(_, o)| o.window == window).map(|(id, _)| id).collect();
        for id in ids
        {
            self.objects.remove(id);
        }
    }

    pub fn remove_owner(&mut self, owner: u64)
    {
        let ids: Vec<_> =
            self.objects.iter().filter(|(_, o)| o.owner == owner).map(|(id, _)| id).collect();
        for id in ids
        {
            self.objects.remove(id);
        }
    }

    pub fn request(&mut self, owner: u64, call: &mut Call, fonts: &FontManager,
                   entry: DefinitionEntry, gate: TrapGate)
                   -> Result<Option<Drawing>, Status>
    {
        if call.reserved != [0; 3] || call.text_length as usize > call.text.len()
        {
            return Err(BAD_ARGUMENT);
        }
        let text = core::str::from_utf8(&call.text[..call.text_length as usize]).map_err(|_| {
                                                                                    BAD_ARGUMENT
                                                                                })?;
        if matches!(call.operation, CREATE | CONFIGURE)
        {
            let (w, h, _) = dimensions(call.spec)?;
            fonts.metrics(fonts_service::selection(call.spec.font))?;
            let previous = if call.operation == CONFIGURE
            {
                self.get(call.id, owner)?.raster.pixels.len() * 8
            }
            else
            {
                0
            };
            let used = self.objects.iter().map(|(_, o)| o.raster.pixels.len() * 8).sum::<usize>();
            if used - previous + w * h * 8 > PIXEL_BUDGET
            {
                return Err(NO_MEMORY);
            }
            if call.operation == CREATE
            {
                let data = raster(call.spec)?;
                call.id = self.objects
                              .insert(Object { owner,
                                               window: call.window,
                                               spec: call.spec,
                                               top: 0,
                                               raster: data,
                                               tracking: None,
                                               highlight: 0 })
                              .map_err(|_| NO_MEMORY)?;
            }
            else
            {
                let object = self.get_mut(call.id, owner)?;
                if object.spec != call.spec
                {
                    let data = raster(call.spec)?;
                    object.spec = call.spec;
                    object.raster = data;
                    object.top = object.top.min(object.maximum());
                    object.tracking = None;
                    object.highlight = 0;
                }
            }
        }
        if call.operation == DISPOSE
        {
            self.get(call.id, owner)?;
            self.objects.remove(call.id);
            return Ok(None);
        }
        let object = self.get_mut(call.id, owner)?;
        let old_top = object.top;
        let old_highlight = object.highlight;
        match call.operation
        {
            CREATE | CONFIGURE | INFO => (),
            INVALIDATE =>
            {
                let bg = object.spec.background;
                let data = writable(object)?;
                data.pixels.fill(bg);
                data.valid.fill(false);
            }
            SCROLL_TO => object.scroll(call.top)?,
            SET_ROW =>
            {
                let index = call.row.checked_sub(object.top).ok_or(BAD_ARGUMENT)? as usize;
                if index >= object.raster.valid.len()
                   || call.row >= object.spec.count
                   || call.flags & !SELECTED != 0
                {
                    return Err(BAD_ARGUMENT);
                }
                let spec = object.spec;
                let (w, h, _) = dimensions(spec)?;
                let data = writable(object)?;
                let mut canvas = Canvas::new(&mut data.pixels, w, h);
                let top = index as i16 * spec.row_height;
                canvas.set_clip(Rect::new(0, top, w as i16, top + spec.row_height));
                canvas.paint_rect(Rect::new(0, top, w as i16, top + spec.row_height),
                                  spec.background);
                let selected = call.flags & SELECTED != 0;
                if selected
                {
                    canvas.paint_rect(Rect::new(1, top, w as i16, top + spec.row_height - 1),
                                      spec.selected_background);
                }
                fonts.draw(fonts_service::selection(spec.font),
                           &mut canvas,
                           Point { h: spec.padding.h,
                                   v: top + spec.padding.v },
                           text,
                           if selected { spec.selected_foreground } else { spec.foreground })?;
                data.valid[index] = true;
            }
            TRACK_BEGIN =>
            {
                let mut c = ControlCall { operation: HIT_SCROLLBAR,
                                          scrollbar: object.state(),
                                          point: call.point,
                                          ..Default::default() };
                super::prepare(&mut c, entry, gate)?;
                let part = c.result as i16;
                call.part = part;
                if matches!(part,
                            SCROLL_UP
                            | SCROLL_DOWN
                            | SCROLL_PAGE_UP
                            | SCROLL_PAGE_DOWN
                            | SCROLL_THUMB)
                {
                    object.tracking = Some(Tracking { part,
                                                      anchor: call.point,
                                                      value: c.scrollbar.value,
                                                      next: call.ticks.saturating_add(20) });
                    object.highlight = part;
                    if part != SCROLL_THUMB
                    {
                        step(object, part)?;
                    }
                }
            }
            TRACK_POLL =>
            {
                if let Some(tracking) = object.tracking.take()
                {
                    let down = call.flags & BUTTON_DOWN != 0;
                    let mut c = ControlCall { operation: if tracking.part == SCROLL_THUMB
                                              {
                                                  TRACK_SCROLLBAR
                                              }
                                              else
                                              {
                                                  HIT_SCROLLBAR
                                              },
                                              scrollbar: object.state(),
                                              anchor: tracking.anchor,
                                              point: call.point,
                                              ..Default::default() };
                    if tracking.part == SCROLL_THUMB
                    {
                        c.scrollbar.value = tracking.value;
                    }
                    super::prepare(&mut c, entry, gate)?;
                    if tracking.part == SCROLL_THUMB
                    {
                        let max = object.maximum().min(i16::MAX as u32);
                        let top = if max == 0
                        {
                            0
                        }
                        else
                        {
                            ((c.result.max(0) as u64 * u64::from(object.maximum())
                              + u64::from(max) / 2)
                             / u64::from(max)).min(u64::from(object.maximum()))
                            as u32
                        };
                        object.scroll(top)?;
                        object.highlight = if down { SCROLL_THUMB } else { 0 };
                    }
                    else
                    {
                        let hot = c.result == i32::from(tracking.part);
                        object.highlight = if down && hot { tracking.part } else { 0 };
                        if down && hot && call.ticks >= tracking.next
                        {
                            step(object, tracking.part)?;
                        }
                    }
                    if down
                    {
                        let next = if call.ticks >= tracking.next
                        {
                            call.ticks.saturating_add(3)
                        }
                        else
                        {
                            tracking.next
                        };
                        object.tracking = Some(Tracking { next,
                                                          ..tracking });
                    }
                }
            }
            DRAW => (),
            _ => return Err(NOT_SUPPORTED)
        }
        call.window = object.window;
        call.spec = object.spec;
        call.top = object.top;
        let missing = object.missing();
        call.first = missing.start;
        call.end = missing.end;
        call.flags = (u32::from(object.tracking.is_some()) * TRACKING)
                     | (u32::from(object.top != old_top || object.highlight != old_highlight)
                        * CHANGED);
        if call.operation == DRAW
        {
            if !missing.is_empty()
            {
                return Err(SERVICE_BUSY);
            }
            let mut c = ControlCall { operation: DRAW_SCROLLBAR,
                                      scrollbar: object.state(),
                                      ..Default::default() };
            let bar = super::prepare(&mut c, entry, gate)?.ok_or(BAD_ARGUMENT)?;
            return Ok(Some(Drawing { id: call.id,
                                     bounds: object.spec.viewport,
                                     raster: object.raster.clone(),
                                     bar }));
        }
        Ok(None)
    }
}
fn step(object: &mut Object, part: i16) -> Result<(), Status>
{
    let delta = match part
    {
        SCROLL_UP => -1,
        SCROLL_DOWN => 1,
        SCROLL_PAGE_UP => -i64::from(object.rows()),
        SCROLL_PAGE_DOWN => i64::from(object.rows()),
        _ => 0
    };
    object.scroll((i64::from(object.top) + delta).clamp(0, i64::from(object.maximum())) as u32)
}

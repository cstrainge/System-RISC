//! Resource-backed resident cursors, animation, and classic visibility nesting.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_abi::{quickdraw::*, *};

use super::presentation::CursorSprite;

struct Animation
{
    frames:    Vec<(MonoCursor, Vec<u32>)>,
    index:     usize,
    interval:  u32,
    last_tick: u32
}

pub struct Cursors
{
    image:             MonoCursor,
    effective_hotspot: Point,
    colors:            Vec<u32>,
    automatic:         bool,
    visibility:        i32,
    obscured:          Option<Point>,
    deferred_hide:     Option<Point>,
    animation:         Option<Animation>,
    resource_id:       i16,
    scope:             mac_abi::resources::Scope
}
impl Default for Cursors
{
    fn default() -> Self
    {
        Self { image:             super::cursor::arrow(),
               effective_hotspot: Point { h: 1,
                                          v: 1 },
               colors:            Vec::new(),
               automatic:         true,
               visibility:        0,
               obscured:          None,
               deferred_hide:     None,
               animation:         None,
               resource_id:       0,
               scope:             resources::SYSTEM }
    }
}
impl Cursors
{
    pub fn is_automatic(&self) -> bool
    {
        self.automatic
    }

    pub fn obscure(&mut self, mouse: Point) -> bool
    {
        let changed = self.obscured != Some(mouse);
        self.obscured = Some(mouse);
        changed
    }

    /// Observe every input movement, even if the pointer later returns to its old position.
    pub fn mouse_moved(&mut self, mouse: Point) -> bool
    {
        let mut changed = false;
        if self.obscured.is_some_and(|at| at != mouse)
        {
            self.obscured = None;
            changed = true;
        }
        if self.deferred_hide.is_some_and(|at| at != mouse)
        {
            self.deferred_hide = None;
            changed = true;
        }
        changed
    }

    /// Animation frames own their palettes. Present/tick borrow the selected
    /// immutable frame instead of allocating a second palette on each advance.
    fn palette(&self) -> &[u32]
    {
        self.animation.as_ref().map_or(self.colors.as_slice(), |animation| {
                                   animation.frames[animation.index].1.as_slice()
                               })
    }

    pub fn snapshot(&self, mouse: Point) -> CursorSnapshot
    {
        let obscured = self.obscured == Some(mouse);
        let deferred = self.deferred_hide == Some(mouse);
        CursorSnapshot { cursor:      self.image,
                         visibility:  self.visibility,
                         flags:       if self.automatic { CURSOR_AUTOMATIC } else { 0 }
                                      | if obscured { CURSOR_OBSCURED } else { 0 }
                                      | if !obscured && (self.visibility == 0 || deferred)
                                      {
                                          CURSOR_VISIBLE
                                      }
                                      else
                                      {
                                          0
                                      }
                                      | if self.animation.is_some() { CURSOR_ANIMATED } else { 0 }
                                      | if deferred { CURSOR_DEFERRED_HIDE } else { 0 }
                                      | if self.palette().is_empty() { 0 } else { CURSOR_COLOR },
                         frame_index: self.animation.as_ref().map_or(0, |a| a.index as u32),
                         frame_count: self.animation.as_ref().map_or(1, |a| a.frames.len() as u32),
                         interval:    self.animation.as_ref().map_or(0, |a| a.interval),
                         resource_id: self.resource_id,
                         scope:       self.scope }
    }

    fn select(&mut self, image: MonoCursor, colors: Vec<u32>, automatic: bool, id: i16,
              scope: mac_abi::resources::Scope)
    {
        self.image = image;
        self.effective_hotspot = image.hotspot;
        self.colors = colors;
        self.automatic = automatic;
        self.animation = None;
        self.resource_id = id;
        self.scope = scope;
    }

    pub fn request(&mut self, call: &mut CursorCall, colors: &[u32], mouse: Point)
                   -> Result<bool, Status>
    {
        let before = self.snapshot(mouse);
        match call.operation
        {
            CURSOR_SET =>
            {
                validate_hotspot(call.cursor.hotspot)?;
                if !colors.is_empty() && colors.len() != 256
                {
                    return Err(BAD_ARGUMENT);
                }
                // Include palette equality: a new palette can change an identical bitmap.
                let palette_changed = self.palette() != colors;
                let owned = if !palette_changed
                {
                    // Transfer the already admitted palette, including when an
                    // explicit SetCursor ends an animation on its current frame.
                    if let Some(animation) = self.animation.as_mut()
                    {
                        core::mem::take(&mut animation.frames[animation.index].1)
                    }
                    else
                    {
                        core::mem::take(&mut self.colors)
                    }
                }
                else
                {
                    let mut owned = Vec::new();
                    owned.try_reserve_exact(colors.len()).map_err(|_| NO_MEMORY)?;
                    owned.extend_from_slice(colors);
                    owned
                };
                self.select(call.cursor, owned, false, 0, resources::CHAIN);
                call.visibility = self.visibility;
                return Ok(palette_changed || before != self.snapshot(mouse));
            }
            CURSOR_INIT => *self = Self::default(),
            CURSOR_HIDE =>
            {
                self.visibility = self.visibility.saturating_sub(1);
                self.deferred_hide = None;
            }
            CURSOR_SHOW =>
            {
                self.visibility = self.visibility.saturating_add(1).min(0);
                if self.visibility == 0
                {
                    self.deferred_hide = None;
                }
            }
            CURSOR_OBSCURE =>
            {
                self.obscure(mouse);
            }
            CURSOR_SHIELD =>
            {
                let hotspot = self.effective_hotspot;
                let left = i32::from(mouse.h) - i32::from(hotspot.h);
                let top = i32::from(mouse.v) - i32::from(hotspot.v);
                let intersects =
                    !call.bounds.is_empty()
                    && left < i32::from(call.bounds.right) - i32::from(call.offset.h)
                    && left + 16 > i32::from(call.bounds.left) - i32::from(call.offset.h)
                    && top < i32::from(call.bounds.bottom) - i32::from(call.offset.v)
                    && top + 16 > i32::from(call.bounds.top) - i32::from(call.offset.v);
                let visible = before.flags & CURSOR_VISIBLE != 0;
                self.visibility = self.visibility.saturating_sub(1);
                self.deferred_hide = if !intersects && visible { Some(mouse) } else { None };
            }
            CURSOR_STATE =>
            {}
            _ => return Err(NOT_SUPPORTED)
        }
        call.visibility = self.visibility;
        Ok(before != self.snapshot(mouse))
    }

    /// Resolve every animation frame before committing, so bad resources leave state unchanged.
    pub fn resource_request(&mut self, call: &mut CursorResourceCall, mouse: Point, now: u32,
                            mut lookup: impl FnMut([u8; 4],
                                  i16,
                                  mac_abi::resources::Scope)
                                  -> Result<Vec<u8>, Status>)
                            -> Result<bool, Status>
    {
        let before = self.snapshot(mouse);
        let mut palette_changed = false;
        match call.operation
        {
            CURSOR_RESOURCE_SET | CURSOR_RESOURCE_COLOR =>
            {
                let kind = if call.operation == CURSOR_RESOURCE_SET { *b"CURS" } else { *b"crsr" };
                let bytes = lookup(kind, call.id, call.scope)?;
                let (image, colors) = if call.operation == CURSOR_RESOURCE_SET
                {
                    (decode_mono(&bytes)?, Vec::new())
                }
                else
                {
                    decode_color(&bytes)?
                };
                palette_changed = self.palette() != colors.as_slice();
                self.select(image, colors, false, call.id, call.scope);
            }
            CURSOR_RESOURCE_BUILTIN =>
            {
                if !matches!(call.builtin, 0..=4 | 17 | 21)
                {
                    return Err(BAD_ARGUMENT);
                }
                let image = if call.builtin == 0
                {
                    super::cursor::arrow()
                }
                else
                {
                    let bytes = lookup(*b"CURS", call.builtin as i16, resources::SYSTEM)?;
                    decode_mono(&bytes)?
                };
                palette_changed = !self.palette().is_empty();
                self.select(image,
                            Vec::new(),
                            call.builtin == 0,
                            call.builtin as i16,
                            resources::SYSTEM);
            }
            CURSOR_RESOURCE_ANIMATE =>
            {
                if call.interval > i32::MAX as u32
                {
                    return Err(BAD_ARGUMENT);
                }
                let bytes = lookup(*b"acur", call.id, call.scope)?;
                let count = usize::from(word(&bytes, 0)?);
                if count == 0 || count > 256 || bytes.len() != 4 + count * 4
                {
                    return Err(BAD_ARGUMENT);
                }
                let mut frames = Vec::new();
                frames.try_reserve_exact(count).map_err(|_| NO_MEMORY)?;
                for index in 0..count
                {
                    let id = word(&bytes, 4 + index * 4)? as i16;
                    let mut frame = |kind| {
                        match lookup(kind, id, call.scope)
                        {
                            Err(NOT_FOUND)
                                if call.scope != resources::SYSTEM
                                   && call.scope != resources::CHAIN =>
                            {
                                lookup(kind, id, resources::SYSTEM)
                            }
                            result => result
                        }
                    };
                    frames.push(match frame(*b"crsr")
                                {
                                    Ok(bytes) => decode_color(&bytes)?,
                                    Err(NOT_FOUND) => (decode_mono(&frame(*b"CURS")?)?, Vec::new()),
                                    Err(error) => return Err(error)
                                });
                }
                palette_changed = self.palette() != frames[0].1.as_slice();
                self.select(frames[0].0, Vec::new(), false, call.id, call.scope);
                self.animation = Some(Animation { frames,
                                                  index: 0,
                                                  interval: call.interval,
                                                  last_tick: now });
            }
            CURSOR_RESOURCE_ADVANCE =>
            {
                let animation = self.animation.as_mut().ok_or(BAD_ARGUMENT)?;
                animation.index = (animation.index + 1) % animation.frames.len();
                animation.last_tick = now;
                self.image = animation.frames[animation.index].0;
                self.effective_hotspot = self.image.hotspot;
            }
            CURSOR_RESOURCE_STATE =>
            {}
            _ => return Err(NOT_SUPPORTED)
        }
        call.snapshot = self.snapshot(mouse);
        Ok(palette_changed || before != call.snapshot)
    }

    pub fn animation_tick(&mut self, now: u32) -> bool
    {
        let Some(animation) = self.animation.as_mut()
        else
        {
            return false;
        };
        if animation.interval == 0
        {
            return false;
        }
        let steps = now.wrapping_sub(animation.last_tick) / animation.interval;
        if steps == 0
        {
            return false;
        }
        animation.last_tick = animation.last_tick.wrapping_add(steps * animation.interval);
        let index = (animation.index + (steps % animation.frames.len() as u32) as usize)
                    % animation.frames.len();
        if index == animation.index
        {
            return false;
        }
        animation.index = index;
        self.image = animation.frames[index].0;
        self.effective_hotspot = self.image.hotspot;
        // Time continues while hidden, but invisible frames must not wake a
        // sleeping app or force the compositor to repaint the whole desktop.
        self.obscured.is_none() && (self.visibility == 0 || self.deferred_hide.is_some())
    }

    /// Borrow a sprite for the serialized present; never paint the scene.
    pub(crate) fn sprite(&mut self, mouse: Point, ibeam: Option<MonoCursor>)
                         -> Option<CursorSprite<'_>>
    {
        self.mouse_moved(mouse);
        if self.snapshot(mouse).flags & CURSOR_VISIBLE == 0
        {
            return None;
        }
        let cursor = if self.automatic { ibeam.unwrap_or(self.image) } else { self.image };
        self.effective_hotspot = cursor.hotspot;
        Some(CursorSprite::new(cursor, self.palette(), mouse))
    }
}

/// The invocation's resource pin stays held while its bounded value copy is
/// admitted. Lookup errors keep their identity instead of becoming NotFound.
pub(crate) fn copy_resource(bytes: &[u8]) -> Result<Vec<u8>, Status>
{
    let mut copy = Vec::new();
    copy.try_reserve_exact(bytes.len()).map_err(|_| NO_MEMORY)?;
    copy.extend_from_slice(bytes);
    Ok(copy)
}

fn validate_hotspot(point: Point) -> Result<(), Status>
{
    if (0..16).contains(&point.h) && (0..16).contains(&point.v)
    {
        Ok(())
    }
    else
    {
        Err(BAD_ARGUMENT)
    }
}

pub fn decode_mono(bytes: &[u8]) -> Result<MonoCursor, Status>
{
    if bytes.len() != 68
    {
        return Err(BAD_ARGUMENT);
    }
    let mut result = MonoCursor::default();
    for row in 0..16
    {
        result.image[row] = word(bytes, row * 2)?;
        result.mask[row] = word(bytes, 32 + row * 2)?;
    }
    result.hotspot = Point { v: word(bytes, 64)? as i16,
                             h: word(bytes, 66)? as i16 };
    validate_hotspot(result.hotspot)?;
    Ok(result)
}

fn word(bytes: &[u8], offset: usize) -> Result<u16, Status>
{
    let data = bytes.get(offset..offset + 2).ok_or(BAD_ARGUMENT)?;
    Ok(u16::from_be_bytes([data[0], data[1]]))
}
fn long(bytes: &[u8], offset: usize) -> Result<usize, Status>
{
    let data = bytes.get(offset..offset + 4).ok_or(BAD_ARGUMENT)?;
    Ok(u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize)
}

/// CCrsr uses resource-relative offsets, not serialized memory pointers.
/// Inside Macintosh: Imaging With QuickDraw, Cursor Utilities, pp. 8-35, 8-43.
pub fn decode_color(bytes: &[u8]) -> Result<(MonoCursor, Vec<u32>), Status>
{
    let header = bytes.get(..96).ok_or(BAD_ARGUMENT)?;
    let image = decode_mono(&header[20..88])?;
    match word(bytes, 0)?
    {
        0x8000 => return Ok((image, Vec::new())),
        0x8001 =>
        {}
        _ => return Err(BAD_ARGUMENT)
    }
    let map_offset = long(bytes, 2)?;
    let pixels_offset = long(bytes, 6)?;
    if map_offset < 96 || pixels_offset < 96
    {
        return Err(BAD_ARGUMENT);
    }
    let map = bytes.get(map_offset..map_offset.checked_add(50).ok_or(BAD_ARGUMENT)?)
                   .ok_or(BAD_ARGUMENT)?;
    let stride = usize::from(word(map, 4)? & 0x3fff);
    let depth = usize::from(word(map, 32)?);
    if !matches!(depth, 1 | 2 | 4 | 8)
       || word(map, 34)? != 1
       || usize::from(word(map, 36)?) != depth
       || word(map, 16)? > 1
       || word(map, 6)? != 0
       || word(map, 8)? != 0
       || word(map, 10)? != 16
       || word(map, 12)? != 16
       || stride < (16 * depth).div_ceil(8)
    {
        return Err(BAD_ARGUMENT);
    }
    let table_offset = long(map, 42)?;
    if table_offset < 96
    {
        return Err(BAD_ARGUMENT);
    }
    let table = bytes.get(table_offset..).ok_or(BAD_ARGUMENT)?;
    let count = usize::from(word(table, 6)?).checked_add(1).ok_or(BAD_ARGUMENT)?;
    if count > 256
    {
        return Err(BAD_ARGUMENT);
    }
    let table_length = 8 + count * 8;
    if table.len() < table_length
    {
        return Err(BAD_ARGUMENT);
    }
    let pixels_length = stride.checked_mul(16).ok_or(BAD_ARGUMENT)?;
    let pixels = bytes.get(pixels_offset
                           ..pixels_offset.checked_add(pixels_length).ok_or(BAD_ARGUMENT)?)
                      .ok_or(BAD_ARGUMENT)?;
    let ranges = [(map_offset, map_offset + 50),
                  (table_offset, table_offset + table_length),
                  (pixels_offset, pixels_offset + pixels_length)];
    for (index, &(start, end)) in ranges.iter().enumerate()
    {
        if ranges[index + 1..].iter().any(|&(other_start, other_end)| start < other_end && other_start < end)
        { return Err(BAD_ARGUMENT); }
    }
    let mut palette = [None; 256];
    for entry in 0..count
    {
        let offset = 8 + entry * 8;
        let index =
            if word(table, 4)? & 0x8000 != 0 { entry } else { usize::from(word(table, offset)?) };
        if index >= 256
        {
            return Err(BAD_ARGUMENT);
        }
        let rgb = (u32::from(word(table, offset + 2)? >> 8) << 16)
                  | (u32::from(word(table, offset + 4)? >> 8) << 8)
                  | u32::from(word(table, offset + 6)? >> 8);
        palette[index] = Some(rgb);
    }
    let mut colors = Vec::new();
    colors.try_reserve_exact(256).map_err(|_| NO_MEMORY)?;
    for row in 0..16
    {
        for column in 0..16
        {
            let bit = column * depth;
            let index = usize::from((pixels[row * stride + bit / 8] >> (8 - depth - bit % 8))
                                    & ((1u16 << depth) - 1) as u8);
            colors.push(palette[index].ok_or(BAD_ARGUMENT)?);
        }
    }
    Ok((image, colors))
}

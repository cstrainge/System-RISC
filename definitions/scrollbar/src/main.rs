//! Native color scrollbar CDEF1. Geometry, drawing and classic hit parts are
//! owned entirely by this resource; clients retain only scalar control state.
#![cfg_attr(target_arch = "riscv64", no_std)]
#![cfg_attr(target_arch = "riscv64", no_main)]
pub use mac_abi::controls::ScrollbarState;
use mac_abi::{controls::*, definitions::CDEF, *};
use toolbox_quickdraw::SurfacePort;
#[derive(Clone, Copy, Debug)]
pub struct Geometry
{
    pub horizontal: bool,
    pub first:      Rect,
    pub last:       Rect,
    pub track:      Rect,
    pub thumb:      Rect
}
pub fn geometry(state: &ScrollbarState) -> Result<Geometry, Status>
{
    let r = state.bounds;
    let width = r.right as i32 - r.left as i32;
    let height = r.bottom as i32 - r.top as i32;
    if width < 3
       || height < 3
       || state.maximum < 0
       || state.value < 0
       || state.value > state.maximum
    {
        return Err(BAD_ARGUMENT);
    }
    let horizontal = width > height;
    let length = if horizontal { width } else { height };
    let thickness = if horizontal { height } else { width };
    if !(15..=16).contains(&thickness) || length < thickness * 2
    {
        return Err(NOT_SUPPORTED);
    }
    let cap = thickness.min(length / 2);
    let start = if horizontal { r.left as i32 } else { r.top as i32 };
    let along = |a: i32, b: i32| {
        if horizontal
        {
            Rect::new(a as i16, r.top, b as i16, r.bottom)
        }
        else
        {
            Rect::new(r.left, a as i16, r.right, b as i16)
        }
    };
    let track_start = start + cap;
    let track_end = start + length - cap;
    let thumb_length = thickness.min((track_end - track_start).max(0));
    let position = if state.maximum > 0
    {
        (track_end - track_start - thumb_length) * state.value as i32 / state.maximum as i32
    }
    else
    {
        0
    };
    Ok(Geometry { horizontal,
                  first: along(start, start + cap),
                  last: along(start + length - cap, start + length),
                  track: along(track_start, track_end),
                  thumb: along(track_start + position, track_start + position + thumb_length) })
}
pub fn hit(state: &ScrollbarState, point: Point) -> i16
{
    let Ok(g) = geometry(state)
    else
    {
        return 0;
    };
    if state.active == 0 || state.maximum == 0 || !state.bounds.contains(point)
    {
        return 0;
    }
    let along = if g.horizontal { point.h } else { point.v };
    let first_end = if g.horizontal { g.first.right } else { g.first.bottom };
    // Original TestControl includes the first track shadow strip in the arrow.
    if along <= first_end
    {
        return SCROLL_UP;
    }
    if g.last.contains(point)
    {
        return SCROLL_DOWN;
    }
    let mut inner = g.thumb;
    if g.horizontal
    {
        inner.left += 1;
        inner.top += 1;
        inner.bottom -= 1;
    }
    else
    {
        inner.top += 1;
        inner.left += 1;
        inner.right -= 1;
    }
    if inner.contains(point)
    {
        return SCROLL_THUMB;
    }
    let middle = if g.horizontal
    {
        (g.thumb.left as i32 + g.thumb.right as i32) / 2
    }
    else
    {
        (g.thumb.top as i32 + g.thumb.bottom as i32) / 2
    };
    if (along as i32) < middle { SCROLL_PAGE_UP } else { SCROLL_PAGE_DOWN }
}
/// Return a value for relative thumb movement, retaining the mouse-down grab
/// offset and initial value. This neither draws nor modifies the control.
pub fn track_value(state: &ScrollbarState, anchor: Point, point: Point) -> Result<i16, Status>
{
    let g = geometry(state)?;
    let travel = if g.horizontal
    {
        g.track.right as i32 - g.track.left as i32 - (g.thumb.right as i32 - g.thumb.left as i32)
    }
    else
    {
        g.track.bottom as i32 - g.track.top as i32 - (g.thumb.bottom as i32 - g.thumb.top as i32)
    };
    if travel <= 0 || state.active == 0 || state.maximum == 0
    {
        return Ok(state.value);
    }
    let delta = if g.horizontal
    {
        point.h as i32 - anchor.h as i32
    }
    else
    {
        point.v as i32 - anchor.v as i32
    };
    let numerator=(state.value as i64*travel as i64+delta as i64*state.maximum as i64).clamp(0,state.maximum as i64*travel as i64);
    Ok(((numerator + travel as i64 / 2) / travel as i64) as i16)
}

/// Draw the resource's complete scrollbar at window-local bounds. The supplied
/// surface connection applies the caller's origin and clipping on every span.
pub fn draw(canvas: &mut SurfacePort<'_>, state: &ScrollbarState) -> mac_traps::Result<()>
{
    let g = geometry(state).map_err(mac_traps::Error)?;
    let active = state.active != 0 && state.maximum > 0;
    let thickness = if g.horizontal
    {
        state.bounds.bottom - state.bounds.top
    }
    else
    {
        state.bounds.right - state.bounds.left
    };
    let base = (thickness as usize - 15) * 16 + usize::from(g.horizontal) * 8;
    canvas.paint_rect(state.bounds, 0xf3f3f3)?;
    canvas.frame_rect(state.bounds, 0)?;
    if active
    {
        let r = if g.horizontal
        {
            Rect::new(g.track.left, g.track.top + 1, g.track.right, g.track.bottom - 1)
        }
        else
        {
            Rect::new(g.track.left + 1, g.track.top, g.track.right - 1, g.track.bottom)
        };
        canvas.fill_rect(r, [0x88, 0x22, 0x88, 0x22, 0x88, 0x22, 0x88, 0x22], 0x969696, 0xe7e7e7)?;
    }
    let first = if !active
    {
        2
    }
    else if state.highlighted == SCROLL_UP
    {
        4
    }
    else
    {
        0
    };
    let last = if !active
    {
        3
    }
    else if state.highlighted == SCROLL_DOWN
    {
        5
    }
    else
    {
        1
    };
    sprite(canvas, g.first, SPRITES[base + first], thickness)?;
    sprite(canvas, g.last, SPRITES[base + last], thickness)?;
    if active
       && (if g.horizontal { g.thumb.right - g.thumb.left } else { g.thumb.bottom - g.thumb.top })
          == thickness
    {
        sprite(canvas,
               g.thumb,
               SPRITES[base + if state.highlighted == SCROLL_THUMB { 7 } else { 6 }],
               thickness)?;
    }
    Ok(())
}
fn sprite(canvas: &mut SurfacePort<'_>, bounds: Rect, data: &[u8], size: i16)
          -> mac_traps::Result<()>
{
    for row in 0..size
    {
        let mut x = 0;
        while x < size
        {
            let color = data[(row * size + x) as usize];
            let mut end = x + 1;
            while end < size && data[(row * size + end) as usize] == color
            {
                end += 1;
            }
            canvas.paint_rect(Rect::new(bounds.left + x,
                                        bounds.top + row,
                                        bounds.left + end,
                                        bounds.top + row + 1),
                              COLORS[(color - b'A') as usize])?;
            x = end;
        }
    }
    Ok(())
}
/// # Safety
/// The caller supplies live native state and an exclusive surface for drawing.
pub unsafe extern "C" fn scrollbar_definition(pointer: *mut DefinitionCall) -> i64
{
    let Some(call) = (unsafe { pointer.as_mut() })
    else
    {
        return BAD_ARGUMENT as i64;
    };
    if !call.header.supports::<DefinitionCall>()
    {
        return BAD_ABI as i64;
    }
    if call.kind != CDEF || call.id != 1 || call.variation != 0
    {
        return NOT_SUPPORTED as i64;
    }
    let Some(state) = (unsafe { call.object.cast::<ScrollbarState>().as_ref() })
    else
    {
        return BAD_ARGUMENT as i64;
    };
    match call.message
    {
        0 =>
        {
            let Some(surface) = (unsafe { call.surface.as_mut() })
            else
            {
                return BAD_ARGUMENT as i64;
            };
            let connection = unsafe { mac_traps::SurfaceConnection::new(surface, call.gate) };
            draw(&mut SurfacePort::new(connection), state).map_or_else(|error| error.0 as i64,
                                                                       |_| 0)
        }
        1 =>
        {
            let bits = call.param as u32;
            hit(state,
                Point { h: bits as i16,
                        v: (bits >> 16) as i16 }) as i64
        }
        2 =>
        {
            let bits = call.param as u64;
            let point = Point { h: bits as i16,
                                v: (bits >> 16) as i16 };
            let anchor = Point { h: (bits >> 32) as i16,
                                 v: (bits >> 48) as i16 };
            track_value(state, anchor, point).map_or_else(|status| status as i64,
                                                          |value| value as i64)
        }
        3..=4 => 0,
        mac_abi::controls::VALIDATE_DEFINITION => geometry(state).map_or_else(|e| e as i64, |_| 0),
        _ => NOT_SUPPORTED as i64
    }
}
/// # Safety
/// System supplies a live native definition call and exclusive borrowed state/surface.
#[cfg(target_arch = "riscv64")]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(call: *mut DefinitionCall) -> i64
{
    unsafe { scrollbar_definition(call) }
}
#[cfg(target_arch = "riscv64")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> !
{
    loop
    {
        core::hint::spin_loop();
    }
}
// BEGIN GENERATED BOUNDED SCROLLBAR ASSETS
// Generated by scripts/extract_scrollbar_assets.py from original System7.6.1.
const COLORS: [u32; 11] = [0x000000, 0x545487, 0x777777, 0x8787b3, 0x969696, 0xb3b3da, 0xc0c0c0,
                           0xdadaff, 0xe7e7e7, 0xf3f3f3, 0xffffff];
const SPRITES: [&[u8];32] = [
    b"AAAAAAAAAAAAAAAAKKKKKKKKKKKKEAAKIIIIIIIIIIIEAAKIIIIIBIIIIIEAAKIIIIBBIIIIIEAAKIIIBFFBIIIIEAAKIIBFFFFBIIIEAAKBBBBFFBBBBIEAAKIIIBFFBIIIIEAAKIIIBFFBIIIIEAAKIIIBFFBIIIIEAAKIIIBBBBIIIIEAAKIIIIIIIIIIIEAAEEEEEEEEEEEEEAAAAAAAAAAAAAAAA", // 15_v_first_active
    b"AAAAAAAAAAAAAAAAKKKKKKKKKKKKEAAKIIIIIIIIIIIEAAKIIIBBBBIIIIEAAKIIIBFFBIIIIEAAKIIIBFFBIIIIEAAKIIIBFFBIIIIEAAKBBBBFFBBBBIEAAKIIBFFFFBIIIEAAKIIIBFFBIIIIEAAKIIIIBBIIIIIEAAKIIIIIBIIIIIEAAKIIIIIIIIIIIEAAEEEEEEEEEEEEEAAAAAAAAAAAAAAAA", // 15_v_last_active
    b"AAAAAAAAAAAAAAAAJJJJJJJJJJJJJAAJJJJJJJJJJJJJAAJJJJJJEJJJJJJAAJJJJJEJEJJJJJAAJJJJEJJJEJJJJAAJJJEJJJJJEJJJAAJEEEEJJJEEEEJAAJJJJEJJJEJJJJAAJJJJEJJJEJJJJAAJJJJEJJJEJJJJAAJJJJEEEEEJJJJAAJJJJJJJJJJJJJAAJJJJJJJJJJJJJAAEEEEEEEEEEEEEA", // 15_v_first_disabled
    b"AEEEEEEEEEEEEEAAJJJJJJJJJJJJJAAJJJJJJJJJJJJJAAJJJJEEEEEJJJJAAJJJJEJJJEJJJJAAJJJJEJJJEJJJJAAJJJJEJJJEJJJJAAJEEEEJJJEEEEJAAJJJEJJJJJEJJJAAJJJJEJJJEJJJJAAJJJJJEJEJJJJJAAJJJJJJEJJJJJJAAJJJJJJJJJJJJJAAJJJJJJJJJJJJJAAAAAAAAAAAAAAAA", // 15_v_last_disabled
    b"AAAAAAAAAAAAAAAAKKKKKKKKKKKKEAAKIIIIIIIIIIIEAAKIIIIIAIIIIIEAAKIIIIAAIIIIIEAAKIIIAAAAIIIIEAAKIIAAAAAAIIIEAAKAAAAAAAAAAIEAAKIIIAAAAIIIIEAAKIIIAAAAIIIIEAAKIIIAAAAIIIIEAAKIIIAAAAIIIIEAAKIIIIIIIIIIIEAAEEEEEEEEEEEEEAAAAAAAAAAAAAAAA", // 15_v_first_pressed
    b"AAAAAAAAAAAAAAAAKKKKKKKKKKKKEAAKIIIIIIIIIIIEAAKIIIAAAAIIIIEAAKIIIAAAAIIIIEAAKIIIAAAAIIIIEAAKIIIAAAAIIIIEAAKAAAAAAAAAAIEAAKIIAAAAAAIIIEAAKIIIAAAAIIIIEAAKIIIIAAIIIIIEAAKIIIIIAIIIIIEAAKIIIIIIIIIIIEAAEEEEEEEEEEEEEAAAAAAAAAAAAAAAA", // 15_v_last_pressed
    b"ACCCCCCCCCCCCCAAHHHHHHHHHHHHBAAHGGGGGGGGGGGBAAHGGGFFFFFGGGBAAHGGGHHHHHGGGBAAHGGGDDDDDGGGBAAHGGGHHHHHGGGBAAHGGGDDDDDGGGBAAHGGGDDDDDGGGBAAHGGGHHHHHGGGBAAHGGGDDDDDGGGBAAHGGGHHHHHGGGBAAHGGGGGGGGGGGBAAHGGGGGGGGGGGBAABBBBBBBBBBBBBA", // 15_v_thumb_active
    b"ACCCCCCCCCCCCCAAHHHHHHHHHHHHBAAHGGGGGGGGGGGBAAHGGGFFFFFGGGBAAHGGGHHHHHGGGBAAHGGGDDDDDGGGBAAHGGGHHHHHGGGBAAHGGGDDDDDGGGBAAHGGGDDDDDGGGBAAHGGGHHHHHGGGBAAHGGGDDDDDGGGBAAHGGGHHHHHGGGBAAHGGGGGGGGGGGBAAHGGGGGGGGGGGBAABBBBBBBBBBBBBA", // 15_v_thumb_pressed
    b"AAAAAAAAAAAAAAAAKKKKKKKKKKKKEAAKIIIIIBIIIIIEAAKIIIIIBIIIIIEAAKIIIIBBIIIIIEAAKIIIBFBBBBBIEAAKIIBFFFFFFBIEAAKIBBFFFFFFBIEAAKIIIBFBBBBBIEAAKIIIIBBIIIIIEAAKIIIIIBIIIIIEAAKIIIIIBIIIIIEAAKIIIIIIIIIIIEAAEEEEEEEEEEEEEAAAAAAAAAAAAAAAA", // 15_h_first_active
    b"AAAAAAAAAAAAAAAAKKKKKKKKKKKKEAAKIIIIIBIIIIIEAAKIIIIIBIIIIIEAAKIIIIIBBIIIIEAAKIBBBBBFBIIIEAAKIBFFFFFFBIIEAAKIBFFFFFFBBIEAAKIBBBBBFBIIIEAAKIIIIIBBIIIIEAAKIIIIIBIIIIIEAAKIIIIIBIIIIIEAAKIIIIIIIIIIIEAAEEEEEEEEEEEEEAAAAAAAAAAAAAAAA", // 15_h_last_active
    b"AAAAAAAAAAAAAAAAJJJJJJJJJJJJJEAJJJJJJEJJJJJJEAJJJJJEEJJJJJJEAJJJJEJEJJJJJJEAJJJEJJEEEEJJJEAJJEJJJJJJEJJJEAJEJJJJJJJEJJJEAJJEJJJJJJEJJJEAJJJEJJEEEEJJJEAJJJJEJEJJJJJJEAJJJJJEEJJJJJJEAJJJJJJEJJJJJJEAJJJJJJJJJJJJJEAAAAAAAAAAAAAAA", // 15_h_first_disabled
    b"AAAAAAAAAAAAAAAEJJJJJJJJJJJJJAEJJJJJJEJJJJJJAEJJJJJJEEJJJJJAEJJJJJJEJEJJJJAEJJJEEEEJJEJJJAEJJJEJJJJJJEJJAEJJJEJJJJJJJEJAEJJJEJJJJJJEJJAEJJJEEEEJJEJJJAEJJJJJJEJEJJJJAEJJJJJJEEJJJJJAEJJJJJJEJJJJJJAEJJJJJJJJJJJJJAAAAAAAAAAAAAAAA", // 15_h_last_disabled
    b"AAAAAAAAAAAAAAAAKKKKKKKKKKKKEAAKIIIIIAIIIIIEAAKIIIIIAIIIIIEAAKIIIIAAIIIIIEAAKIIIAAAAAAAIEAAKIIAAAAAAAAIEAAKIAAAAAAAAAIEAAKIIIAAAAAAAIEAAKIIIIAAIIIIIEAAKIIIIIAIIIIIEAAKIIIIIAIIIIIEAAKIIIIIIIIIIIEAAEEEEEEEEEEEEEAAAAAAAAAAAAAAAA", // 15_h_first_pressed
    b"AAAAAAAAAAAAAAAAKKKKKKKKKKKKEAAKIIIIIAIIIIIEAAKIIIIIAIIIIIEAAKIIIIIAAIIIIEAAKIAAAAAAAIIIEAAKIAAAAAAAAIIEAAKIAAAAAAAAAIEAAKIAAAAAAAIIIEAAKIIIIIAAIIIIEAAKIIIIIAIIIIIEAAKIIIIIAIIIIIEAAKIIIIIIIIIIIEAAEEEEEEEEEEEEEAAAAAAAAAAAAAAAA", // 15_h_last_pressed
    b"AAAAAAAAAAAAAAACHHHHHHHHHHHHHBCHGGGGGGGGGGGGBCHGGGGGGGGGGGGBCHGGGGGGGGGGGGBCHGFHDHDDHDHGGBCHGFHDHDDHDHGGBCHGFHDHDDHDHGGBCHGFHDHDDHDHGGBCHGFHDHDDHDHGGBCHGGGGGGGGGGGGBCHGGGGGGGGGGGGBCHGGGGGGGGGGGGBCBBBBBBBBBBBBBBAAAAAAAAAAAAAAA", // 15_h_thumb_active
    b"AAAAAAAAAAAAAAACHHHHHHHHHHHHHBCHGGGGGGGGGGGGBCHGGGGGGGGGGGGBCHGGGGGGGGGGGGBCHGFHDHDDHDHGGBCHGFHDHDDHDHGGBCHGFHDHDDHDHGGBCHGFHDHDDHDHGGBCHGFHDHDDHDHGGBCHGGGGGGGGGGGGBCHGGGGGGGGGGGGBCHGGGGGGGGGGGGBCBBBBBBBBBBBBBBAAAAAAAAAAAAAAA", // 15_h_thumb_pressed
    b"AAAAAAAAAAAAAAAAAKKKKKKKKKKKKKEAAKIIIIIIIIIIIIEAAKIIIIIBIIIIIIEAAKIIIIBFBIIIIIEAAKIIIBFFFBIIIIEAAKIIBFFFFFBIIIEAAKIBFFFFFFFBIIEAAKBBBBFFFBBBBIEAAKIIIBFFFBIIIIEAAKIIIBFFFBIIIIEAAKIIIBFFFBIIIIEAAKIIIBBBBBIIIIEAAKIIIIIIIIIIIIEAAEEEEEEEEEEEEEEAAAAAAAAAAAAAAAAA", // 16_v_first_active
    b"AAAAAAAAAAAAAAAAAKKKKKKKKKKKKKEAAKIIIIIIIIIIIIEAAKIIIBBBBBIIIIEAAKIIIBFFFBIIIIEAAKIIIBFFFBIIIIEAAKIIIBFFFBIIIIEAAKBBBBFFFBBBBIEAAKIBFFFFFFFBIIEAAKIIBFFFFFBIIIEAAKIIIBFFFBIIIIEAAKIIIIBFBIIIIIEAAKIIIIIBIIIIIIEAAKIIIIIIIIIIIIEAAEEEEEEEEEEEEEEAAAAAAAAAAAAAAAAA", // 16_v_last_active
    b"AAAAAAAAAAAAAAAAAJJJJJJJJJJJJJJAAJJJJJJJJJJJJJJAAJJJJJJEEJJJJJJAAJJJJJEJJEJJJJJAAJJJJEJJJJEJJJJAAJJJEJJJJJJEJJJAAJJEJJJJJJJJEJJAAJEEEEJJJJEEEEJAAJJJJEJJJJEJJJJAAJJJJEJJJJEJJJJAAJJJJEJJJJEJJJJAAJJJJEEEEEEJJJJAAJJJJJJJJJJJJJJAAJJJJJJJJJJJJJJAAEEEEEEEEEEEEEEA", // 16_v_first_disabled
    b"AEEEEEEEEEEEEEEAAJJJJJJJJJJJJJJAAJJJJJJJJJJJJJJAAJJJJEEEEEEJJJJAAJJJJEJJJJEJJJJAAJJJJEJJJJEJJJJAAJJJJEJJJJEJJJJAAJEEEEJJJJEEEEJAAJJEJJJJJJJJEJJAAJJJEJJJJJJEJJJAAJJJJEJJJJEJJJJAAJJJJJEJJEJJJJJAAJJJJJJEEJJJJJJAAJJJJJJJJJJJJJJAAJJJJJJJJJJJJJJAAAAAAAAAAAAAAAAA", // 16_v_last_disabled
    b"AAAAAAAAAAAAAAAAAKKKKKKKKKKKKKEAAKIIIIIIIIIIIIEAAKIIIIIAIIIIIIEAAKIIIIAAAIIIIIEAAKIIIAAAAAIIIIEAAKIIAAAAAAAIIIEAAKIAAAAAAAAAIIEAAKAAAAAAAAAAAIEAAKIIIAAAAAIIIIEAAKIIIAAAAAIIIIEAAKIIIAAAAAIIIIEAAKIIIAAAAAIIIIEAAKIIIIIIIIIIIIEAAEEEEEEEEEEEEEEAAAAAAAAAAAAAAAAA", // 16_v_first_pressed
    b"AAAAAAAAAAAAAAAAAKKKKKKKKKKKKKEAAKIIIIIIIIIIIIEAAKIIIAAAAAIIIIEAAKIIIAAAAAIIIIEAAKIIIAAAAAIIIIEAAKIIIAAAAAIIIIEAAKAAAAAAAAAAAIEAAKIAAAAAAAAAIIEAAKIIAAAAAAAIIIEAAKIIIAAAAAIIIIEAAKIIIIAAAIIIIIEAAKIIIIIAIIIIIIEAAKIIIIIIIIIIIIEAAEEEEEEEEEEEEEEAAAAAAAAAAAAAAAAA", // 16_v_last_pressed
    b"ACCCCCCCCCCCCCCAAHHHHHHHHHHHHHBAAHGGGGGGGGGGGGBAAHGGGFFFFFFGGGBAAHGGGHHHHHHGGGBAAHGGGDDDDDDGGGBAAHGGGHHHHHHGGGBAAHGGGDDDDDDGGGBAAHGGGHHHHHHGGGBAAHGGGDDDDDDGGGBAAHGGGHHHHHHGGGBAAHGGGDDDDDDGGGBAAHGGGHHHHHHGGGBAAHGGGGGGGGGGGGBAAHGGGGGGGGGGGGBAABBBBBBBBBBBBBBA", // 16_v_thumb_active
    b"ACCCCCCCCCCCCCCAAHHHHHHHHHHHHHBAAHGGGGGGGGGGGGBAAHGGGFFFFFFGGGBAAHGGGHHHHHHGGGBAAHGGGDDDDDDGGGBAAHGGGHHHHHHGGGBAAHGGGDDDDDDGGGBAAHGGGHHHHHHGGGBAAHGGGDDDDDDGGGBAAHGGGHHHHHHGGGBAAHGGGDDDDDDGGGBAAHGGGHHHHHHGGGBAAHGGGGGGGGGGGGBAAHGGGGGGGGGGGGBAABBBBBBBBBBBBBBA", // 16_v_thumb_pressed
    b"AAAAAAAAAAAAAAAAAKKKKKKKKKKKKKEAAKIIIIIIBIIIIIEAAKIIIIIBBIIIIIEAAKIIIIBFBIIIIIEAAKIIIBFFBBBBBIEAAKIIBFFFFFFFBIEAAKIBFFFFFFFFBIEAAKIIBFFFFFFFBIEAAKIIIBFFBBBBBIEAAKIIIIBFBIIIIIEAAKIIIIIBBIIIIIEAAKIIIIIIBIIIIIEAAKIIIIIIIIIIIIEAAEEEEEEEEEEEEEEAAAAAAAAAAAAAAAAA", // 16_h_first_active
    b"AAAAAAAAAAAAAAAAAKKKKKKKKKKKKKEAAKIIIIIBIIIIIIEAAKIIIIIBBIIIIIEAAKIIIIIBFBIIIIEAAKIBBBBBFFBIIIEAAKIBFFFFFFFBIIEAAKIBFFFFFFFFBIEAAKIBFFFFFFFBIIEAAKIBBBBBFFBIIIEAAKIIIIIBFBIIIIEAAKIIIIIBBIIIIIEAAKIIIIIBIIIIIIEAAKIIIIIIIIIIIIEAAEEEEEEEEEEEEEEAAAAAAAAAAAAAAAAA", // 16_h_last_active
    b"AAAAAAAAAAAAAAAAAJJJJJJJJJJJJJJEAJJJJJJEJJJJJJJEAJJJJJEEJJJJJJJEAJJJJEJEJJJJJJJEAJJJEJJEEEEEJJJEAJJEJJJJJJJEJJJEAJEJJJJJJJJEJJJEAJEJJJJJJJJEJJJEAJJEJJJJJJJEJJJEAJJJEJJEEEEEJJJEAJJJJEJEJJJJJJJEAJJJJJEEJJJJJJJEAJJJJJJEJJJJJJJEAJJJJJJJJJJJJJJEAAAAAAAAAAAAAAAA", // 16_h_first_disabled
    b"AAAAAAAAAAAAAAAAEJJJJJJJJJJJJJJAEJJJJJJJEJJJJJJAEJJJJJJJEEJJJJJAEJJJJJJJEJEJJJJAEJJJEEEEEJJEJJJAEJJJEJJJJJJJEJJAEJJJEJJJJJJJJEJAEJJJEJJJJJJJJEJAEJJJEJJJJJJJEJJAEJJJEEEEEJJEJJJAEJJJJJJJEJEJJJJAEJJJJJJJEEJJJJJAEJJJJJJJEJJJJJJAEJJJJJJJJJJJJJJAAAAAAAAAAAAAAAAA", // 16_h_last_disabled
    b"AAAAAAAAAAAAAAAAAKKKKKKKKKKKKKEAAKIIIIIIAIIIIIEAAKIIIIIAAIIIIIEAAKIIIIAAAIIIIIEAAKIIIAAAAAAAAIEAAKIIAAAAAAAAAIEAAKIAAAAAAAAAAIEAAKIIAAAAAAAAAIEAAKIIIAAAAAAAAIEAAKIIIIAAAIIIIIEAAKIIIIIAAIIIIIEAAKIIIIIIAIIIIIEAAKIIIIIIIIIIIIEAAEEEEEEEEEEEEEEAAAAAAAAAAAAAAAAA", // 16_h_first_pressed
    b"AAAAAAAAAAAAAAAAAKKKKKKKKKKKKKEAAKIIIIIAIIIIIIEAAKIIIIIAAIIIIIEAAKIIIIIAAAIIIIEAAKIAAAAAAAAIIIEAAKIAAAAAAAAAIIEAAKIAAAAAAAAAAIEAAKIAAAAAAAAAIIEAAKIAAAAAAAAIIIEAAKIIIIIAAAIIIIEAAKIIIIIAAIIIIIEAAKIIIIIAIIIIIIEAAKIIIIIIIIIIIIEAAEEEEEEEEEEEEEEAAAAAAAAAAAAAAAAA", // 16_h_last_pressed
    b"AAAAAAAAAAAAAAAACHHHHHHHHHHHHHHBCHGGGGGGGGGGGGGBCHGGGGGGGGGGGGGBCHGGGGGGGGGGGGGBCHGFHDHDHDHDHGGBCHGFHDHDHDHDHGGBCHGFHDHDHDHDHGGBCHGFHDHDHDHDHGGBCHGFHDHDHDHDHGGBCHGFHDHDHDHDHGGBCHGGGGGGGGGGGGGBCHGGGGGGGGGGGGGBCHGGGGGGGGGGGGGBCBBBBBBBBBBBBBBBAAAAAAAAAAAAAAAA", // 16_h_thumb_active
    b"AAAAAAAAAAAAAAAACHHHHHHHHHHHHHHBCHGGGGGGGGGGGGGBCHGGGGGGGGGGGGGBCHGGGGGGGGGGGGGBCHGFHDHDHDHDHGGBCHGFHDHDHDHDHGGBCHGFHDHDHDHDHGGBCHGFHDHDHDHDHGGBCHGFHDHDHDHDHGGBCHGFHDHDHDHDHGGBCHGGGGGGGGGGGGGBCHGGGGGGGGGGGGGBCHGGGGGGGGGGGGGBCBBBBBBBBBBBBBBBAAAAAAAAAAAAAAAA", // 16_h_thumb_pressed
];
// END GENERATED BOUNDED SCROLLBAR ASSETS

#[cfg(test)]
mod tests
{
    use super::*;
    unsafe extern "C" fn unused_gate(_: u32, _: *mut (), _: usize) -> i64
    {
        NOT_SUPPORTED as i64
    }
    #[test]
    fn original_thumb_positions_geometry_and_interior_hit_parts()
    {
        let mut state = ScrollbarState { bounds: Rect::new(400, 100, 416, 280),
                                         maximum: 100,
                                         active: 1,
                                         ..Default::default() };
        for (value, start) in [(0, 116), (50, 182), (100, 248)]
        {
            state.value = value;
            let g = geometry(&state).unwrap();
            assert_eq!(g.thumb, Rect::new(400, start, 416, start + 16));
            assert_eq!(g.first, Rect::new(400, 100, 416, 116));
            assert_eq!(g.last, Rect::new(400, 264, 416, 280));
            assert_eq!(hit(&state,
                           Point { h: 408,
                                   v: start + 8 }),
                       SCROLL_THUMB);
        }
        state.value = 50;
        for (v, part) in
            [(108, SCROLL_UP), (272, SCROLL_DOWN), (150, SCROLL_PAGE_UP), (220, SCROLL_PAGE_DOWN)]
        {
            assert_eq!(hit(&state,
                           Point { h: 408,
                                   v }),
                       part);
        }
        assert_eq!(hit(&state,
                       Point { h: 399,
                               v: 190 }),
                   0);
        state.active = 0;
        assert_eq!(hit(&state,
                       Point { h: 408,
                               v: 190 }),
                   0);
        state.active = 1;
        state.value = 0;
        state.maximum = 0;
        assert_eq!(hit(&state,
                       Point { h: 408,
                               v: 108 }),
                   0);
        state.bounds = Rect::new(100, 280, 380, 295);
        state.maximum = 100;
        state.value = 50;
        assert_eq!(geometry(&state).unwrap().thumb, Rect::new(232, 280, 247, 295));
        state.value = 101;
        assert_eq!(geometry(&state).unwrap_err(), BAD_ARGUMENT);
        state.value = 0;
        state.bounds = Rect::new(100, 100, 114, 280);
        assert_eq!(geometry(&state).unwrap_err(), NOT_SUPPORTED);
    }
    #[test]
    fn thumb_tracking_keeps_grab_offset_and_clamps_at_extremes()
    {
        let mut state = ScrollbarState { bounds: Rect::new(400, 100, 416, 280),
                                         value: 17,
                                         maximum: 100,
                                         active: 1,
                                         ..Default::default() };
        let anchor = Point { h: 402,
                             v: 150 };
        assert_eq!(track_value(&state, anchor, anchor).unwrap(), 17);
        assert_eq!(track_value(&state,
                               anchor,
                               Point { h: 402,
                                       v: 216 }).unwrap(),
                   67);
        assert_eq!(track_value(&state,
                               anchor,
                               Point { h: 402,
                                       v: i16::MAX }).unwrap(),
                   100);
        assert_eq!(track_value(&state,
                               anchor,
                               Point { h: 402,
                                       v: i16::MIN }).unwrap(),
                   0);
        assert_eq!(track_value(&state,
                               anchor,
                               Point { h: 100,
                                       v: 150 }).unwrap(),
                   17);
        state.bounds = Rect::new(100, 305, 380, 321);
        state.value = 25;
        assert_eq!(track_value(&state,
                               Point { h: 150,
                                       v: 310 },
                               Point { h: 266,
                                       v: 310 }).unwrap(),
                   75);
        state.active = 0;
        assert_eq!(track_value(&state,
                               anchor,
                               Point { h: 300,
                                       v: 300 }).unwrap(),
                   25);
    }

    #[test]
    fn definition_protocol_rejects_bad_abi_identity_and_borrows()
    {
        let mut state = ScrollbarState { bounds: Rect::new(0, 0, 16, 180),
                                         maximum: 100,
                                         active: 1,
                                         ..Default::default() };
        let mut call = DefinitionCall { header:    Header::for_type::<DefinitionCall>(),
                                        kind:      CDEF,
                                        id:        1,
                                        variation: 0,
                                        message:   1,
                                        reserved:  0,
                                        param:     (8 << 16) | 8,
                                        object:    (&mut state as *mut ScrollbarState).cast(),
                                        gate:      unused_gate,
                                        surface:   core::ptr::null_mut() };
        assert_eq!(unsafe { scrollbar_definition(&mut call) }, SCROLL_UP as i64);
        call.message = 0;
        assert_eq!(unsafe { scrollbar_definition(&mut call) }, BAD_ARGUMENT as i64);
        call.message = 99;
        assert_eq!(unsafe { scrollbar_definition(&mut call) }, NOT_SUPPORTED as i64);
        call.message = 1;
        call.object = core::ptr::null_mut();
        assert_eq!(unsafe { scrollbar_definition(&mut call) }, BAD_ARGUMENT as i64);
        call.id = 0;
        assert_eq!(unsafe { scrollbar_definition(&mut call) }, NOT_SUPPORTED as i64);
        call.header.version = 0;
        assert_eq!(unsafe { scrollbar_definition(&mut call) }, BAD_ABI as i64);
        assert_eq!(unsafe { scrollbar_definition(core::ptr::null_mut()) }, BAD_ARGUMENT as i64);
    }
}

//! Native color scrollbar CDEF1. Geometry, drawing and classic hit parts are
//! owned entirely by this resource; clients retain only scalar control state.
#![cfg_attr(target_arch = "riscv64", no_std)]
#![cfg_attr(target_arch = "riscv64", no_main)]
#![deny(unsafe_code)]
use mac_definition::{Error, Message, Part, Point, Rect, Reply, Result, Scrollbar, Services};
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
pub fn geometry(state: &Scrollbar) -> Result<Geometry>
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
        return Err(Error::BadArgument);
    }
    let horizontal = width > height;
    let length = if horizontal { width } else { height };
    let thickness = if horizontal { height } else { width };
    if !(15..=16).contains(&thickness) || length < thickness * 2
    {
        return Err(Error::NotSupported);
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
pub fn hit(state: &Scrollbar, point: Point) -> Part
{
    let Ok(g) = geometry(state)
    else
    {
        return Part::None;
    };
    if !state.active || state.maximum == 0 || !state.bounds.contains(point)
    {
        return Part::None;
    }
    let along = if g.horizontal { point.h } else { point.v };
    let first_end = if g.horizontal { g.first.right } else { g.first.bottom };
    // Original TestControl includes the first track shadow strip in the arrow.
    if along <= first_end
    {
        return Part::Up;
    }
    if g.last.contains(point)
    {
        return Part::Down;
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
        return Part::Thumb;
    }
    let middle = if g.horizontal
    {
        (g.thumb.left as i32 + g.thumb.right as i32) / 2
    }
    else
    {
        (g.thumb.top as i32 + g.thumb.bottom as i32) / 2
    };
    if (along as i32) < middle { Part::PageUp } else { Part::PageDown }
}
/// Return a value for relative thumb movement, retaining the mouse-down grab
/// offset and initial value. This neither draws nor modifies the control.
pub fn track_value(state: &Scrollbar, anchor: Point, point: Point) -> Result<i16>
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
    if travel <= 0 || !state.active || state.maximum == 0
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
pub fn draw(canvas: &mut SurfacePort<'_>, state: &Scrollbar, theme: mac_definition::Theme)
            -> mac_traps::Result<()>
{
    let g = geometry(state)?;
    let active = state.active && state.maximum > 0;
    let thickness = if g.horizontal
    {
        state.bounds.bottom - state.bounds.top
    }
    else
    {
        state.bounds.right - state.bounds.left
    };
    let colors = theme.scrollbar.sprite_palette();
    let base = (thickness as usize - 15) * 16 + usize::from(g.horizontal) * 8;
    canvas.paint_rect(state.bounds, theme.scrollbar.face_light)?;
    canvas.frame_rect(state.bounds, theme.scrollbar.ink)?;
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
        canvas.fill_rect(r,
                         [0x88, 0x22, 0x88, 0x22, 0x88, 0x22, 0x88, 0x22],
                         theme.scrollbar.gray_mid,
                         theme.scrollbar.gray_light)?;
    }
    let first = if !active
    {
        2
    }
    else if state.highlighted == Part::Up
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
    else if state.highlighted == Part::Down
    {
        5
    }
    else
    {
        1
    };
    sprite(canvas, g.first, SPRITES[base + first], thickness, &colors)?;
    sprite(canvas, g.last, SPRITES[base + last], thickness, &colors)?;
    if active
       && (if g.horizontal { g.thumb.right - g.thumb.left } else { g.thumb.bottom - g.thumb.top })
          == thickness
    {
        sprite(canvas,
               g.thumb,
               SPRITES[base + if state.highlighted == Part::Thumb { 7 } else { 6 }],
               thickness,
               &colors)?;
    }
    Ok(())
}
fn sprite(canvas: &mut SurfacePort<'_>, bounds: Rect, data: &[u8], size: i16, colors: &[u32; 11])
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
                              colors[(color - b'A') as usize])?;
            x = end;
        }
    }
    Ok(())
}
/// # Safety
#[mac_definition::cdef_main]
fn main<'a>(message: Message<'a, Scrollbar>, services: Services<'a>) -> Result<Reply>
{
    match message
    {
        Message::Reflow { request,
                          creation,
                          .. } =>
        {
            if services.reflow() != mac_definition::Reflow::Yes
            {
                return Err(Error::NotSupported);
            }
            return mac_definition::leaf_reflow(request, creation, mac_definition::reflow::Size::try_from(if creation.width() > creation.height() { mac_abi::Dimensions { width: 30, height: 15 } } else { mac_abi::Dimensions { width: 15, height: 30 } }).map_err(|_| Error::BadAbi)?);
        }
        Message::Draw { state,
                        mut port } =>
        {
            if services.reflow() == mac_definition::Reflow::Yes && geometry(&state).is_err()
            {
                let theme = services.theme()?;
                port.paint_rect(state.bounds, theme.scrollbar.face_light)?;
                port.frame_rect(state.bounds, theme.scrollbar.ink)?;
            }
            else
            {
                draw(&mut port, &state, services.theme()?)?;
            }
        }
        Message::HitTest { state,
                           point } => return Ok(Reply::Hit(hit(&state, point))),
        Message::Track { state,
                         anchor,
                         point } =>
        {
            return Ok(Reply::Value(track_value(&state, anchor, point)? as u16));
        }
        Message::Validate(state) =>
        {
            if services.reflow() != mac_definition::Reflow::Yes
            {
                geometry(&state)?;
            }
            else if state.maximum < 0 || state.value < 0 || state.value > state.maximum
            {
                return Err(Error::BadArgument);
            }
        }
        Message::Initialize(_) | Message::Dispose(_) => (),
        Message::CalculateRegions(_) | Message::ChangeBounds { .. } =>
        {
            return Err(Error::NotSupported);
        }
    }
    Ok(Reply::Done)
}
// BEGIN GENERATED BOUNDED SCROLLBAR ASSETS
// Generated by scripts/extract_scrollbar_assets.py from original System7.6.1.
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

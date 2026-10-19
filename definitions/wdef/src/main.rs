//! Native WDEF 0 dispatch. The object layout is our RV64 ABI, not a 68k WindowRecord.
//!
//! Messages and hit results follow the classic WDEF protocol. Geometry is based
//! on installed System 7.6.1 WDEF 0. Chicago 12 and the measured reference-device
//! palette reproduce all retained original-Mac color reference captures exactly.
#![cfg_attr(target_arch = "riscv64", no_std)]
#![cfg_attr(target_arch = "riscv64", no_main)]
pub use mac_abi::definitions::*;
use mac_abi::{BAD_ABI, BAD_ARGUMENT, DefinitionCall, NOT_SUPPORTED};
use toolbox_quickdraw::{Point, Rect, SurfacePort as Canvas, WHITE, font::CHICAGO_12};
const BLACK: u32 = 0;
pub const TITLE_HEIGHT: i16 = 19;

fn calculate(state: &mut WindowDefinitionState, variation: i16)
{
    let r = state.content;
    let style = variation & 3;
    state.zoom_box = Rect::new(r.right - 19, r.top - 14, r.right - 8, r.top - 3);
    state.structure = match style
    {
        0 => Rect::new(r.left - 1, r.top - TITLE_HEIGHT, r.right + 2, r.bottom + 2),
        1 =>
        {
            Rect::new(r.left - 8,
                      r.top - if variation == 5 { TITLE_HEIGHT + 4 } else { 8 },
                      r.right + 8,
                      r.bottom + 8)
        }
        2 => Rect::new(r.left - 1, r.top - 1, r.right + 1, r.bottom + 1),
        _ => Rect::new(r.left - 1, r.top - 1, r.right + 3, r.bottom + 3)
    };
    state.structure_piece_count = if style == 0 || style == 3 { 2 } else { 1 };
    state.structure_pieces = [state.structure, Rect::ZERO];
    if style == 0 || style == 3
    {
        let offset = if style == 0 { 1 } else { 2 };
        let mut primary = state.structure;
        primary.right -= offset;
        primary.bottom -= offset;
        let mut shadow = primary;
        shadow.offset(offset, offset);
        state.structure_pieces = [primary, shadow];
    }
    state.close_box = if style == 0
    {
        let shift = if variation == 5 { 4 } else { 0 };
        Rect::new(r.left + 8, r.top - 14 - shift, r.left + 19, r.top - 3 - shift)
    }
    else
    {
        Rect::ZERO
    };
    state.grow_box = if style == 0 && variation & 4 == 0
    {
        Rect::new(r.right - 14, r.bottom - 14, r.right, r.bottom)
    }
    else
    {
        Rect::ZERO
    };
}

pub fn zoom_box(state: &WindowDefinitionState) -> Rect
{
    state.zoom_box
}

fn paint_around_content(canvas: &mut Canvas, bounds: Rect, content: Rect, color: u32)
                        -> mac_traps::Result<()>
{
    let intersect = Rect::new(bounds.left.max(content.left),
                              bounds.top.max(content.top),
                              bounds.right.min(content.right),
                              bounds.bottom.min(content.bottom));
    if intersect.is_empty()
    {
        canvas.paint_rect(bounds, color)?;
        return Ok(());
    }
    for strip in [Rect::new(bounds.left, bounds.top, bounds.right, intersect.top),
                  Rect::new(bounds.left, intersect.bottom, bounds.right, bounds.bottom),
                  Rect::new(bounds.left, intersect.top, intersect.left, intersect.bottom),
                  Rect::new(intersect.right, intersect.top, bounds.right, intersect.bottom)]
    {
        canvas.paint_rect(strip, color)?;
    }

    Ok(())
}

// Display RGB values measured on the pinned System 7.6.1 indexed-color device.
// Logical wctb RGB16 interpolation and the device mapping are retained separately
// in provenance/system7-wdef-color-assets.json and the original runtime captures.
const TITLE_FILL: u32 = 0xf3f3f3;
const STRIPE: u32 = 0x969696;
const HIGHLIGHT: u32 = 0xdadaff;
const SHADOW: u32 = 0xb3b3da;
const BUTTON_EDGE: u32 = 0x545487;
const BUTTON_FILL: u32 = 0xc0c0c0;
const GROW_LIGHT: u32 = 0xcdcdcd;
const INACTIVE_FRAME: u32 = 0x777777;
const INACTIVE_TEXT: u32 = 0xa5a5a5;
const MODAL_SHADOW: u32 = 0x8787b3;

const CLOSE_IMAGE: [&[u8; 11]; 11] = [b"EEEEEEEEEEE", b"EHHHHHHHHHH", b"EHfffffffEH",
                                      b"EHfffffffEH", b"EHfffffffEH", b"EHfffffffEH",
                                      b"EHfffffffEH", b"EHfffffffEH", b"EHfffffffEH",
                                      b"EHEEEEEEEEH", b"EHHHHHHHHHH"];
const ZOOM_IMAGE: [&[u8; 11]; 11] = [b"EEEEEEEEEEE", b"EHHHHHHHHHH", b"EHffffEffEH",
                                     b"EHffffEffEH", b"EHffffEffEH", b"EHffffEffEH",
                                     b"EHEEEEEffEH", b"EHfffffffEH", b"EHfffffffEH",
                                     b"EHEEEEEEEEH", b"EHHHHHHHHHH"];
const PRESSED_IMAGE: [&[u8; 11]; 11] = [b"###########", b"#LLLL#LLLL#", b"#L#LL#LL#L#",
                                        b"#LL#L#L#LL#", b"#LLLLLLLLL#", b"####LLL####",
                                        b"#LLLLLLLLL#", b"#LL#L#L#LL#", b"#L#LL#LL#L#",
                                        b"#LLLL#LLLL#", b"###########"];
const GROW_IMAGE: [&[u8; 15]; 15] = [b"###############", b"#..............", b"#..............",
                                     b"#..EEEEEE......", b"#..EHHHHHEEEE..", b"#..EHgggEHHHH..",
                                     b"#..EHgggEfffE..", b"#..EHgggEfffE..", b"#..EHEEEEfffE..",
                                     b"#...EHffffffE..", b"#...EHffffffE..", b"#...EHffffffE..",
                                     b"#...EHEEEEEEE..", b"#..............", b"#.............."];

fn image<const W: usize, const H: usize>(canvas: &mut Canvas, x: i16, y: i16, rows: [&[u8; W]; H])
                                         -> mac_traps::Result<()>
{
    for (v, row) in rows.iter().enumerate()
    {
        for (h, code) in row.iter().enumerate()
        {
            let color = match code
            {
                b'#' => BLACK,
                b'.' => TITLE_FILL,
                b'E' => BUTTON_EDGE,
                b'H' => HIGHLIGHT,
                b'L' => SHADOW,
                b'f' => BUTTON_FILL,
                b'g' => GROW_LIGHT,
                _ => continue
            };
            let left = x + h as i16;
            let top = y + v as i16;
            canvas.paint_rect(Rect::new(left, top, left + 1, top + 1), color)?;
        }
    }

    Ok(())
}

fn draw(state: &WindowDefinitionState, variation: i16, canvas: &mut Canvas)
        -> mac_traps::Result<()>
{
    let r = state.content;
    let style = variation & 3;
    let active = state.active != 0;
    let edge = if active { BLACK } else { INACTIVE_FRAME };
    let mut frame = state.structure;
    if style == 0 || style == 3
    {
        let offset = if style == 0 { 1 } else { 2 };
        frame.right -= offset;
        frame.bottom -= offset;
        let mut shadow = frame;
        shadow.offset(offset, offset);
        paint_around_content(canvas, shadow, r, BLACK)?;
    }
    paint_around_content(canvas, frame, r, WHITE)?;
    canvas.frame_rect(frame, edge)?;
    if style == 1
    {
        let mut modal = frame;
        if variation == 5
        {
            modal.top += TITLE_HEIGHT - 4;
        }
        // The modal frame has a bevel and a black inner boundary four pixels
        // inside its outer edge; the other three pixels are white padding.
        for (inset, upper, lower) in if active
        {
            [(1, HIGHLIGHT, MODAL_SHADOW),
             (2, GROW_LIGHT, GROW_LIGHT),
             (3, MODAL_SHADOW, HIGHLIGHT),
             (4, BLACK, BLACK)]
        }
        else
        {
            [(1, WHITE, WHITE), (2, WHITE, WHITE), (3, STRIPE, STRIPE), (4, STRIPE, STRIPE)]
        }
        {
            let mut ring = modal;
            ring.inset(inset, inset);
            canvas.paint_rect(Rect::new(ring.left, ring.top, ring.right - 1, ring.top + 1), upper)?;
            canvas.paint_rect(Rect::new(ring.left, ring.top, ring.left + 1, ring.bottom - 1),
                              upper)?;
            canvas.paint_rect(Rect::new(ring.right - 1, ring.top, ring.right, ring.bottom), lower)?;
            canvas.paint_rect(Rect::new(ring.left, ring.bottom - 1, ring.right, ring.bottom),
                              lower)?;
            if active && inset == 3
            {
                canvas.paint_rect(Rect::new(ring.right - 1, ring.top, ring.right, ring.top + 1),
                                  upper)?;
                canvas.paint_rect(Rect::new(ring.left,
                                            ring.bottom - 1,
                                            ring.left + 1,
                                            ring.bottom),
                                  upper)?;
            }
        }
    }
    if style != 0 && variation != 5
    {
        return Ok(());
    }
    let mut title = Rect::new(r.left, r.top - TITLE_HEIGHT + 1, r.right, r.top - 1);
    if variation == 5
    {
        title = Rect::new(r.left - 7, r.top - TITLE_HEIGHT - 3, r.right + 7, r.top - 5);
    }
    let fill = if active { TITLE_FILL } else { WHITE };
    canvas.paint_rect(title, fill)?;
    if active
    {
        canvas.paint_rect(Rect::new(title.left, title.top, title.right - 1, title.top + 1),
                          HIGHLIGHT)?;
        canvas.paint_rect(Rect::new(title.left, title.top, title.left + 1, title.bottom - 1),
                          HIGHLIGHT)?;
        canvas.paint_rect(Rect::new(title.right - 1, title.top, title.right, title.bottom),
                          SHADOW)?;
        canvas.paint_rect(Rect::new(title.left, title.bottom - 1, title.right, title.bottom),
                          SHADOW)?;
    }
    let length = (state.title_length as usize).min(state.title.len());
    let text = core::str::from_utf8(&state.title[..length]).unwrap_or("WINDOW");
    let text_width = canvas.text_width(CHICAGO_12, text)?;
    let offset = ((title.right - title.left - text_width) / 2).max(32);
    let x = title.left + offset;
    let end = if variation & 8 != 0 { title.right - 32 } else { title.right - offset };
    if active
    {
        let margin = if variation == 5 { 2 } else { 1 };
        let stripes =
            Rect::new(title.left + margin, title.top + 3, title.right - margin, title.bottom - 3);
        let gap_left = x - 6;
        let mut gap_right = (x + text_width).min(end) + 6;
        if variation & 8 != 0
        {
            gap_right = gap_right.min(stripes.right - 26);
        }
        for y in (stripes.top..stripes.bottom).step_by(2)
        {
            canvas.paint_rect(Rect::new(stripes.left, y, gap_left.min(stripes.right), y + 1),
                              STRIPE)?;
            canvas.paint_rect(Rect::new(gap_right.max(stripes.left), y, stripes.right, y + 1),
                              STRIPE)?;
        }
    }
    let baseline = title.top + canvas.font_metrics(CHICAGO_12)?.ascent + 1;
    canvas.draw_text(CHICAGO_12,
                     x,
                     baseline,
                     text,
                     (end - x).max(0),
                     if active { BLACK } else { INACTIVE_TEXT })?;
    if active && style == 0
    {
        let x = r.left + 8;
        let y = r.top - TITLE_HEIGHT + 4;
        canvas.paint_rect(Rect::new(x - 1, y - 1, x + 12, y + 12), fill)?;
        image(canvas, x, y, if state.close_pressed != 0 { PRESSED_IMAGE } else { CLOSE_IMAGE })?;
        if variation & 8 != 0
        {
            let x = r.right - 19;
            canvas.paint_rect(Rect::new(x - 1, y - 1, x + 12, y + 12), fill)?;
            image(canvas, x, y, if state.zoom_pressed != 0 { PRESSED_IMAGE } else { ZOOM_IMAGE })?;
        }
    }
    if style == 0
    {
        canvas.paint_rect(Rect::new(r.left - 1, r.top - 1, r.right + 1, r.top), edge)?;
    }

    Ok(())
}

fn grow_icon(state: &WindowDefinitionState, canvas: &mut Canvas) -> mac_traps::Result<()>
{
    let r = state.content;
    canvas.paint_rect(Rect::new(r.right - 15, r.top, r.right - 14, r.bottom), BLACK)?;
    canvas.paint_rect(Rect::new(r.left, r.bottom - 15, r.right, r.bottom - 14), BLACK)?;
    image(canvas, r.right - 15, r.bottom - 15, GROW_IMAGE)?;

    Ok(())
}

/// # Safety
/// The caller owns a valid DefinitionCall, WindowDefinitionState and, for draw
/// messages, an exclusive Surface backed by live writable pixel memory.
pub unsafe extern "C" fn standard_window_definition(pointer: *mut DefinitionCall) -> i64
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
    if call.kind != WDEF
       || call.id != 0
       || !matches!(call.variation, 0 | 1 | 2 | 3 | 4 | 5 | 8 | 12)
    {
        return NOT_SUPPORTED as i64;
    }
    let Some(state) = (unsafe { (call.object as *mut WindowDefinitionState).as_mut() })
    else
    {
        return BAD_ARGUMENT as i64;
    };
    match call.message
    {
        CALCULATE | NEW =>
        {
            calculate(state, call.variation);
            0
        }
        DISPOSE => 0,
        HIT =>
        {
            let bits = call.param as u32;
            let point = Point { v: (bits >> 16) as i16,
                                h: bits as i16 };
            if !state.structure.contains(point) ||
                (state.structure_piece_count!=0 && !state.structure_pieces[..(state.structure_piece_count as usize).min(2)].iter().any(|rect|rect.contains(point)))
            {
                0
            }
            else if state.active != 0 && state.close_box.contains(point)
            {
                4
            }
            else if state.active != 0
                      && call.variation & 8 != 0
                      && zoom_box(state).contains(point)
            {
                if state.zoomed != 0 { 5 } else { 6 }
            }
            else if call.variation & 3 != 0 && call.variation != 5
            {
                1
            }
            else if point.v < state.content.top - if call.variation == 5 { 4 } else { 0 }
            {
                2
            }
            else if state.active != 0 && state.grow_box.contains(point)
            {
                3
            }
            else if state.content.contains(point)
            {
                1
            }
            else
            {
                0
            }
        }
        DRAW | DRAW_GROW_ICON | GROW =>
        {
            let Some(surface) = (unsafe { call.surface.as_mut() })
            else
            {
                return BAD_ARGUMENT as i64;
            };
            let connection = unsafe { mac_traps::SurfaceConnection::new(surface, call.gate) };
            let mut canvas = Canvas::new(connection);
            let result = match call.message
            {
                DRAW => draw(state, call.variation, &mut canvas),
                GROW =>
                {
                    let Some(rect) = (unsafe { (call.param as usize as *const Rect).as_ref() })
                    else
                    {
                        return BAD_ARGUMENT as i64;
                    };
                    grow_outline(rect, &mut canvas)
                }
                _ => grow_icon(state, &mut canvas)
            };
            result.map_or_else(|e| e.0 as i64, |_| 0)
        }
        _ => NOT_SUPPORTED as i64
    }
}


fn grow_outline(rect: &Rect, canvas: &mut Canvas) -> mac_traps::Result<()>
{
    // Original WDEF0 at0x806 uses the caller's XOR pen, FrameRect, then
    // inclusive LineTo endpoints on the expanded rectangle. Keep intersections
    // reversible: painting a union loses the cancellation at crossing lines.
    let mut outer = *rect;
    outer.inset(-1, -1);
    outer.top -= TITLE_HEIGHT - 1;
    canvas.invert_rect(Rect::new(outer.left, outer.top, outer.right, outer.top + 1))?;
    canvas.invert_rect(Rect::new(outer.left, outer.bottom - 1, outer.right, outer.bottom))?;
    canvas.invert_rect(Rect::new(outer.left, outer.top + 1, outer.left + 1, outer.bottom - 1))?;
    canvas.invert_rect(Rect::new(outer.right - 1, outer.top + 1, outer.right, outer.bottom - 1))?;
    canvas.invert_rect(Rect::new(outer.left,
                                 outer.bottom - 16,
                                 outer.right + 1,
                                 outer.bottom - 15))?;
    outer.top += TITLE_HEIGHT - 1;
    canvas.invert_rect(Rect::new(outer.left, outer.top, outer.right + 1, outer.top + 1))?;
    canvas.invert_rect(Rect::new(outer.right - 16, outer.top, outer.right - 15, outer.bottom + 1))?;
    Ok(())
}


/// # Safety
/// System supplies a live definition call and exclusive borrowed state/surface.
#[cfg(target_arch = "riscv64")]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(call: *mut mac_abi::DefinitionCall) -> i64
{
    unsafe { standard_window_definition(call) }
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

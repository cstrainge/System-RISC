//! Standard native menu definition. Drawing, sizing and hit geometry stay in
//! this independently loaded resource; System owns state and event routing.
#![cfg_attr(target_arch = "riscv64", no_std)]
#![cfg_attr(target_arch = "riscv64", no_main)]
use mac_abi::{menus::*, *};
use toolbox_quickdraw::SurfacePort;
fn text(bytes: &[u8], length: u32) -> Result<&str, Status>
{
    core::str::from_utf8(bytes.get(..length as usize).ok_or(BAD_ARGUMENT)?).map_err(|_| {
                                                                               BAD_ARGUMENT
                                                                           })
}
fn paint(state: &MenuDefinitionState, popup: bool, canvas: &mut SurfacePort)
         -> mac_traps::Result<()>
{
    if !popup
    {
        if state.first != 0
        {
            canvas.paint_rect(Rect::new(0, 0, 640, 20), 0xffffff)?;
            canvas.paint_rect(Rect::new(0, 19, 640, 20), 0)?;
        }
        if state.title_length == 0
        {
            return Ok(());
        }
        let selected = state.bar_selected != 0;
        if selected
        {
            // Original MBDF0 leaves the top row and bottom bar rule intact.
            // Title selection extends into the adjacent inter-title spacing.
            canvas.paint_rect(Rect::new(state.title_bounds.left - 1,
                                        1,
                                        state.title_bounds.right + 4,
                                        19),
                              0)?;
        }
        // Original MBDF0 recognizes the one-byte Apple title and plots the
        // System small-icon family. Its rainbow colors survive title selection.
        if state.title_length == 1 && state.title[0] == 0x14
        {
            let left = state.title_bounds.left
                       + (state.title_bounds.right - state.title_bounds.left - 16) / 2
                       + 1;
            return canvas.icon_resource(0,
                                        -16386,
                                        Rect::new(left, 1, left + 16, 17),
                                        u16::from(state.enabled == 0));
        }
        canvas.draw_text(0,
                         state.title_bounds.left + 8,
                         14,
                         text(&state.title, state.title_length).map_err(mac_traps::Error)?,
                         state.title_bounds.right - state.title_bounds.left - 13,
                         if selected
                         {
                             0xffffff
                         }
                         else if state.enabled != 0
                         {
                             0
                         }
                         else
                         {
                             0xa5a5a5
                         })?;
        return Ok(());
    }
    let r = state.popup_bounds;
    let outer = Rect::new(r.left - 1, r.top - 1, r.right + 1, r.bottom + 1);
    canvas.paint_rect(Rect::new(outer.left + 3, outer.top + 3, outer.right + 1, outer.bottom + 1),
                      0)?;
    canvas.paint_rect(outer, 0xffffff)?;
    canvas.frame_rect(outer, 0)?;
    for (index, item) in state.items[..state.item_count as usize].iter().enumerate()
    {
        let r = item.bounds;
        if item.separator != 0
        {
            canvas.paint_rect(Rect::new(r.left, r.top + 8, r.right, r.top + 9), 0xa5a5a5)?;
            continue;
        }
        let selected = state.highlighted == index as i16 + 1;
        let fg = if selected
        {
            0xffffff
        }
        else if item.enabled != 0
        {
            0
        }
        else
        {
            0xa5a5a5
        };
        if selected
        {
            canvas.paint_rect(r, 0)?;
        }
        if item.checked != 0
        {
            canvas.draw_text(0, r.left + 2, r.top + 12, "\u{12}", 14, fg)?;
        }
        let label = text(&item.text, item.text_length).map_err(mac_traps::Error)?;
        if item.style == 0
        {
            canvas.draw_text(0, r.left + 14, r.top + 12, label, r.right - r.left - 14, fg)?;
        }
        else
        {
            canvas.styled_text(u16::from(item.style), r.left + 14, r.top + 12, label, fg)?;
        }
        if item.checked == 0 && item.mark != 0
        {
            if item.mark >= 128
            {
                return Err(mac_traps::Error(NOT_SUPPORTED));
            }
            let bytes = [item.mark];
            let mark = core::str::from_utf8(&bytes).map_err(|_| mac_traps::Error(BAD_ARGUMENT))?;
            canvas.draw_text(0, r.left + 2, r.top + 12, mark, 14, fg)?;
        }
        if item.key != 0
        {
            canvas.draw_text(0, r.right - 24, r.top + 12, "\u{11}", 14, fg)?;
            let key = [item.key.to_ascii_uppercase()];
            let key = core::str::from_utf8(&key).map_err(|_| mac_traps::Error(NOT_SUPPORTED))?;
            let command_width = canvas.text_width(0, "\u{11}")?;
            canvas.draw_text(0, r.right - 24 + command_width, r.top + 12, key, 13, fg)?;
        }
    }
    Ok(())
}
fn measure(state: &mut MenuDefinitionState, canvas: &mut SurfacePort) -> mac_traps::Result<()>
{
    let title = text(&state.title, state.title_length).map_err(mac_traps::Error)?;
    state.title_bounds.right = state.title_bounds.left + canvas.text_width(0, title)? + 13;
    state.title_bounds.top = 0;
    state.title_bounds.bottom = 20;
    let mut width = state.minimum_width.max(24);
    for item in &state.items[..state.item_count as usize]
    {
        let label = text(&item.text, item.text_length).map_err(mac_traps::Error)?;
        let label_width = if item.style == 0
        {
            canvas.text_width(0, label)?
        }
        else
        {
            canvas.styled_width(u16::from(item.style), label)?
        };
        // Original CalcMenuSize reserves 24 extra pixels for a shortcut.
        // Combined with the keyless margins this leaves 10 pixels between the
        // label (left+14) and the command column (right-24).
        width = width.max(label_width + 24 + if item.key != 0 { 24 } else { 0 });
    }
    let left = state.title_bounds.left.min(638 - width);
    let mut y = 20;
    for item in &mut state.items[..state.item_count as usize]
    {
        let h = if item.text[..item.text_length as usize].starts_with(&[0, 0]) { 18 } else { 16 };
        item.bounds = Rect::new(left, y, left + width, y + h);
        y += h;
    }
    state.popup_bounds = Rect::new(left, 20, left + width, y);
    Ok(())
}
/// # Safety
/// Caller owns the live native state and surface for this definition invocation.
pub unsafe extern "C" fn standard_menu_definition(pointer: *mut DefinitionCall) -> i64
{
    let Some(call) = (unsafe { pointer.as_mut() })
    else
    {
        return BAD_ARGUMENT as i64;
    };
    if !call.header.supports::<DefinitionCall>() || call.kind != MDEF || call.id != 0
    {
        return BAD_ABI as i64;
    }
    let Some(state) = (unsafe { call.object.cast::<MenuDefinitionState>().as_mut() })
    else
    {
        return BAD_ARGUMENT as i64;
    };
    if state.item_count as usize > MAX_ITEMS
    {
        return BAD_ARGUMENT as i64;
    }
    if call.message == HIT_ITEM
    {
        let p = Point { v: (call.param as u32 >> 16) as i16,
                        h: call.param as i16 };
        return state.items[..state.item_count as usize].iter()
                                                       .position(|i| {
                                                           i.enabled != 0
                                                           && i.separator == 0
                                                           && i.bounds.contains(p)
                                                       })
                                                       .map_or(0, |i| i as i64 + 1);
    }
    let Some(surface) = (unsafe { call.surface.as_mut() })
    else
    {
        return BAD_ARGUMENT as i64;
    };
    let connection = unsafe { mac_traps::SurfaceConnection::new(surface, call.gate) };
    let mut canvas = SurfacePort::new(connection);
    let result = match call.message
    {
        MEASURE => measure(state, &mut canvas),
        DRAW_BAR => paint(state, false, &mut canvas),
        DRAW_POPUP => paint(state, true, &mut canvas),
        _ => Err(mac_traps::Error(NOT_SUPPORTED))
    };
    result.map_or_else(|e| e.0 as i64, |_| 0)
}
#[cfg(target_arch = "riscv64")]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
/// # Safety
/// The loader supplies a live exclusive DefinitionCall and its borrowed state.
pub unsafe extern "C" fn _start(call: *mut DefinitionCall) -> i64
{
    unsafe { standard_menu_definition(call) }
}
#[cfg(target_arch = "riscv64")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> !
{
    loop
    {
        core::hint::spin_loop()
    }
}

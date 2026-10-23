//! Independently relocatable static-text CDEF. UTF-8, font metrics, alignment
//! and clipping belong to the definition; resident controls own its lifetime.
#![cfg_attr(target_arch = "riscv64", no_std)]
#![cfg_attr(target_arch = "riscv64", no_main)]
use mac_abi::{controls::*,
              definitions::{CDEF, ControlDefinitionState},
              *};
use toolbox_quickdraw::SurfacePort;
fn text(state: &ControlDefinitionState) -> Result<&str, Status>
{
    if state.bounds.is_empty()
       || state.flags != 0
       || state.highlighted != 0
       || state.minimum != 0
       || state.maximum != 0
       || !(-1..=1).contains(&state.value)
    {
        return Err(BAD_ARGUMENT);
    }
    core::str::from_utf8(state.title.get(..state.title_length as usize).ok_or(BAD_ARGUMENT)?)
        .map_err(|_| BAD_ARGUMENT)
}
fn draw(canvas: &mut SurfacePort<'_>, state: &ControlDefinitionState) -> mac_traps::Result<()>
{
    let title = text(state).map_err(mac_traps::Error::from_status)?;
    let r = state.bounds;
    canvas.paint_rect(r, 0xffffff)?;
    let metrics = canvas.selected_font_metrics(state.font)?;
    let baseline = i32::from(r.top) + i32::from(metrics.ascent);
    let line_height = (i32::from(metrics.ascent)
                       + i32::from(metrics.descent)
                       + i32::from(metrics.leading)).max(1);
    for (row, line) in title.split('\n')
                            .flat_map(|line| line.strip_suffix('\r').unwrap_or(line).split('\r'))
                            .enumerate()
    {
        let y = baseline + row as i32 * line_height;
        if y - i32::from(metrics.ascent) >= i32::from(r.bottom)
        {
            break;
        }
        if line.is_empty()
        {
            continue;
        }
        let width = i32::from(canvas.font_width(state.font, line)?);
        let available = i32::from(r.right) - i32::from(r.left);
        let x = i32::from(r.left)
                + match state.value
                {
                    -1 => (available - width).max(0),
                    1 => ((available - width) / 2).max(0),
                    _ => 0
                };
        canvas.font_text_clipped(state.font,
                                 Point { h: x as i16,
                                         v: y.min(i16::MAX as i32) as i16 },
                                 line,
                                 (i32::from(r.right) - x).min(i16::MAX as i32) as i16,
                                 if state.enabled != 0 { 0 } else { 0xa5a5a5 })?;
    }
    Ok(())
}
/// # Safety
/// System supplies live, aligned, exclusively borrowed family state and surface.
pub unsafe extern "C" fn static_text_definition(pointer: *mut DefinitionCall) -> i64
{
    let result = (|| -> Result<i64, Status> {
        let call = unsafe { pointer.as_mut() }.ok_or(BAD_ARGUMENT)?;
        if !call.header.supports::<DefinitionCall>()
        {
            return Err(BAD_ABI);
        }
        if call.kind != CDEF
           || call.id != CDEF_STATIC_TEXT
           || call.variation != 0
           || call.reserved != 0
        {
            return Err(NOT_SUPPORTED);
        }
        let state =
            unsafe { call.object.cast::<ControlDefinitionState>().as_mut() }.ok_or(BAD_ARGUMENT)?;
        text(state)?;
        if call.message == VALIDATE_DEFINITION
        {
            // Font lookup is immutable and independent of the desktop borrow.
            // Reject an unavailable face before publishing the managed object.
            let services = SystemServices { header:       Header::for_type::<SystemServices>(),
                                            trap:         call.gate,
                                            capabilities: 0 };
            mac_traps::Client::new(&services).map_err(|e| e.status())?
                                             .font(quickdraw::FONT_METRICS, state.font, "", 0)
                                             .map_err(|e| e.status())?;
            return Ok(0);
        }
        match call.message
        {
            0 =>
            {
                let surface = unsafe { call.surface.as_mut() }.ok_or(BAD_ARGUMENT)?;
                let connection = unsafe { mac_traps::SurfaceConnection::new(surface, call.gate) };
                draw(&mut SurfacePort::new(connection), state).map_err(|e| e.status())?;
            }
            1..=4 | VALIDATE_DEFINITION => (),
            _ => return Err(NOT_SUPPORTED)
        }
        Ok(0)
    })();
    result.unwrap_or_else(|e| i64::from(e))
}
#[cfg(target_arch = "riscv64")]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(call: *mut DefinitionCall) -> i64
{
    unsafe { static_text_definition(call) }
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

//! Independently relocatable progress-bar CDEF. Geometry and delta damage live
//! here, shared by Finder's copy window and the System startup panel.
#![cfg_attr(target_arch = "riscv64", no_std)]
#![cfg_attr(target_arch = "riscv64", no_main)]
use mac_abi::{controls::*,
              definitions::{CDEF, ControlDefinitionState},
              *};
use toolbox_quickdraw::SurfacePort;

// Provenance: original Finder7.6.1 CODE174:0x36e..0x736, reached from
// the fvew5250 class table and its explicit item8803 branch. See the source
// hashes and colour/pattern observations in provenance/finder-copy-window.json.
fn interior(state: &ControlDefinitionState) -> Result<Rect, Status>
{
    let r = state.bounds;
    if i32::from(r.right) - i32::from(r.left) < 3
       || i32::from(r.bottom) - i32::from(r.top) < 3
       || state.progress.reserved != 0
       || state.progress.determinate > 1
       || state.progress.completed > state.progress.total
       || state.progress.in_progress > state.progress.total - state.progress.completed
       || (state.progress.determinate == 0
           && (state.progress.total != 0
               || state.progress.completed != 0
               || state.progress.in_progress != 0))
       || state.reserved != 0
       || state.highlighted != 0
       || state.title_length != 0
    {
        return Err(BAD_ARGUMENT);
    }
    Ok(Rect::new(r.left + 1, r.top + 1, r.right - 1, r.bottom - 1))
}
fn edge(state: &ControlDefinitionState) -> Result<i16, Status>
{
    let r = interior(state)?;
    let width = i32::from(r.right) - i32::from(r.left);
    let p = state.progress;
    let position = |count: u64| {
        (i32::from(r.left)
         + if p.determinate == 0
         {
             0
         }
         else if p.total == 0
         {
             width
         }
         else
         {
             (width as u128 * u128::from(count) / u128::from(p.total)) as i32
         }) as i16
    };
    Ok(position(p.completed))
}
fn change(change: &mut ControlChange) -> Result<(), Status>
{
    let r = interior(&change.after)?;
    let old_settled = edge(&change.before)?;
    let settled = edge(&change.after)?;
    change.damage = [Rect::ZERO; 2];
    if change.before.bounds != change.after.bounds
    {
        change.damage[0] = change.after.bounds;
        return Ok(());
    }
    // The frame and blue track stay fixed. Only completed pixels change;
    // in-flight work is not reported as completed progress.
    if old_settled != settled
    {
        change.damage[0] =
            Rect::new(old_settled.min(settled), r.top, old_settled.max(settled), r.bottom);
    }
    Ok(())
}
fn draw(canvas: &mut SurfacePort<'_>, state: &ControlDefinitionState, monochrome: bool)
        -> mac_traps::Result<()>
{
    let inside = interior(state).map_err(mac_traps::Error::from_status)?;
    let settled = edge(state).map_err(mac_traps::Error::from_status)?;
    canvas.frame_rect(state.bounds, 0)?;
    canvas.paint_rect(Rect::new(inside.left, inside.top, settled, inside.bottom),
                      if monochrome { 0 } else { 0x666666 })?;
    canvas.paint_rect(Rect::new(settled, inside.top, inside.right, inside.bottom),
                      if monochrome { 0xffffff } else { 0xdadaff })?;
    Ok(())
}
/// # Safety
/// System supplies live, aligned, exclusively borrowed family state and surface.
pub unsafe extern "C" fn progress_bar_definition(pointer: *mut DefinitionCall) -> i64
{
    let result = (|| -> Result<i64, Status> {
        let call = unsafe { pointer.as_mut() }.ok_or(BAD_ARGUMENT)?;
        if !call.header.supports::<DefinitionCall>()
        {
            return Err(BAD_ABI);
        }
        if call.kind != CDEF
           || call.id != CDEF_PROGRESS_BAR
           || !matches!(call.variation, 0 | 1)
           || call.reserved != 0
        {
            return Err(NOT_SUPPORTED);
        }
        if call.message == CHANGE_BOUNDS_DEFINITION
        {
            let state =
                unsafe { call.object.cast::<ControlChange>().as_mut() }.ok_or(BAD_ARGUMENT)?;
            change(state)?;
            return Ok(0);
        }
        let state =
            unsafe { call.object.cast::<ControlDefinitionState>().as_mut() }.ok_or(BAD_ARGUMENT)?;
        interior(state)?;
        match call.message
        {
            0 =>
            {
                let surface = unsafe { call.surface.as_mut() }.ok_or(BAD_ARGUMENT)?;
                let connection = unsafe { mac_traps::SurfaceConnection::new(surface, call.gate) };
                draw(&mut SurfacePort::new(connection), state, call.variation == 1).map_err(|e| {
                                                                                       e.status()
                                                                                   })?;
            }
            1..=4 | VALIDATE_DEFINITION => (),
            _ => return Err(NOT_SUPPORTED)
        }
        // Passive: never returns a hit part or captures input.
        Ok(0)
    })();
    result.unwrap_or_else(|e| i64::from(e))
}
#[cfg(target_arch = "riscv64")]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(call: *mut DefinitionCall) -> i64
{
    unsafe { progress_bar_definition(call) }
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

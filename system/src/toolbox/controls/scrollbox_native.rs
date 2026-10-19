//! Narrow borrowed-surface boundary; the resident control itself is safe Rust.
use mac_abi::{scrollbox::*, *};
/// # Safety
/// Requests and optional exclusive surface storage borrow initialized live memory.
pub unsafe fn dispatch(pointer: *mut (), size: usize,
                       mut run: impl FnMut(&mut Call)
                             -> Result<Option<super::scrollbox::Drawing>, Status>)
                       -> i64
{
    if pointer.is_null()
       || size != size_of::<Call>()
       || !(pointer as usize).is_multiple_of(align_of::<Call>())
    {
        return BAD_ARGUMENT as i64;
    }
    let call = unsafe { &mut *pointer.cast::<Call>() };
    let surface = call.surface;
    if !surface.is_null()
       && (call.operation != DRAW || !(surface as usize).is_multiple_of(align_of::<Surface>()))
    {
        return BAD_ARGUMENT as i64;
    }
    match run(call)
    {
        Err(e) => e as i64,
        Ok(Some(drawing)) if !surface.is_null() =>
        {
            use crate::toolbox::quickdraw::SurfaceExt;
            let Some(mut canvas) = (unsafe { (&mut *surface).canvas() })
            else
            {
                return BAD_ARGUMENT as i64;
            };
            drawing.paint(&mut canvas);
            OK as i64
        }
        Ok(_) => OK as i64
    }
}

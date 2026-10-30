//! Narrow synchronous services for a resident definition invoked while scheduler
//! maintenance still names an interrupted module worker. No executor identity,
//! allocation owner, resource selection or application authority is borrowed.
use mac_abi::{quickdraw::*, traps::*, *};

/// The native ABI still requires live request pointees. This checks the request
/// layout before inspecting it; the ordinary dispatcher validates its contents.
pub(super) unsafe fn admit(invocation: &crate::context::DefinitionInvocations, number: u32,
                           pointer: *mut (), size: usize)
                           -> Result<(), Status>
{
    let surface = invocation.borrowed_surface().ok_or(WRONG_THREAD)?;
    let owns_surface = |candidate: *mut Surface| !surface.is_null() && candidate == surface;
    let allowed = match number
    {
        THEME | DEFINITION_PANIC => true,
        SURFACE =>
        {
            let call = unsafe { super::request::<SurfaceCall>(pointer, size) }.ok_or(BAD_ARGUMENT)?;
            owns_surface(call.surface)
        }
        QUICKDRAW =>
        {
            let call =
                unsafe { super::request::<GraphicsCall>(pointer, size) }.ok_or(BAD_ARGUMENT)?;
            call.target == Target::default() && owns_surface(call.surface)
        }
        COPY_BITS =>
        {
            let call = unsafe { super::request::<BitmapCall>(pointer, size) }.ok_or(BAD_ARGUMENT)?;
            call.target == Target::default() && owns_surface(call.surface)
        }
        PICTURE =>
        {
            let call = unsafe { super::request::<PictureCall>(pointer, size) }.ok_or(BAD_ARGUMENT)?;
            call.operation == PICTURE_BYTES
            && call.target == Target::default()
            && owns_surface(call.surface)
        }
        IMAGE =>
        {
            let call = unsafe { super::request::<mac_abi::images::ImageCall>(pointer, size) }
                .ok_or(BAD_ARGUMENT)?;
            call.operation == mac_abi::images::DRAW
            && call.target == Target::default()
            && owns_surface(call.surface)
        }
        FONT =>
        {
            let call = unsafe { super::request::<FontCall>(pointer, size) }.ok_or(BAD_ARGUMENT)?;
            call.target == Target::default()
            && match call.operation
            {
                FONT_METRICS | FONT_WIDTH | FONT_WIDTH_CHECKED =>
                {
                    call.surface.is_null() || owns_surface(call.surface)
                }
                FONT_DRAW => owns_surface(call.surface),
                _ => false
            }
        }
        ICON =>
        {
            // Only SurfacePort's resource-icon drawing request. File loading,
            // resource mutation and window/world drawing remain unavailable.
            let call = unsafe { super::request::<IconCall>(pointer, size) }.ok_or(BAD_ARGUMENT)?;
            call.operation == ICON_PLOT
            && call.target == Target::default()
            && owns_surface(call.surface)
        }
        _ => false
    };
    if allowed { Ok(()) } else { Err(WRONG_THREAD) }
}

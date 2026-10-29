//! Reuse the resource source as a host fixture; no second WDEF implementation.
#[path = "../../definitions/wdef/src/main.rs"]
mod wdef;
use mac_abi::{definitions::*, *};
use mac_system::toolbox::quickdraw::{Canvas, SurfaceExt};

unsafe extern "C" fn unavailable(number: u32, _: *mut (), _: usize) -> i64
{
    assert_eq!(number, traps::SURFACE); // The resource asks only for drawing primitives.
    NO_MEMORY as i64
}

#[test]
fn resource_owns_geometry_and_propagates_primitive_service_failures()
{
    let mut state = WindowDefinitionState::new(Rect::new(20, 60, 120, 140), "System RISC");
    let mut pixels = [0xffffff; 160 * 160];
    let mut canvas = Canvas::new(&mut pixels, 160, 160);
    let mut surface = Surface::from_canvas(&mut canvas);
    let mut call = DefinitionCall { reflow:    0,
                                    header:    Header::for_type::<DefinitionCall>(),
                                    kind:      WDEF,
                                    id:        0,
                                    variation: 8,
                                    message:   CALCULATE,
                                    reserved:  0,
                                    param:     0,
                                    object:    (&mut state as *mut WindowDefinitionState).cast(),
                                    surface:   &mut surface,
                                    gate:      unavailable };
    assert_eq!(unsafe { wdef::standard_window_definition(&mut call) }, OK as i64);
    assert_eq!(state.structure, Rect::new(19, 41, 122, 142));
    assert_eq!(state.zoom_box, Rect::new(101, 46, 112, 57));
    call.message = DRAW;
    assert_eq!(unsafe { wdef::standard_window_definition(&mut call) }, NO_MEMORY as i64);
    assert!(pixels.iter().all(|&pixel| pixel == 0xffffff));
}

//! Host reference renderer: output variation active close_pressed zoom_pressed
//! [left top right bottom title]. Defaults match the original-Mac WDEFProbe.
#[path = "../../definitions/wdef/src/main.rs"]
mod wdef;
use std::{env,
          fs::File,
          io::{BufWriter, Write}};

use mac_abi::{DefinitionCall, Header, Surface,
              definitions::{self as definition, WindowDefinitionState}};
use mac_system::toolbox::quickdraw::{Canvas, Rect, SurfaceExt};

fn main() -> Result<(), Box<dyn std::error::Error>>
{
    let args: Vec<String> = env::args().skip(1).collect();
    let output = args.first().ok_or("usage: wdef_render output.ppm variation active \
                                      close_pressed zoom_pressed [left top right bottom title]")?;
    let argument = |index: usize, default: i16| -> Result<i16, Box<dyn std::error::Error>> {
        Ok(args.get(index).map_or(Ok(default), |value| value.parse())?)
    };
    let variation = argument(1, 0)?;
    let bounds =
        Rect::new(argument(5, 100)?, argument(6, 100)?, argument(7, 420)?, argument(8, 300)?);
    let mut state = WindowDefinitionState::new(bounds, args.get(9).map_or("TEST", String::as_str));
    state.active = argument(2, 1)? as u32;
    state.close_pressed = argument(3, 0)? as u32;
    state.zoom_pressed = argument(4, 0)? as u32;
    let mut pixels = vec![0x808080; 640 * 480];
    let mut canvas = Canvas::new(&mut pixels, 640, 480);
    canvas.paint_rect(bounds, 0xffffff);
    let mut surface = Surface::from_canvas(&mut canvas);
    let mut call = DefinitionCall { header: Header::for_type::<DefinitionCall>(),
                                    kind: definition::WDEF,
                                    id: 0,
                                    variation,
                                    message: definition::CALCULATE,
                                    reserved: 0,
                                    param: 0,
                                    object: (&mut state as *mut WindowDefinitionState).cast(),
                                    surface: &mut surface,
                                    gate: mac_system::toolbox::quickdraw::surface_gate };
    for message in [definition::CALCULATE, definition::DRAW, definition::DRAW_GROW_ICON]
    {
        call.message = message;
        let status = unsafe { wdef::standard_window_definition(&mut call) };
        if status != 0
        {
            return Err(format!("WDEF message {message} returned {status}").into());
        }
    }
    let mut output = BufWriter::new(File::create(output)?);
    write!(output, "P6\n640 480\n255\n")?;
    for pixel in pixels
    {
        output.write_all(&[(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8])?;
    }
    output.flush()?;
    Ok(())
}

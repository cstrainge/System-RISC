//! Render exactly the original CDEFProbe's four controls, via the resource entry.
#[path = "../../definitions/scrollbar/src/main.rs"]
mod scrollbar;
use std::{env,
          fs::File,
          io::{BufWriter, Write}};

use mac_abi::{controls::ScrollbarState, definitions::CDEF, *};
use mac_system::toolbox::quickdraw::{Canvas, SurfaceExt};
fn main() -> Result<(), Box<dyn std::error::Error>>
{
    let args: Vec<_> = env::args().skip(1).collect();
    if args.first().is_some_and(|s| s == "--hits")
    {
        return hit_probe(args.get(1).ok_or("missing hit output path")?);
    }
    let output = args.first().ok_or("usage: scrollbar_render output.ppm page")?;
    let page = args.get(1).map_or(Ok(0), |v| v.parse::<u8>())?;
    if page > 7
    {
        return Err("page must be0..7".into());
    }
    let mut pixels = vec![0xffffff; 640 * 480];
    let mut canvas = Canvas::new(&mut pixels, 640, 480);
    let mut surface = Surface::from_canvas(&mut canvas);
    for bounds in [Rect::new(380, 100, 395, 280),
                   Rect::new(400, 100, 416, 280),
                   Rect::new(100, 280, 380, 295),
                   Rect::new(100, 305, 380, 321)]
    {
        let mut state = ScrollbarState { bounds,
                                         value: if page == 1
                                         {
                                             50
                                         }
                                         else if page == 2
                                         {
                                             100
                                         }
                                         else
                                         {
                                             0
                                         },
                                         maximum: if page == 4 { 0 } else { 100 },
                                         active: u32::from(page != 3),
                                         highlighted: match page
                                         {
                                             5 => 20,
                                             6 => 21,
                                             7 => 129,
                                             _ => 0
                                         },
                                         reserved: 0 };
        let mut call = DefinitionCall { header:    Header::for_type::<DefinitionCall>(),
                                        kind:      CDEF,
                                        id:        1,
                                        variation: 0,
                                        message:   0,
                                        reserved:  0,
                                        param:     0,
                                        object:    (&mut state as *mut ScrollbarState).cast(),
                                        surface:   &mut surface,
                                        gate:      mac_system::toolbox::quickdraw::surface_gate };
        let result = unsafe { scrollbar::scrollbar_definition(&mut call) };
        if result != 0
        {
            return Err(format!("CDEF1 error{result}").into());
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

fn hit_probe(path: &str) -> Result<(), Box<dyn std::error::Error>>
{
    let mut output = BufWriter::new(File::create(path)?);
    write!(output, "[")?;
    let mut first = true;
    for page in [0, 3]
    {
        for (control, bounds) in [Rect::new(280, 0, 295, 180),
                                  Rect::new(300, 0, 316, 180),
                                  Rect::new(0, 180, 280, 195),
                                  Rect::new(0, 205, 280, 221)].into_iter()
                                                              .enumerate()
        {
            let vertical = control < 2;
            let size =
                if vertical { bounds.right - bounds.left } else { bounds.bottom - bounds.top };
            let length =
                if vertical { bounds.bottom - bounds.top } else { bounds.right - bounds.left };
            for along in [-1,
                          0,
                          1,
                          size - 1,
                          size,
                          size + 1,
                          2 * size - 1,
                          2 * size,
                          length / 2,
                          length - size - 1,
                          length - size,
                          length - 1,
                          length]
            {
                for cross in [-1, 0, 1, size / 2, size - 2, size - 1, size]
                {
                    let point = Point { h: bounds.left + if vertical { cross } else { along },
                                        v: bounds.top + if vertical { along } else { cross } };
                    let mut state = ScrollbarState { bounds,
                                                     value: 0,
                                                     maximum: 100,
                                                     active: u32::from(page == 0),
                                                     ..Default::default() };
                    let mut call =
                        DefinitionCall { header:    Header::for_type::<DefinitionCall>(),
                                         kind:      CDEF,
                                         id:        1,
                                         variation: 0,
                                         message:   1,
                                         reserved:  0,
                                         param:     (((point.v as u16 as u32) << 16)
                                                     | point.h as u16 as u32)
                                                    as i64,
                                         object:    (&mut state as *mut ScrollbarState).cast(),
                                         surface:   core::ptr::null_mut(),
                                         gate:      mac_system::toolbox::quickdraw::surface_gate };
                    let part = unsafe { scrollbar::scrollbar_definition(&mut call) };
                    if !first
                    {
                        write!(output, ",")?;
                    }
                    first = false;
                    write!(output,
                           "{{\"page\":{page},\"control\":{control},\"along\":{along},\"cross\":\
                            {cross},\"point\":[{},{}],\"part\":{part}}}",
                           point.h, point.v)?;
                }
            }
        }
    }
    writeln!(output, "]")?;
    output.flush()?;
    Ok(())
}

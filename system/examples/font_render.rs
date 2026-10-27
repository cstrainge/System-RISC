//! Render the exact text/positions used by the original QuickDrawProbe.
use std::{env, fs,
          io::{BufWriter, Write}};

use mac_system::toolbox::quickdraw::{Canvas, Point,
                                     fonts::{FontManager, FontPreference, FontSelection}};

fn main() -> Result<(), Box<dyn std::error::Error>>
{
    let args: Vec<_> = env::args().skip(1).collect();
    if args.len() < 3
    {
        return Err("usage: font_render System.rsrc output.ppm page[0,1,2] [--unhinted]".into());
    }
    let mut resources = mac_formats::resource::decode(&fs::read(&args[0])?)?;
    let fonts = FontManager::take_resources(&mut resources).map_err(|status| {
                    std::io::Error::other(format!("cannot initialize font catalog: status \
                                                   {status}"))
                })?;
    let page: u16 = args[2].parse()?;
    let preference = if args.iter().any(|a| a == "--unhinted")
    {
        FontPreference::OutlineUnhinted
    }
    else if page == 0
    {
        FontPreference::BitmapPreferred
    }
    else
    {
        FontPreference::OutlinePreferred
    };
    let mut pixels = vec![0xffffff; 640 * 460];
    let mut canvas = Canvas::new(&mut pixels, 640, 460);
    let cases = if page == 2
    {
        [0, 1, 2, 3, 4, 8, 16, 32, 64].into_iter()
                                      .map(|face| (20, 18, face, "Hamburgefonts 0123 éÅ"))
                                      .collect::<Vec<_>>()
    }
    else
    {
        vec![(0, 12, 0, "TEST RISCy System éÅ"),
             (3, 9, 0, "Hamburgefonts 0123 éÅ"),
             (4, 9, 0, "Hamburgefonts 0123 éÅ"),
             (4, 12, 0, "Hamburgefonts 0123 éÅ"),
             (20, 12, 0, "Hamburgefonts 0123 éÅ"),
             (20, 18, 0, "Hamburgefonts 0123 éÅ"),
             (20, 24, 0, "Hamburgefonts 0123 éÅ"),
             (21, 12, 0, "Hamburgefonts 0123 éÅ"),
             (21, 18, 0, "Hamburgefonts 0123 éÅ"),
             (22, 12, 0, "Hamburgefonts 0123 éÅ")]
    };
    for (row, (family, size, face, text)) in cases.into_iter().enumerate()
    {
        let selection = FontSelection { family,
                                        size,
                                        face };
        let metrics = fonts.metrics_with_preference(selection, preference)
                           .map_err(|s| format!("metrics status {s}"))?;
        let advance = fonts.draw_with_preference(selection,
                                                 &mut canvas,
                                                 Point { h: 16,
                                                         v: 32 + row as i16 * 40 },
                                                 text,
                                                 0,
                                                 preference)
                           .map_err(|s| format!("draw status {s}"))?;
        let width = fonts.text_width_with_preference(selection, text, preference)
                         .map_err(|s| format!("width status {s}"))?;
        println!("font page={page} row={row} family={family} size={size} face={face} \
                  text_width={width} advance={advance} ascent={} descent={} leading={} max={} \
                  source={:?}",
                 metrics.ascent,
                 metrics.descent,
                 metrics.leading,
                 metrics.max_width,
                 fonts.source(selection, preference).map_err(|s| format!("source status {s}"))?);
    }
    let mut output = BufWriter::new(fs::File::create(&args[1])?);
    write!(output, "P6\n640 460\n255\n")?;
    for pixel in pixels
    {
        output.write_all(&[(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8])?;
    }
    output.flush()?;
    Ok(())
}

//! Render the exact operation sequences in fixtures/quickdraw-reference/QuickDrawProbe.cc.
use std::{env,
          fs::File,
          io::{BufWriter, Write}};

use mac_system::toolbox::quickdraw::{Canvas, Point, Rect,
                                     port::{self, PortState, Shape, Verb},
                                     raster::{self, Bitmap, TransferMode},
                                     region::{BooleanOp, Region}};
fn draw(canvas: &mut Canvas, state: &PortState, shape: Shape, verb: Verb)
{
    port::draw(canvas, state, shape, verb).unwrap();
}
fn shapes(canvas: &mut Canvas)
{
    let mut state = PortState::default();
    for row in 0..2
    {
        state.pen_size = if row == 0
        {
            Point { h: 1,
                    v: 1 }
        }
        else
        {
            Point { h: 3,
                    v: 2 }
        };
        let y = 20 + row * 100;
        let mut r = Rect::new(20, y, 100, y + 60);
        draw(canvas, &state, Shape::Rect(r), Verb::Frame);
        r.offset(120, 0);
        draw(canvas, &state, Shape::Oval(r), Verb::Frame);
        r.offset(120, 0);
        draw(canvas,
             &state,
             Shape::RoundRect { bounds:      r,
                                oval_width:  24,
                                oval_height: 20 },
             Verb::Frame);
        r.offset(120, 0);
        draw(canvas,
             &state,
             Shape::Arc { bounds: r,
                          start:  30,
                          sweep:  240 },
             Verb::Frame);
        r.offset(120, 0);
        draw(canvas,
             &state,
             Shape::Arc { bounds: r,
                          start:  220,
                          sweep:  -160 },
             Verb::Frame);
    }
    state.pen_normal();
    let mut r = Rect::new(20, 220, 100, 280);
    draw(canvas, &state, Shape::Rect(r), Verb::Paint);
    r.offset(120, 0);
    draw(canvas, &state, Shape::Oval(r), Verb::Paint);
    r.offset(120, 0);
    draw(canvas,
         &state,
         Shape::RoundRect { bounds:      r,
                            oval_width:  24,
                            oval_height: 20 },
         Verb::Paint);
    r.offset(120, 0);
    draw(canvas,
         &state,
         Shape::Arc { bounds: r,
                      start:  30,
                      sweep:  240 },
         Verb::Paint);
    r.offset(120, 0);
    draw(canvas,
         &state,
         Shape::Arc { bounds: r,
                      start:  220,
                      sweep:  -160 },
         Verb::Paint);
    let dx = [75, 75, 25, -25, -75, -75, -25, 25];
    let dy = [25, 75, 75, 75, 25, -25, -75, -75];
    for i in 0..8
    {
        let x = 50 + i as i16 * 75;
        state.move_to(Point { h: x,
                              v: 370 });
        port::line_to(canvas,
                      &mut state,
                      Point { h: x + dx[i] / 2,
                              v: 370 + dy[i] / 2 }).unwrap();
    }
    let p = [Point { h: 20,
                     v: 410 },
             Point { h: 95,
                     v: 410 },
             Point { h: 60,
                     v: 425 },
             Point { h: 95,
                     v: 440 },
             Point { h: 20,
                     v: 440 },
             Point { h: 20,
                     v: 410 }];
    draw(canvas, &state, Shape::Polygon(&p), Verb::Paint);
    let p: Vec<_> = p.into_iter()
                     .map(|p| {
                         Point { h: p.h + 130,
                                 v: p.v }
                     })
                     .collect();
    state.pen_size = Point { h: 3,
                             v: 2 };
    draw(canvas, &state, Shape::Polygon(&p), Verb::Frame);
}
fn bitmodes(canvas: &mut Canvas)
{
    let source = [0xf0, 0, 0x0f, 0, 0xaa, 0, 0x55, 0, 0x81, 0, 0x42, 0, 0x24, 0, 0x18, 0];
    let bitmap = Bitmap::Mono { data:      &source,
                                row_bytes: 2,
                                bounds:    Rect::new(0, 0, 8, 8) };
    let mut state = PortState::default();
    for mode in 0..8
    {
        let r = Rect::new(20 + (mode % 4) * 150,
                          20 + (mode / 4) * 120,
                          100 + (mode % 4) * 150,
                          100 + (mode / 4) * 120);
        draw(canvas,
             &state,
             Shape::Rect(r),
             Verb::Fill([0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55]));
        raster::copy_bits(canvas,
                          &bitmap,
                          Rect::new(0, 0, 8, 8),
                          r,
                          TransferMode::new(mode).unwrap(),
                          None,
                          0,
                          0xffffff).unwrap();
    }
    for mode in 0..8
    {
        let r = Rect::new(20 + (mode % 4) * 150,
                          280 + (mode / 4) * 90,
                          120 + (mode % 4) * 150,
                          340 + (mode / 4) * 90);
        state.foreground = 0xff;
        draw(canvas, &state, Shape::Rect(r), Verb::Paint);
        state.foreground = 0xff0000;
        state.pen_mode = TransferMode::new(mode + 8).unwrap();
        draw(canvas, &state, Shape::Rect(r), Verb::Paint);
        state.pen_normal();
    }
}
fn regions(canvas: &mut Canvas)
{
    let state = PortState::default();
    let a = Region::from_rect(Rect::new(20, 20, 160, 120));
    let b = Region::from_rect(Rect::new(70, 50, 210, 150));
    let intersection = a.combine(&b, BooleanOp::Intersection);
    draw(canvas, &state, Shape::Region(&intersection), Verb::Paint);
    draw(canvas, &state, Shape::Region(&intersection.offset(200, 0).unwrap()), Verb::Frame);
    draw(canvas,
         &state,
         Shape::Region(&a.combine(&b, BooleanOp::Union).offset(0, 170).unwrap()),
         Verb::Paint);
    draw(canvas,
         &state,
         Shape::Region(&a.combine(&b, BooleanOp::Difference).offset(230, 170).unwrap()),
         Verb::Paint);
    draw(canvas,
         &state,
         Shape::Region(&a.combine(&b, BooleanOp::Xor).offset(400, 0).unwrap()),
         Verb::Paint);
    let oval = port::shape_region(Shape::Oval(Rect::new(20, 340, 100, 400))).unwrap();
    draw(canvas, &state, Shape::Region(&oval), Verb::Paint);
    let inset = oval.offset(150, 0).unwrap().inset(3, 2).unwrap();
    draw(canvas, &state, Shape::Region(&inset), Verb::Frame);
    for (name, r) in [("rect", a),
                      ("rect_b", b),
                      ("intersection", intersection),
                      ("oval60x80", oval),
                      ("inset_oval", inset)]
    {
        eprintln!("region {name} {}",
                  r.to_be_bytes().unwrap().iter().map(|b| format!("{b:02x}")).collect::<String>());
    }
}
fn direct_modes(canvas: &mut Canvas)
{
    let mut state = PortState::default();
    for mode in 0..8
    {
        let r = Rect::new(20 + (mode % 4) * 150,
                          20 + (mode / 4) * 120,
                          100 + (mode % 4) * 150,
                          100 + (mode / 4) * 120);
        state.foreground = 0xff;
        draw(canvas, &state, Shape::Rect(r), Verb::Paint);
        state.foreground = 0xff0000;
        state.pen_mode = TransferMode::new(mode + 8).unwrap();
        draw(canvas, &state, Shape::Rect(r), Verb::Paint);
        state.pen_normal();
    }
}
fn edge_corpus(path: &str) -> Result<(), Box<dyn std::error::Error>>
{
    let mut output = BufWriter::new(File::create(path)?);
    write!(output, "{{\"oval_regions\":[")?;
    let dims = (1..=16).flat_map(|h| (1..=16).map(move |w| (w, h))).chain([(79, 59),
                                                                           (80, 60),
                                                                           (81, 61),
                                                                           (78, 58),
                                                                           (74, 56),
                                                                           (24, 20),
                                                                           (60, 80),
                                                                           (61, 81),
                                                                           (59, 79),
                                                                           (32, 32),
                                                                           (33, 33)]);
    for (index, (w, h)) in dims.enumerate()
    {
        if index != 0
        {
            write!(output, ",")?;
        }
        let r = port::shape_region(Shape::Oval(Rect::new(0, 0, w, h))).unwrap();
        write!(output, "{{\"name\":\"oval_w{w}_h{h}\",\"bytes_hex\":\"")?;
        for byte in r.to_be_bytes().unwrap()
        {
            write!(output, "{byte:02x}")?;
        }
        write!(output, "\"}}")?;
    }
    write!(output, "],\"lines\":[")?;
    let mut count = 0;
    for (a, b) in [(37, 12),
                   (15, 4),
                   (17, 7),
                   (8, 3),
                   (1, 0),
                   (2, 1),
                   (3, 1),
                   (4, 1),
                   (5, 2),
                   (7, 3),
                   (13, 4)]
    {
        for transpose in [false, true]
        {
            for sx in [-1, 1]
            {
                for sy in [-1, 1]
                {
                    let (dx, dy) = if transpose { (b * sx, a * sy) } else { (a * sx, b * sy) };
                    let mut pixels = vec![0xffffff; 96 * 96];
                    let mut canvas = Canvas::new(&mut pixels, 96, 96);
                    let mut state = PortState { pen_location: Point { h: 48,
                                                                      v: 48 },
                                                ..PortState::default() };
                    port::line_to(&mut canvas,
                                  &mut state,
                                  Point { h: 48 + dx,
                                          v: 48 + dy }).unwrap();
                    if count != 0
                    {
                        write!(output, ",")?;
                    }
                    count += 1;
                    write!(output, "{{\"delta\":[{dx},{dy}],\"points\":[")?;
                    let mut comma = false;
                    for y in 0..96
                    {
                        for x in 0..96
                        {
                            if canvas.pixels[y * 96 + x] == 0
                            {
                                if comma
                                {
                                    write!(output, ",")?;
                                }
                                comma = true;
                                write!(output, "[{},{}]", x as i16 - 48, y as i16 - 48)?;
                            }
                        }
                    }
                    write!(output, "]}}")?;
                }
            }
        }
    }
    write!(output, "]}}")?;
    output.flush()?;
    Ok(())
}
fn main() -> Result<(), Box<dyn std::error::Error>>
{
    let args: Vec<_> = env::args().skip(1).collect();
    let page: u8 = args.first().ok_or("page output.ppm")?.parse()?;
    let path = args.get(1).ok_or("page output.ppm")?;
    if page == 6
    {
        return edge_corpus(path);
    }
    let mut pixels = vec![0xffffff; 640 * 460];
    // The original screen's visible region leaves these rounded bottom corners
    // at the pre-existing black display matte. Match that initial surface state.
    for (y, width) in [(455, 1), (456, 1), (457, 2), (458, 3), (459, 5)]
    {
        for x in 0..width
        {
            pixels[y * 640 + x] = 0;
            pixels[y * 640 + 639 - x] = 0;
        }
    }
    let mut canvas = Canvas::new(&mut pixels, 640, 460);
    match page
    {
        3 => shapes(&mut canvas),
        4 => bitmodes(&mut canvas),
        5 => regions(&mut canvas),
        7 => direct_modes(&mut canvas),
        _ => return Err("page must be 3,4,5,6,7".into())
    }
    let mut output = BufWriter::new(File::create(path)?);
    write!(output, "P6\n640 460\n255\n")?;
    for pixel in pixels
    {
        output.write_all(&[(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8])?;
    }
    output.flush()?;
    Ok(())
}

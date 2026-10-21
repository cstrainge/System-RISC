//! Interactive QuickDraw exercise. Every operation crosses the public Toolbox API.
use mac_toolbox::{Error, Point, Rect, System, WindowId,
                  quickdraw::{bitmaps::Bitmap,
                              fonts::{BOLD, FontSelection, ITALIC, UNDERLINE},
                              graphics::{GraphicsPort, Shape, TransferMode, Verb}}};
fn point(h: i16, v: i16) -> Point
{
    Point { h,
            v }
}
fn text(port: GraphicsPort<'_>, x: i16, y: i16, s: &str) -> Result<(), Error>
{
    port.text(FontSelection { family: 3,
                              size:   9,
                              face:   0 },
              point(x, y),
              s,
              0)
        .map(|_| ())
}
pub fn open(system: &System<'_>) -> Result<WindowId, Error>
{
    let layout = system.resources().application().get(crate::resources::WINDOW_LAYOUT)?;
    let mut bytes = [0; 20];
    if layout.size() != bytes.len() || layout.read(0, &mut bytes)? != bytes.len()
    {
        return Err(mac_toolbox::Error::BadArgument);
    }
    layout.release()?;
    let word = |at| i16::from_be_bytes([bytes[at], bytes[at + 1]]);
    let mut title = [0; 128];
    let title = system.resources().application().string(128, &mut title)?;
    system.new_window(Rect::new(word(2), word(0), word(6), word(4)), title, (400, 280), word(8))
}
pub fn selector(system: &System<'_>, index: usize)
                -> Result<mac_toolbox::controls::StandardState, Error>
{
    let mut bytes = [0; 128];
    let label = system.resources().application().indexed_string(2000, index as u32, &mut bytes)?;
    let left = 2 + index as i16 * 56;
    let mut state =
        mac_toolbox::controls::StandardState::new(Rect::new(left, 1, left + 53, 22), label);
    state.font = FontSelection { family: 3,
                                 size:   9,
                                 face:   BOLD };
    Ok(state)
}

pub fn draw(system: &System<'_>, id: WindowId, page: u8, icon_offset: u32,
            selectors: &[mac_toolbox::controls::ControlId; 10])
            -> Result<(), Error>
{
    let r = system.window(id)?.bounds;
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    system.begin_update(id)?;
    let port = system.quickdraw().graphics(id);
    let run = (|| -> mac_toolbox::ToolboxResult<()> {
        port.reset()?;
        port.colors(0, 0xffffff)?;
        port.paint_rect(Rect::new(0, 0, w, h))?;
        port.colors(0xffffff, 0xffffff)?;
        port.paint_rect(Rect::new(0, 0, w, h))?;
        port.colors(0, 0xffffff)?;
        for (index, id) in selectors.iter().enumerate()
        {
            system.controls()
                  .control(*id)
                  .update(mac_toolbox::controls::StandardKind::WindowFontButton,
                          selector(system, index)?)?;
        }
        port.move_to(point(0, 23))?;
        port.line_to(point(w, 23))?;
        match page
        {
            0 => shapes(port)?,
            1 => modes(port)?,
            2 => regions(system, port)?,
            3 => bitmaps(port)?,
            4 => fonts(system, port)?,
            5 => worlds(system, port)?,
            6 => pictures(port)?,
            7 => icons(system, port, icon_offset)?,
            8 => resources(system, port)?,
            _ => png(system, port)?
        }
        let mut bytes = [0; 128];
        let footer = system.resources().application().indexed_string(2000, 10, &mut bytes)?;
        text(port, 10, h - 24, footer)?;
        Ok(())
    })();
    system.end_update(id)?;
    run?;
    let marker = match page
    {
        0 => "QD_DEMO_READY page=1 shapes\n",
        1 => "QD_DEMO_READY page=2 transfer\n",
        2 => "QD_DEMO_READY page=3 regions\n",
        3 => "QD_DEMO_READY page=4 bitmaps\n",
        4 => "QD_DEMO_READY page=5 fonts\n",
        5 => "QD_DEMO_READY page=6 offscreen\n",
        6 => "QD_DEMO_READY page=7 pictures\n",
        7 => "QD_DEMO_READY page=8 icons\n",
        8 => "QD_DEMO_READY page=9 resources\n",
        _ => "QD_DEMO_READY page=10 png\n"
    };
    system.log(marker)
}
fn shapes(port: GraphicsPort<'_>) -> mac_toolbox::ToolboxResult<()>
{
    for (i, label) in ["Frame", "Paint", "Erase", "Invert", "Fill"].iter().enumerate()
    {
        text(port, 18 + i as i16 * 107, 40, label)?;
    }
    for column in 0..5
    {
        let x = 16 + column * 107;
        for row in 0..5
        {
            let y = 50 + row * 51;
            let r = Rect::new(x, y, x + 83, y + 36);
            port.colors(if row % 2 == 0 { 0x2b4dba } else { 0xba3a46 }, 0xffffff)?;
            if column == 2 || column == 3
            {
                port.draw(Shape::Rect(r),
                          Verb::Fill([0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55]))?;
            }
            let points =
                [point(x, y + 34), point(x + 35, y), point(x + 82, y + 30), point(x + 45, y + 18)];
            let shape = match row
            {
                0 => Shape::Rect(r),
                1 => Shape::Oval(r),
                2 =>
                {
                    Shape::RoundRect { bounds: r,
                                       corner: point(18, 18) }
                }
                3 =>
                {
                    Shape::Arc { bounds: r,
                                 start:  25,
                                 sweep:  265 }
                }
                _ => Shape::Polygon(&points)
            };
            let verb = match column
            {
                0 => Verb::Frame,
                1 => Verb::Paint,
                2 => Verb::Erase,
                3 => Verb::Invert,
                _ => Verb::Fill([0x88, 0x22, 0x88, 0x22, 0x88, 0x22, 0x88, 0x22])
            };
            port.draw(shape, verb)?;
        }
    }
    port.colors(0, 0xffffff)?;
    port.pen_size(3, 2)?;
    port.move_to(point(16, 316))?;
    port.line_to(point(535, 316))?;
    port.pen_size(1, 1)?;
    Ok(())
}
fn modes(port: GraphicsPort<'_>) -> mac_toolbox::ToolboxResult<()>
{
    use TransferMode::*;
    let modes = [Copy, Or, Xor, Bic, NotCopy, NotOr, NotXor, NotBic, PatternCopy, PatternOr,
                 PatternXor, PatternBic, NotPatternCopy, NotPatternOr, NotPatternXor,
                 NotPatternBic];
    let labels = ["srcCopy", "srcOr", "srcXor", "srcBic", "notSrcCopy", "notSrcOr", "notSrcXor",
                  "notSrcBic", "patCopy", "patOr", "patXor", "patBic", "notPatCopy", "notPatOr",
                  "notPatXor", "notPatBic"];
    for (i, mode) in modes.iter().enumerate()
    {
        let x = 14 + (i % 4) as i16 * 136;
        let y = 44 + (i / 4) as i16 * 69;
        text(port, x, y, labels[i])?;
        port.pen_mode(PatternCopy)?;
        port.pen_pattern([255; 8])?;
        port.colors(0x8ab4d9, 0xffffff)?;
        port.paint_rect(Rect::new(x, y + 7, x + 120, y + 49))?;
        port.colors(0x842044, 0xffffff)?;
        port.pen_pattern([0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55])?;
        port.pen_mode(*mode)?;
        port.paint_oval(Rect::new(x + 12, y + 12, x + 108, y + 45))?;
    }
    port.pen_mode(PatternCopy)?;
    port.pen_pattern([255; 8])?;
    port.colors(0, 0xffffff)?;
    Ok(())
}
fn regions(system: &System<'_>, port: GraphicsPort<'_>) -> mac_toolbox::ToolboxResult<()>
{
    let regions = system.quickdraw().regions();
    let mut ab = [0u8; 4096];
    let mut bb = [0u8; 4096];
    let mut output = [0u8; 8192];
    let a = regions.oval(Rect::new(15, 55, 150, 180), &mut ab)?;
    let b = regions.rectangle(Rect::new(80, 95, 180, 220), &mut bb)?;
    for i in 0..4
    {
        let r = match i
        {
            0 => regions.union(a, b, &mut output)?,
            1 => regions.intersection(a, b, &mut output)?,
            2 => regions.difference(a, b, &mut output)?,
            _ => regions.xor(a, b, &mut output)?
        };
        port.set_origin(point(i * 135, 0))?;
        port.colors([0x2b4dba, 0x339957, 0xba3a46, 0x973cba][i as usize], 0xffffff)?;
        port.draw(Shape::Region(r.bytes()),
                  Verb::Fill([0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55]))?;
    }
    port.set_origin(point(0, 0))?;
    port.colors(0, 0xffffff)?;
    text(port, 20, 244, "Union                 Intersection           Difference            XOR")?;
    let clip = regions.oval(Rect::new(30, 260, 530, 322), &mut ab)?;
    port.clip_region(clip.bytes())?;
    port.colors(0x276cb1, 0xffffff)?;
    port.paint_rect(Rect::new(0, 245, 560, 340))?;
    port.clip_rect(Rect::new(0, 0, 32767, 32767))?;
    port.colors(0, 0xffffff)?;
    Ok(())
}
fn bitmaps(port: GraphicsPort<'_>) -> mac_toolbox::ToolboxResult<()>
{
    let mono = [0x81, 0x42, 0x24, 0x18, 0x18, 0x24, 0x42, 0x81];
    let bitmap = Bitmap::Mono { data:      &mono,
                                row_bytes: 1,
                                bounds:    Rect::new(0, 0, 8, 8) };
    text(port, 16, 42, "1-bit scaled CopyBits")?;
    port.copy_bits(&bitmap, bitmap.bounds(), Rect::new(16, 53, 144, 181), TransferMode::Copy, &[])?;
    let mut palette = [0u32; 256];
    for (i, p) in palette.iter_mut().enumerate()
    {
        *p = ((i as u32) * 0x010101) ^ 0x304080;
    }
    let mut indices = [0u8; 256];
    for (i, p) in indices.iter_mut().enumerate()
    {
        *p = i as u8;
    }
    let indexed = Bitmap::Indexed { data:      &indices,
                                    row_bytes: 16,
                                    bounds:    Rect::new(0, 0, 16, 16),
                                    depth:     8,
                                    palette:   &palette };
    text(port, 195, 42, "8-bit indexed palette")?;
    port.copy_bits(&indexed,
                   indexed.bounds(),
                   Rect::new(195, 53, 323, 181),
                   TransferMode::Copy,
                   &[])?;
    let mut rgb = [0u32; 256];
    for (i, p) in rgb.iter_mut().enumerate()
    {
        *p = (((i % 16) * 17) as u32) << 16 | (((i / 16) * 17) as u32) << 8 | 0x80;
    }
    let color = Bitmap::Rgb32 { pixels:     &rgb,
                                row_pixels: 16,
                                bounds:     Rect::new(0, 0, 16, 16) };
    text(port, 375, 42, "32-bit RGB scaling")?;
    port.copy_bits(&color, color.bounds(), Rect::new(375, 53, 503, 181), TransferMode::Copy, &[])?;
    text(port, 16, 220, "Source clipping, scaling and all pixel reads happen in System.")?;
    for (i, mode) in
        [TransferMode::Copy, TransferMode::Or, TransferMode::Xor, TransferMode::Bic].iter()
                                                                                    .enumerate()
    {
        let x = 16 + i as i16 * 137;
        port.colors(0x739bad, 0xffffff)?;
        port.paint_rect(Rect::new(x, 240, x + 110, 316))?;
        port.copy_bits(&bitmap, bitmap.bounds(), Rect::new(x, 240, x + 110, 316), *mode, &[])?;
    }
    port.colors(0, 0xffffff)?;
    Ok(())
}
fn fonts(system: &System<'_>, port: GraphicsPort<'_>) -> mac_toolbox::ToolboxResult<()>
{
    let fonts = system.quickdraw().fonts();
    for i in 0..fonts.count()?.min(9)
    {
        let family = fonts.family(i)?;
        let y = 48 + i as i16 * 29;
        text(port, 10, y, family.name())?;
        for (column, size) in [12, 17, 24].iter().enumerate()
        {
            let selection = FontSelection { family: family.selection.family,
                                            size:   *size,
                                            face:   0 };
            port.text(selection, point(100 + column as i16 * 150, y), "Abc 123 é", 0)?;
        }
    }
    port.text(FontSelection { family: 20,
                              size:   18,
                              face:   BOLD | ITALIC | UNDERLINE },
              point(16, 325),
              "TrueType / bitmap styles",
              0x294ca0)?;
    Ok(())
}

fn worlds(system: &System<'_>, port: GraphicsPort<'_>) -> mac_toolbox::ToolboxResult<()>
{
    text(port, 16, 45, "Resident offscreen GWorld, then CopyBits into this window")?;
    let world = system.quickdraw().new_gworld(64, 64)?;
    let offscreen = world.graphics();
    offscreen.colors(0x325bab, 0xffffff)?;
    offscreen.paint_rect(Rect::new(0, 0, 64, 64))?;
    offscreen.colors(0xf4b832, 0xffffff)?;
    offscreen.paint_oval(Rect::new(4, 4, 60, 60))?;
    offscreen.colors(0x2e805c, 0xffffff)?;
    offscreen.draw(Shape::RoundRect { bounds: Rect::new(15, 20, 53, 49),
                                      corner: point(12, 12) },
                   Verb::Paint)?;
    offscreen.text(FontSelection { family: 3,
                                   size:   12,
                                   face:   0 },
                   point(18, 40),
                   "QD",
                   0xffffff)?;
    let mut pixels = [0u32; 64 * 64];
    if world.pixels(&mut pixels)? != pixels.len() || pixels[0] != 0x325bab
    {
        return Err(mac_toolbox::Error::BadArgument);
    }
    world.copy_to(port, Rect::new(0, 0, 64, 64), Rect::new(16, 62, 256, 302), TransferMode::Copy)?;
    world.copy_to(port, Rect::new(0, 0, 64, 64), Rect::new(300, 62, 428, 190), TransferMode::Copy)?;
    world.copy_to(port, Rect::new(0, 0, 64, 64), Rect::new(450, 62, 514, 126), TransferMode::Copy)?;
    drop(world);
    text(port, 285, 230, "Readback verified; source disposed.")?;
    text(port, 285, 250, "Retained copies survive redraw.")?;
    Ok(())
}

fn pictures(port: GraphicsPort<'_>) -> mac_toolbox::ToolboxResult<()>
{
    let picture = crate::resources::QUICKDRAW_DEMO_PICTURE.id;
    text(port, 16, 44, "Original System 7 recorded PICT, replayed by resident Rust QuickDraw")?;
    port.picture_resource(mac_toolbox::resources::APPLICATION,
                          picture,
                          Rect::new(16, 65, 236, 185))?;
    port.picture_resource(mac_toolbox::resources::APPLICATION,
                          picture,
                          Rect::new(330, 65, 440, 125))?;
    port.picture_resource(mac_toolbox::resources::APPLICATION,
                          picture,
                          Rect::new(16, 205, 236, 325))?;
    Ok(())
}
fn icons(system: &System<'_>, port: GraphicsPort<'_>, offset: u32)
         -> mac_toolbox::ToolboxResult<()>
{
    let icons = system.quickdraw().icons();
    text(port, 16, 42, "Original resource families: normal / selected / disabled")?;
    for i in 0..16
    {
        let index = offset + i;
        if index >= icons.count()?
        {
            break;
        }
        let id = icons.at(index)?;
        let x = 12 + (i % 4) as i16 * 136;
        let y = 56 + (i / 4) as i16 * 68;
        for (column, flags) in
            [Default::default(),
             mac_toolbox::quickdraw::icons::IconFlags { selected: true,
                                                        disabled: false,
                                                        ..Default::default() },
             mac_toolbox::quickdraw::icons::IconFlags { selected: false,
                                                        disabled: true,
                                                        ..Default::default() }].iter()
                                                                               .enumerate()
        {
            icons.plot(port.window_id(),
                       id,
                       Rect::new(x + column as i16 * 39, y, x + column as i16 * 39 + 32, y + 32),
                       *flags)?;
        }
        let mut label = Label::default();
        use core::fmt::Write;
        let _ = write!(&mut label, "Icon {}", id);
        text(port, x, y + 45, label.as_str())?;
    }
    text(port, 16, 320, "Left/right: browse every bundled icon family.")?;
    Ok(())
}
#[derive(Default)]
struct Label
{
    data:   [u8; 32],
    length: usize
}
impl core::fmt::Write for Label
{
    fn write_str(&mut self, s: &str) -> core::fmt::Result
    {
        let end = self.length + s.len();
        if end > self.data.len()
        {
            return Err(core::fmt::Error);
        }
        self.data[self.length..end].copy_from_slice(s.as_bytes());
        self.length = end;
        Ok(())
    }
}
impl Label
{
    fn as_str(&self) -> &str
    {
        core::str::from_utf8(&self.data[..self.length]).unwrap_or("")
    }
}
fn resources(system: &System<'_>, port: GraphicsPort<'_>) -> mac_toolbox::ToolboxResult<()>
{
    let resources = system.resources();
    // Exercise the real disk-loaded font store through the Resource Manager,
    // including name lookup and scope pinning after ownership moves to fonts.
    if resources.system().count(mac_toolbox::TypeCode::new(*b"sfnt"))? != 17
       || resources.system().count(mac_toolbox::TypeCode::new(*b"FOND"))? != 9
       || resources.type_count()? == 0
    {
        return Err(mac_toolbox::Error::BadArgument);
    }
    resources.type_at(0)?;
    let chicago = resources.system().named(mac_toolbox::TypeCode::new(*b"FOND"), "Chicago")?;
    if chicago.key().id != 0 || chicago.home() != mac_toolbox::resources::SYSTEM
    {
        return Err(mac_toolbox::Error::BadArgument);
    }
    chicago.release()?;
    let picture = resources.application().get(crate::resources::QUICKDRAW_DEMO_PICTURE)?;
    let mut header = [0; 10];
    if picture.read(0, &mut header)? != header.len()
       || picture.read(picture.size(), &mut header)? != 0
    {
        return Err(mac_toolbox::Error::BadArgument);
    }
    picture.release()?;
    text(port, 16, 45, "Resource Manager: application -> System search chain")?;
    for (row, kind) in [*b"ICN#", *b"icl8", *b"ics#", *b"cicn", *b"PICT", *b"CURS", *b"FOND",
                        *b"FONT", *b"NFNT", *b"sfnt", *b"BNDL", *b"FREF"].into_iter()
                                                                         .enumerate()
    {
        let count = resources.count(mac_toolbox::TypeCode::new(kind))?;
        let mut label = Label::default();
        use core::fmt::Write;
        let name = core::str::from_utf8(&kind).unwrap_or("????");
        let _ = write!(&mut label, "{}: {} resources", name, count);
        text(port, 16 + (row as i16 / 6) * 270, 75 + (row as i16 % 6) * 32, label.as_str())?;
        if count > 0
        {
            let resource = resources.at(mac_toolbox::TypeCode::new(kind), 0)?;
            resource.load()?;
            let mut prefix = [0; 16];
            let got = resource.read(0, &mut prefix)?;
            if got != resource.size().min(prefix.len())
            {
                return Err(mac_toolbox::Error::BadArgument);
            }
            resource.release()?;
        }
    }
    text(port, 16, 310,
         "Lookup, enumeration, metadata, partial reads, load and release verified.")?;
    Ok(())
}

fn png(system: &System<'_>, port: GraphicsPort<'_>) -> mac_toolbox::ToolboxResult<()>
{
    let resource = system.resources().application().get(crate::resources::QUICKDRAW_DEMO_PNG)?;
    let mut bytes = [0; 32 * 1024];
    if resource.size() > bytes.len()
    {
        return Err(mac_toolbox::Error::NoMemory);
    }
    let count = resource.read(0, &mut bytes)?;
    let image = &bytes[..count];
    let info = system.quickdraw().images().info(image).map_err(|e| e.service)?;
    text(port, 16, 44, "PNG: resident decoding, alpha composition, scaling and clipping")?;
    // A checkerboard makes translucent pixels easy to distinguish from opaque RGB.
    for row in 0..14i16
    {
        for column in 0..32i16
        {
            let color = if (row + column) % 2 == 0 { 0xd0d0d0 } else { 0xffffff };
            port.colors(color, 0xffffff)?;
            port.paint_rect(Rect::new(16 + column * 16,
                                      60 + row * 16,
                                      32 + column * 16,
                                      76 + row * 16))?;
        }
    }
    port.draw_png(image, Rect::new(24, 76, 120, 140)).map_err(|e| e.service)?;
    port.draw_png(image, Rect::new(160, 76, 448, 268)).map_err(|e| e.service)?;
    port.clip_rect(Rect::new(460, 100, 520, 240))?;
    port.draw_png(image, Rect::new(440, 76, 536, 268)).map_err(|e| e.service)?;
    port.clip_rect(Rect::new(0, 0, i16::MAX, i16::MAX))?;
    let mut label = Label::default();
    use core::fmt::Write;
    let _ = write!(&mut label, "PNG {}x{}, {} bit", info.width, info.height, info.bit_depth);
    text(port, 16, 308, label.as_str())?;
    text(port, 16, 326, "Static PNG. Transparency blends over existing QuickDraw pixels.")?;
    Ok(())
}

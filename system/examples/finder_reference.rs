//! Render Finder's actual icon-view policy against the original fixture catalog.
use std::{env, fs,
          io::{BufWriter, Write}};

use mac_abi::{Point, Rect, Status};
use mac_formats::resource;
use mac_system::toolbox::quickdraw::{Canvas,
                                     fonts::{FontManager, FontSelection},
                                     icons::{IconFamily, IconFlags}};
#[path = "../../finder/src/icon_view.rs"]
// This original-reference renderer only exercises System resource icons.
#[allow(dead_code)]
mod icon_view;
use icon_view::{Drawing, Layout, State};
struct Draw<'a, 'b>
{
    canvas:    &'a mut Canvas<'b>,
    fonts:     &'a FontManager,
    resources: &'a [resource::Resource]
}
impl Drawing for Draw<'_, '_>
{
    fn fill(&mut self, bounds: Rect, color: u32) -> Result<(), Status>
    {
        self.canvas.paint_rect(bounds, color);
        Ok(())
    }

    fn text_width(&mut self, text: &str) -> Result<i16, Status>
    {
        self.fonts.text_width(FontSelection { family: 3,
                                              size:   9,
                                              face:   0 },
                              text)
    }

    fn text(&mut self, baseline: Point, text: &str, color: u32) -> Result<(), Status>
    {
        self.fonts
            .draw(FontSelection { family: 3,
                                  size:   9,
                                  face:   0 },
                  self.canvas,
                  baseline,
                  text,
                  color)
            .map(|_| ())
    }

    fn icon(&mut self, id: icon_view::Icon, bounds: Rect, state: State) -> Result<(), Status>
    {
        let icon_view::Icon::Resource(id) = id
        else
        {
            return Err(-2);
        };
        IconFamily::from_resources(self.resources, id).map_err(|_| -2)?
                                                      .draw(self.canvas,
                                                            bounds,
                                                            IconFlags { selected:
                                                                            state.selected
                                                                            && state.active,
                                                                        open: state.open,
                                                                        ..Default::default() })
                                                      .map_err(|_| -2)
    }
}
fn main()
{
    let args: Vec<_> = env::args().collect();
    assert!(args.len() >= 3, "finder_reference CASE OUTPUT.ppm [NAME ...]");
    let mut resources =
        resource::decode(&fs::read("build/system7/dist/System.rsrc").unwrap()).unwrap();
    let fonts = FontManager::take_resources(&mut resources);
    let desktop = args[1].starts_with("desktop");
    let (width, height) = if desktop { (640, 480) } else { (404, 188) };
    let mut pixels = vec![0xffffff; width * height];
    let mut canvas = Canvas::new(&mut pixels, width, height);
    if desktop
    {
        canvas.fill_rect(Rect::new(0, 21, 640, 480),
                         [0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55],
                         0x878787,
                         0xc0c0c0);
    }
    else
    {
        canvas.set_clip(Rect::new(0, 20, 389, 173));
    }
    let mut draw = Draw { canvas:    &mut canvas,
                          fonts:     &fonts,
                          resources: &resources };
    if desktop
    {
        let two = args[1].contains("two");
        for index in 0..if two { 2 } else { 1 }
        {
            let selected = args[1].contains("selected") && (!two || index == 1);
            let open = args[1].contains("open") && (!two || index == 1);
            icon_view::draw_item(&mut draw,
                                 24000,
                                 icon_view::volume_bounds(index),
                                 if index == 0 { "REA Fixtures" } else { "Second Volume" },
                                 State { active: true,
                                         selected,
                                         open,
                                         desktop: true }).unwrap();
        }
        icon_view::draw_item(&mut draw,
                             -3993,
                             icon_view::TRASH_BOUNDS,
                             "Trash",
                             State { active: true,
                                     desktop: true,
                                     ..Default::default() }).unwrap();
    }
    else
    {
        let mut layout = Layout::new(404);
        let names: Vec<&str> = if args.len() > 3
        {
            args[3..].iter().map(String::as_str).collect()
        }
        else
        {
            vec!["Alpha", "Beta Folder", "Long folder name for wrapping", "System Folder Copy"]
        };
        for (index, name) in names.into_iter().enumerate()
        {
            let width = draw.text_width(name).unwrap();
            let bounds = layout.place(width);
            let state = State { active:   true,
                                selected: index == 0 && args[1].contains("selected"),
                                open:     index == 0 && args[1].contains("open"),
                                desktop:  false };
            icon_view::draw_item(&mut draw, -3999, bounds, name, state).unwrap();
            println!("item={} width={} rect={},{},{},{}",
                     name, width, bounds.left, bounds.top, bounds.right, bounds.bottom);
        }
    }
    let mut output = BufWriter::new(fs::File::create(&args[2]).unwrap());
    write!(output, "P6\n{width} {height}\n255\n").unwrap();
    for p in pixels
    {
        output.write_all(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]).unwrap();
    }
}

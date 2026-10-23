//! Bootstrap-only welcome panel. Progress follows completed initialization work;
//! later stages publish only the newly changed part of the striped bar.
use mac_formats::{Error, Result, resource::Resource};

use crate::toolbox::quickdraw::{Canvas, Point,
                                fonts::{FontManager, FontSelection}};
pub struct BootScreen
{
    panel: mac_boot_screen::Panel
}
impl BootScreen
{
    pub fn begin(resources: &[Resource], fonts: &FontManager) -> Result<Self>
    {
        let title = resources.iter()
                             .find(|r| r.kind == *b"STR " && r.id == 129)
                             .ok_or(Error::Malformed("boot title resource is missing"))?;
        let (&length, text) =
            title.data.split_first().ok_or(Error::Malformed("invalid boot title resource"))?;
        if text.len() != usize::from(length)
        {
            return Err(Error::Malformed("invalid boot title resource"));
        }
        let title = mac_formats::text::decode(text);
        let icon = resources.iter()
                            .find(|r| r.kind == *b"ICN#" && r.id == 32740)
                            .ok_or(Error::Malformed("boot icon resource is missing"))?;
        let icon: &[u8; 256] = icon.data
                                   .as_slice()
                                   .try_into()
                                   .map_err(|_| Error::Malformed("invalid boot icon resource"))?;
        let pixels = crate::platform::back_buffer();
        let panel=mac_boot_screen::Panel::begin(&mut mac_boot_screen::Raster::new(pixels,640,480).ok_or(Error::Malformed("invalid boot framebuffer"))?,7,icon);
        let font = FontSelection::default();
        let width = fonts.text_width(font, &title)
                         .map_err(|_| Error::Malformed("boot title font unavailable"))?;
        let mut canvas = Canvas::new(pixels, 640, 480);
        fonts.draw(font,
                   &mut canvas,
                   Point { h: 320 - width / 2,
                           v: mac_boot_screen::TITLE_BASELINE },
                   &title,
                   0)
             .map_err(|_| Error::Malformed("boot title drawing failed"))?;
        crate::platform::present(pixels);
        Ok(Self { panel })
    }

    pub fn completed(&mut self, stage: u8)
    {
        let pixels = crate::platform::back_buffer();
        let Some(mut raster) = mac_boot_screen::Raster::new(pixels, 640, 480)
        else
        {
            return;
        };
        if let Some(damage) = self.panel.advance(&mut raster, stage)
        {
            crate::platform::present_rects(pixels, [damage]);
            serial!("BOOT_PROGRESS completed={} total=7", stage);
        }
    }
}

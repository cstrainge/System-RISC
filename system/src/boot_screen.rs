//! Welcome panel using the same owned Control Manager object and loaded CDEF
//! as application progress bars. No desktop or app event loop is needed yet.
use mac_abi::{controls::*, definitions::ControlDefinitionState, *};
use mac_formats::{Error, Result, resource::Resource};

use crate::toolbox::{controls::objects::Manager,
                     quickdraw::{Canvas, Point,
                                 fonts::{FontManager, FontSelection}}};

pub struct BootScreen
{
    controls: Manager,
    id:       u64,
    entry:    DefinitionEntry,
    state:    ControlDefinitionState
}
fn failed(_: Status) -> Error
{
    Error::Malformed("boot Progress Bar control failed")
}
impl BootScreen
{
    pub fn begin(resources: &[Resource], fonts: &FontManager,
                 definitions: &mut crate::loader::Definitions)
                 -> Result<Self>
    {
        let title = resources.iter()
                             .find(|r| r.kind == *b"STR " && r.id == 129)
                             .ok_or(Error::Missing("boot title resource is missing"))?;
        let (&length, text) =
            title.data.split_first().ok_or(Error::Malformed("invalid boot title resource"))?;
        if text.len() != usize::from(length)
        {
            return Err(Error::Malformed("invalid boot title resource"));
        }
        let title = mac_formats::text::decode(text);
        let icon = resources.iter()
                            .find(|r| r.kind == *b"ICN#" && r.id == 32740)
                            .ok_or(Error::Missing("boot icon resource is missing"))?;
        let icon: &[u8; 256] = icon.data
                                   .as_slice()
                                   .try_into()
                                   .map_err(|_| Error::Malformed("invalid boot icon resource"))?;
        let entry = definitions.resolve(*b"CDEF", CDEF_PROGRESS_BAR)?;
        let pixels = crate::platform::back_buffer();
        mac_boot_screen::draw_panel(&mut mac_boot_screen::Raster::new(pixels, 640, 480)
            .ok_or(Error::Malformed("invalid boot framebuffer"))?, icon);
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
        let state = ControlDefinitionState { bounds: mac_boot_screen::PROGRESS,
                                             progress: ProgressRange { total: 7,
                                                                       determinate: 1,
                                                                       ..Default::default() },
                                             ..Default::default() };
        let mut controls = Manager::new(crate::toolbox::handles::controls_kinds().map_err(failed)?).map_err(failed)?;
        let mut call = ControlCall { operation: CREATE_STANDARD,
                                     procedure: PROGRESS_BAR_PROCEDURE,
                                     standard: state,
                                     ..Default::default() };
        controls.request(&crate::toolbox::handles::system_owner().map_err(failed)?,
                         &mut call,
                         entry,
                         crate::machine::traps::gateway)
                .map_err(failed)?;
        call.operation = DRAW_OBJECT;
        let drawing = controls.request(&crate::toolbox::handles::system_owner().map_err(failed)?,
                                       &mut call,
                                       entry,
                                       crate::machine::traps::gateway)
                              .map_err(failed)?
                              .ok_or(Error::Malformed("boot Progress Bar drawing is missing"))?;
        drawing.paint_clipped(&mut canvas).map_err(failed)?;
        crate::platform::present(pixels);
        Ok(Self { controls,
                  id: call.id,
                  entry,
                  state })
    }

    pub fn completed(&mut self, stage: u8) -> Result<()>
    {
        let value = u64::from(stage.min(7));
        if value <= self.state.progress.completed
        {
            return Ok(());
        }
        let mut next = self.state;
        next.progress.completed = value;
        let mut call = ControlCall { id: self.id,
                                     operation: CONFIGURE_OBJECT,
                                     procedure: PROGRESS_BAR_PROCEDURE,
                                     standard: next,
                                     ..Default::default() };
        let drawing = self.controls
                          .request(&crate::toolbox::handles::system_owner().map_err(failed)?,
                                   &mut call,
                                   self.entry,
                                   crate::machine::traps::gateway)
                          .map_err(failed)?;
        self.state = next;
        if let Some(drawing) = drawing
        {
            let pixels = crate::platform::back_buffer();
            let mut canvas = Canvas::new(pixels, 640, 480);
            let damage = drawing.damage();
            for rect in damage.into_iter().filter(|rect| !rect.is_empty())
            {
                canvas.set_clip(rect);
                drawing.paint_clipped(&mut canvas).map_err(failed)?;
            }
            crate::platform::present_rects(pixels,
                                           damage.into_iter().filter(|rect| !rect.is_empty()),
                                           None);
            serial!("BOOT_PROGRESS completed={} total=7", value);
        }
        Ok(())
    }
}
impl Drop for BootScreen
{
    fn drop(&mut self)
    {
        self.controls.remove_owner(0);
    }
}

//! Welcome panel using the same owned Control Manager object and loaded CDEF
//! as application progress bars. No desktop or app event loop is needed yet.
use mac_abi::{controls::*, definitions::ControlDefinitionState, *};
use mac_formats::{Error, Result, resource::Resource};

use crate::toolbox::{controls::objects::Manager,
                     quickdraw::{Canvas, Point,
                                 fonts::{FontManager, FontSelection}}};
mod strip;
pub(crate) use strip::Refusal as ExtensionRefusal;

pub struct BootScreen
{
    id:         u64,
    entry:      DefinitionEntry,
    state:      ControlDefinitionState,
    extensions: strip::Strip
}
fn failed(_: Status) -> Error
{
    Error::Malformed("boot Progress Bar control failed")
}
impl BootScreen
{
    pub fn begin(resources: &[Resource], fonts: &FontManager,
                 definitions: &mut crate::loader::Definitions, controls: &mut Manager)
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
        // Package admission and theme resolution happen before any paint or
        // module callback. Every outcome then owns ready-to-copy pixels.
        let extensions =
            strip::prepare_theme(resources, crate::display::WIDTH, crate::display::HEIGHT)?;
        let pixels = crate::platform::back_buffer();
        mac_boot_screen::draw_panel(&mut mac_boot_screen::Raster::new(pixels, crate::display::WIDTH, crate::display::HEIGHT)
            .ok_or(Error::Malformed("invalid boot framebuffer"))?, icon);
        let layout = mac_boot_screen::PanelLayout::new(crate::display::WIDTH as i16,
                                                       crate::display::HEIGHT as i16);
        let font = FontSelection::default();
        let width = fonts.text_width(font, &title)
                         .map_err(|_| Error::Malformed("boot title font unavailable"))?;
        let mut canvas = Canvas::new(pixels, crate::display::WIDTH, crate::display::HEIGHT);
        fonts.draw(font,
                   &mut canvas,
                   Point { h: crate::display::WIDTH as i16 / 2 - width / 2,
                           v: layout.title_baseline },
                   &title,
                   0)
             .map_err(|_| Error::Malformed("boot title drawing failed"))?;
        let state = ControlDefinitionState { bounds: layout.progress,
                                             progress: ProgressRange { total: 7,
                                                                       determinate: 1,
                                                                       ..Default::default() },
                                             ..Default::default() };
        let mut call = ControlCall { operation: CREATE_STANDARD,
                                     procedure: PROGRESS_BAR_PROCEDURE,
                                     standard: state,
                                     ..Default::default() };
        let owner = crate::toolbox::handles::system_owner().map_err(failed)?;
        controls.request(&owner, &owner, &mut call, entry, crate::machine::traps::gateway)
                .map_err(failed)?;
        call.operation = DRAW_OBJECT;
        let drawing =
            controls.request(&owner, &owner, &mut call, entry, crate::machine::traps::gateway)
                    .map_err(failed)?
                    .ok_or(Error::Malformed("boot Progress Bar drawing is missing"))?;
        drawing.paint_clipped(&mut canvas).map_err(failed)?;
        crate::platform::present(pixels);
        Ok(Self { id: call.id,
                  entry,
                  state,
                  extensions })
    }

    /// The loaded code's real result determines the icon. Artwork I/O and
    /// conversion finish first; no Files, registry, desktop or framebuffer borrow
    /// survives into the foreign initializer. A refusal does not publish hooks.
    pub(crate) fn extension_attempt(&mut self, name: &str, volume: u32, cnid: u32,
                                    refusal: Option<ExtensionRefusal>,
                                    call: impl FnOnce() -> core::result::Result<u64, Status>)
                                    -> core::result::Result<u64, Status>
    {
        if let Err(error) = self.present_extensions()
        {
            serial!("EXTENSION_STARTUP_PRESENTATION_REFUSED name={name:?} cause={error}; \
                     next=restart with fewer extensions or repair the display");
            return Err(SERVICE_BUSY);
        }
        let family = crate::kernel::with_resources(|resident| {
                         crate::kernel::with_files(|files| files.file_icon(volume, cnid, resident))
                .map_err(crate::state::AccessError::status)?
                     }).ok_or(NOT_SUPPORTED)
                       .and_then(|family| family);
        let (artwork, artwork_failure) = match family
        {
            Ok(family) =>
            {
                match strip::Bitmap::prepare(&family)
                {
                    Ok(bitmap) => (Some(bitmap), None),
                    Err(strip::Error::Artwork(status)) => (None, Some(status)),
                    Err(_) => (None, Some(NO_MEMORY))
                }
            }
            Err(NOT_FOUND) => (None, None),
            Err(status) => (None, Some(status))
        };
        let cause = refusal.or(artwork_failure.map(ExtensionRefusal::Artwork));
        let ordinal = self.extensions.len();
        let result = match self.extensions.attempt(name, artwork, cause, || {
                                              if let Some(status) = artwork_failure
                                              {
                                                  Err(status)
                                              }
                                              else
                                              {
                                                  call()
                                              }
                                          })
        {
            Ok(result) => result,
            Err(error) =>
            {
                serial!("EXTENSION_STARTUP_PRESENTATION_REFUSED name={name:?} cause={error}; \
                         next=restart with fewer extensions or repair the System theme");
                return Err(NO_MEMORY);
            }
        };
        if let Ok(module) = result.as_ref()
        {
            match crate::drivers::startup_pending(*module)
            {
                Ok(false) => (),
                Ok(true) => self.extensions.defer(ordinal),
                Err(status) =>
                {
                    self.extensions.defer(ordinal);
                    serial!("EXTENSION_STARTUP_STATUS_RETAINED name={name:?} status={status}; \
                             pending until authenticated scheduled confirmation");
                }
            }
        }
        if let Some(view) = self.extensions.view(ordinal)
        {
            if let strip::Outcome::Refused(cause) = view.outcome
            {
                serial!("EXTENSION_STARTUP_REFUSED name={:?} cause={} next={}",
                        view.name,
                        cause.cause(),
                        cause.action());
            }
        }
        // The initializer's result remains authoritative even if presentation
        // fails. A display error cannot turn published code into a fake refusal;
        // the exact pending cell stays retained for the next presentation retry.
        if let Err(error) = self.present_extensions()
        {
            serial!("EXTENSION_STARTUP_PRESENTATION_REFUSED name={name:?} cause={error}; \
                     next=repair the display and retry presenting the retained outcome");
        }
        result
    }

    pub(crate) fn extension_refused(&mut self, name: &str, volume: u32, cnid: u32,
                                    cause: ExtensionRefusal, status: Status)
    {
        // This is an outcome notification, not successful activation. The
        // initializer is never called and extension_attempt reports any display
        // refusal itself while preserving the named original refusal.
        let _: core::result::Result<u64, Status> =
            self.extension_attempt(name, volume, cnid, Some(cause), || Err(status));
    }

    fn present_extensions(&mut self) -> core::result::Result<(), strip::Error>
    {
        let Some(bounds) = self.extensions.paint_pending(crate::platform::back_buffer())?
        else
        {
            return Ok(());
        };
        crate::platform::present_rects(crate::platform::back_buffer(),
                                       [bounds],
                                       Default::default());
        let ordinal = self.extensions.len() - 1;
        if let Some(view) = self.extensions.view(ordinal)
        {
            serial!("EXTENSION_ICON name={:?} ordinal={} outcome={} artwork={} bounds={},{},{},{}",
                    view.name,
                    ordinal,
                    match view.outcome
                    {
                        strip::Outcome::Loaded => "ready",
                        strip::Outcome::Pending => "pending",
                        strip::Outcome::Refused(_) => "refused"
                    },
                    if view.source == strip::Source::Package { "package" } else { "theme" },
                    view.bounds.left,
                    view.bounds.top,
                    view.bounds.right,
                    view.bounds.bottom);
        }
        Ok(())
    }

    pub fn completed(&mut self, stage: u8, controls: &mut Manager) -> Result<()>
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
        let owner = crate::toolbox::handles::system_owner().map_err(failed)?;
        let drawing =
            controls.request(&owner, &owner, &mut call, self.entry, crate::machine::traps::gateway)
                    .map_err(failed)?;
        self.state = next;
        if let Some(drawing) = drawing
        {
            let pixels = crate::platform::back_buffer();
            let mut canvas = Canvas::new(pixels, crate::display::WIDTH, crate::display::HEIGHT);
            let damage = drawing.damage();
            for rect in damage.into_iter().filter(|rect| !rect.is_empty())
            {
                canvas.set_clip(rect);
                drawing.paint_clipped(&mut canvas).map_err(failed)?;
            }
            crate::platform::present_rects(pixels,
                                           damage.into_iter().filter(|rect| !rect.is_empty()),
                                           Default::default());
            serial!("BOOT_PROGRESS completed={} total=7", value);
        }
        Ok(())
    }

    /// Dispose only the boot panel's control through the same canonical manager
    /// that the caller transfers into Desktop. A refusal retains the exact ID
    /// for a subsequent cleanup attempt; unrelated System controls stay alive.
    pub fn finish(&mut self, controls: &mut Manager) -> Result<()>
    {
        if self.id == 0
        {
            return Ok(());
        }
        let owner = crate::toolbox::handles::system_owner().map_err(failed)?;
        let mut call = ControlCall { id: self.id,
                                     operation: DISPOSE_OBJECT,
                                     ..Default::default() };
        controls.request(&owner, &owner, &mut call, self.entry, crate::machine::traps::gateway)
                .map_err(failed)?;
        self.id = 0;
        Ok(())
    }
}

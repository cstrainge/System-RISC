//! Retained drawing shared by window content and the application-owned desktop layer.
#![forbid(unsafe_code)]
use alloc::{string::String, vec::Vec};

use mac_abi::{BAD_ARGUMENT, DrawCommand, Point, Rect, Status, WindowId};

use super::{Canvas, fonts::FontManager, fonts_service, service};

pub enum RenderCommand
{
    Legacy(alloc::boxed::Box<DrawCommand>),
    Control(crate::toolbox::controls::Drawing),
    ScrollBox(crate::toolbox::controls::scrollbox::Drawing),
    Graphics(service::Drawing),
    Picture(super::pictures_service::Drawing),
    Image(super::png::Drawing),
    Icon(super::icons_service::Drawing),
    Bitmap(super::bitmaps_service::BitmapDrawing),
    Text
    {
        font:         mac_abi::quickdraw::FontSelection,
        baseline:     Point,
        text:         String,
        color:        u32,
        port:         service::Port,
        vertical_ink: Option<(i32, i32)>
    }
}
impl RenderCommand
{
    pub fn storage(&self) -> usize
    {
        match self
        {
            Self::Control(c) => c.storage(),
            Self::ScrollBox(c) => c.storage(),
            Self::Legacy(_) => core::mem::size_of::<DrawCommand>(),
            Self::Graphics(g) => g.storage(),
            Self::Picture(p) => p.storage(),
            Self::Image(i) => i.storage(),
            Self::Icon(i) => i.storage(),
            Self::Bitmap(b) => b.storage() + b.port.storage(),
            Self::Text { text,
                         port,
                         .. } => text.len() + port.storage() + 32
        }
    }

    fn paint(&self, canvas: &mut Canvas, fonts: Option<&FontManager>,
             legacy: &mut impl FnMut(&mut Canvas, &DrawCommand))
    {
        match self
        {
            Self::Legacy(c) => legacy(canvas, c),
            Self::Control(c) => c.paint(canvas),
            Self::ScrollBox(c) => c.paint(canvas),
            Self::Graphics(d) =>
            {
                let _ = d.paint(canvas);
            }
            Self::Bitmap(d) =>
            {
                let _ = d.paint(canvas);
            }
            Self::Icon(d) =>
            {
                let _ = d.paint(canvas);
            }
            Self::Picture(d) =>
            {
                if let Some(fonts) = fonts
                {
                    let _ = d.paint(canvas, fonts);
                }
            }
            Self::Image(d) => d.paint(canvas),
            Self::Text { font,
                         baseline,
                         text,
                         color,
                         port,
                         vertical_ink } =>
            {
                if let Some(fonts) = fonts
                {
                    service::with_clip(canvas, port, |canvas| {
                        // Reject clipped-out text before decoding/rasterizing its
                        // glyphs. A narrow viewport update must not execute the
                        // fixed header's font renderer merely to discard pixels.
                        if let Some((top, bottom)) = vertical_ink
                        {
                            let baseline = i32::from(baseline.v) + i32::from(canvas.origin().v);
                            let top = baseline + top;
                            let bottom = baseline + bottom;
                            if top >= i32::from(canvas.clip().bottom)
                               || bottom <= i32::from(canvas.clip().top)
                            {
                                return;
                            }
                        }
                        let _ = fonts.draw(fonts_service::selection(*font),
                                           canvas,
                                           *baseline,
                                           text,
                                           *color);
                    });
                }
            }
        }
    }
}

pub struct Content
{
    pub id:       WindowId,
    pub commands: Vec<RenderCommand>,
    pub port:     service::Port
}
impl Content
{
    pub fn new(id: WindowId) -> Self
    {
        Self { id,
               commands: Vec::new(),
               port: service::Port::default() }
    }

    pub fn storage(&self) -> usize
    {
        self.port.storage() + self.commands.iter().map(RenderCommand::storage).sum::<usize>()
    }

    pub fn paint(&self, canvas: &mut Canvas, fonts: Option<&FontManager>,
                 mut legacy: impl FnMut(&mut Canvas, &DrawCommand))
    {
        for command in &self.commands
        {
            command.paint(canvas, fonts, &mut legacy);
        }
    }
}

/// A scene replacement is staged until EndUpdate; input/presentation may run
/// during an update without exposing a half-built desktop or losing the old one.
pub struct DesktopLayer
{
    bounds:    Rect,
    committed: Content,
    pending:   Option<Content>
}
impl DesktopLayer
{
    pub fn new(bounds: Rect) -> Self
    {
        let mut committed = Content::new(WindowId(mac_abi::quickdraw::DESKTOP_TARGET));
        committed.port.state.clip = Some(super::region::Region::from_rect(bounds));
        Self { bounds,
               committed,
               pending: None }
    }

    /// Validate and execute desktop-only QuickDraw operations. True means a
    /// replacement scene was committed and should be presented immediately.
    pub fn request(&mut self, call: &mut mac_abi::quickdraw::GraphicsCall) -> Result<bool, Status>
    {
        use mac_abi::quickdraw::*;
        if call.window != DESKTOP_TARGET
        {
            return Err(BAD_ARGUMENT);
        }
        let committed = match call.operation
        {
            QD_DESKTOP_BEGIN =>
            {
                self.begin_update()?;
                false
            }
            QD_DESKTOP_END =>
            {
                self.end_update()?;
                true
            }
            QD_DESKTOP_CANCEL =>
            {
                self.cancel_update()?;
                false
            }
            QD_DESKTOP_BOUNDS => false,
            _ => return Err(mac_abi::NOT_SUPPORTED)
        };
        call.bounds = self.bounds;
        call.state = self.port().wire();
        Ok(committed)
    }

    pub fn bounds(&self) -> Rect
    {
        self.bounds
    }

    pub fn begin_update(&mut self) -> Result<(), Status>
    {
        if self.pending.is_some()
        {
            return Err(BAD_ARGUMENT);
        }
        let mut content = Content::new(self.committed.id);
        content.port.state.clip = Some(super::region::Region::from_rect(self.bounds));
        self.pending = Some(content);
        Ok(())
    }

    pub fn end_update(&mut self) -> Result<(), Status>
    {
        self.committed = self.pending.take().ok_or(BAD_ARGUMENT)?;
        Ok(())
    }

    pub fn cancel_update(&mut self) -> Result<(), Status>
    {
        self.pending.take().ok_or(BAD_ARGUMENT)?;
        Ok(())
    }

    pub fn pending_mut(&mut self) -> Result<&mut Content, Status>
    {
        self.pending.as_mut().ok_or(BAD_ARGUMENT)
    }

    pub fn port(&self) -> &service::Port
    {
        &self.pending.as_ref().unwrap_or(&self.committed).port
    }

    pub fn storage(&self) -> usize
    {
        self.committed.storage() + self.pending.as_ref().map_or(0, Content::storage)
    }

    pub fn paint(&self, canvas: &mut Canvas, fonts: Option<&FontManager>,
                 legacy: impl FnMut(&mut Canvas, &DrawCommand))
    {
        let origin = canvas.origin();
        let clip = canvas.clip();
        let mut visible = Rect::ZERO;
        super::geometry::sect_rect(clip, self.bounds, &mut visible);
        canvas.set_origin(0, 0);
        canvas.set_clip(visible);
        self.committed.paint(canvas, fonts, legacy);
        canvas.set_clip(clip);
        canvas.set_origin(origin.h, origin.v);
    }
}

#[cfg(test)]
mod tests
{
    use alloc::vec;

    use mac_abi::{PAINT_RECT, quickdraw::*};

    use super::*;

    fn paint_legacy(canvas: &mut Canvas, command: &DrawCommand)
    {
        assert_eq!(command.kind, PAINT_RECT);
        canvas.paint_rect(command.bounds, command.color);
    }
    fn rectangle(layer: &mut DesktopLayer, bounds: Rect, color: u32)
    {
        layer.pending_mut()
             .unwrap()
             .commands
             .push(RenderCommand::Legacy(alloc::boxed::Box::new(DrawCommand { kind: PAINT_RECT,
                                                       bounds,
                                                       color,
                                                       text_length: 0,
                                                       text: [0; mac_abi::INLINE_STRING_BYTES] })));
    }
    fn pixel(layer: &DesktopLayer) -> u32
    {
        let mut pixels = vec![0x878787; 64 * 48];
        layer.paint(&mut Canvas::new(&mut pixels, 64, 48), None, paint_legacy);
        pixels[30 * 64 + 20]
    }
    #[test]
    fn desktop_replacements_are_atomic_and_cancellable()
    {
        let mut layer = DesktopLayer::new(Rect::new(0, 21, 64, 48));
        assert_eq!(layer.end_update(), Err(BAD_ARGUMENT));
        assert_eq!(layer.pending_mut().err(), Some(BAD_ARGUMENT));
        layer.begin_update().unwrap();
        assert_eq!(layer.begin_update(), Err(BAD_ARGUMENT));
        rectangle(&mut layer, Rect::new(0, 0, 64, 48), 0x123456);
        assert_eq!(pixel(&layer), 0x878787);
        layer.end_update().unwrap();
        assert_eq!(pixel(&layer), 0x123456);
        layer.begin_update().unwrap();
        rectangle(&mut layer, Rect::new(0, 0, 64, 48), 0xff0000);
        assert_eq!(pixel(&layer), 0x123456);
        layer.cancel_update().unwrap();
        assert_eq!(pixel(&layer), 0x123456);
        layer.begin_update().unwrap();
        assert!(layer.pending_mut().unwrap().commands.is_empty());
        assert_eq!(layer.port().wire().clip, layer.bounds());
        layer.end_update().unwrap();
        assert_eq!(pixel(&layer), 0x878787);
    }
    #[test]
    fn desktop_controls_and_clip_protect_menu_and_restore_canvas()
    {
        let mut layer = DesktopLayer::new(Rect::new(0, 21, 64, 48));
        let mut call = GraphicsCall { window: 0,
                                      operation: QD_DESKTOP_BEGIN,
                                      ..Default::default() };
        assert_eq!(layer.request(&mut call), Err(BAD_ARGUMENT));
        call.window = DESKTOP_TARGET;
        assert_eq!(layer.request(&mut call), Ok(false));
        assert_eq!(call.bounds, Rect::new(0, 21, 64, 48));
        rectangle(&mut layer, Rect::new(-100, -100, 100, 100), 0x123456);
        // Even unrestricted application clipping cannot write into System's menu.
        layer.pending_mut().unwrap().port.state.clip = None;
        call.operation = QD_DESKTOP_END;
        assert_eq!(layer.request(&mut call), Ok(true));
        let mut pixels = vec![0xffffff; 64 * 48];
        let mut canvas = Canvas::new(&mut pixels, 64, 48);
        canvas.set_origin(2, 3);
        canvas.set_clip(Rect::new(8, 7, 30, 35));
        let origin = canvas.origin();
        let clip = canvas.clip();
        layer.paint(&mut canvas, None, paint_legacy);
        assert_eq!(canvas.origin(), origin);
        assert_eq!(canvas.clip(), clip);
        assert_eq!(pixels[20 * 64 + 20], 0xffffff);
        assert_eq!(pixels[21 * 64 + 20], 0x123456);
        assert_eq!(pixels[30 * 64 + 9], 0xffffff);
        assert_eq!(pixels[30 * 64 + 10], 0x123456);
        assert_eq!(pixels[40 * 64 + 20], 0xffffff);
    }
    #[test]
    fn windows_occlude_desktop_and_disposal_reveals_retained_pixels()
    {
        use crate::toolbox::{events::EventManager, windows::WindowManager};
        let mut layer = DesktopLayer::new(Rect::new(0, 21, 640, 480));
        layer.begin_update().unwrap();
        rectangle(&mut layer, Rect::new(100, 60, 240, 210), 0x123456);
        layer.end_update().unwrap();
        let mut windows = WindowManager::new();
        let mut events = EventManager::new();
        let id = windows.new_window(Rect::new(120, 100, 220, 180),
                                    "Window",
                                    0,
                                    (40, 30),
                                    &mut events,
                                    0)
                        .unwrap();
        let mut pixels = vec![0x878787; 640 * 480];
        {
            let mut canvas = Canvas::new(&mut pixels, 640, 480);
            layer.paint(&mut canvas, None, paint_legacy);
            windows.draw(&mut canvas, |window, canvas| {
                       canvas.paint_rect(window.local_bounds(), 0xffffff)
                   });
        }
        assert_eq!(pixels[120 * 640 + 110], 0x123456);
        assert_eq!(pixels[120 * 640 + 150], 0xffffff);
        // A desktop scene committed behind a window does not overwrite its content.
        layer.begin_update().unwrap();
        rectangle(&mut layer, Rect::new(100, 60, 240, 210), 0xabcdef);
        layer.end_update().unwrap();
        pixels.fill(0x878787);
        {
            let mut canvas = Canvas::new(&mut pixels, 640, 480);
            layer.paint(&mut canvas, None, paint_legacy);
            windows.draw(&mut canvas, |window, canvas| {
                       canvas.paint_rect(window.local_bounds(), 0xffffff)
                   });
        }
        assert_eq!(pixels[120 * 640 + 150], 0xffffff);
        assert!(windows.dispose_window(id, &mut events, 1));
        pixels.fill(0x878787);
        layer.paint(&mut Canvas::new(&mut pixels, 640, 480), None, paint_legacy);
        assert_eq!(pixels[120 * 640 + 150], 0xabcdef);
    }
}

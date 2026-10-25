//! Retained drawing shared by window content and the application-owned desktop layer.
#![forbid(unsafe_code)]
use alloc::{string::String, sync::Arc, vec::Vec};

use mac_abi::{BAD_ARGUMENT, DrawCommand, Point, Rect, Status, WindowId};

use super::{Canvas,
            fonts::FontManager,
            fonts_service,
            region::{BooleanOp, Region},
            service};

/// Retained scenes have an explicit kind, independent of object token bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneTarget
{
    Window(WindowId),
    Desktop
}
impl From<WindowId> for SceneTarget
{
    fn from(id: WindowId) -> Self
    {
        Self::Window(id)
    }
}
impl From<u64> for SceneTarget
{
    fn from(id: u64) -> Self
    {
        Self::Window(WindowId(id))
    }
}
impl PartialEq<WindowId> for SceneTarget
{
    fn eq(&self, id: &WindowId) -> bool
    {
        *self == Self::Window(*id)
    }
}
impl SceneTarget
{
    pub fn decode(target: mac_abi::quickdraw::Target) -> Result<Self, Status>
    {
        match target.decode().map_err(|_| BAD_ARGUMENT)?
        {
            mac_abi::quickdraw::DrawingTarget::Desktop => Ok(Self::Desktop),
            mac_abi::quickdraw::DrawingTarget::Window(id) => Ok(Self::Window(id)),
            mac_abi::quickdraw::DrawingTarget::World(_) => Err(BAD_ARGUMENT)
        }
    }

    pub fn window(self) -> Result<WindowId, Status>
    {
        match self
        {
            Self::Window(id) => Ok(id),
            Self::Desktop => Err(BAD_ARGUMENT)
        }
    }
}

/// Immutable retained text: shared editor snapshots avoid one allocation per
/// visible line and survive subsequent in-place document edits.
pub enum TextData
{
    Owned(String),
    Shared
    {
        source: Arc<String>,
        range:  core::ops::Range<usize>
    }
}
impl TextData
{
    fn as_str(&self) -> &str
    {
        match self
        {
            Self::Owned(s) => s,
            Self::Shared { source,
                           range } => &source[range.clone()]
        }
    }

    fn storage(&self) -> usize
    {
        // Conservative charging includes the whole snapshot for each reference.
        match self
        {
            Self::Owned(s) => s.len(),
            Self::Shared { source,
                           .. } => source.len()
        }
    }
}
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
    Caret
    {
        editor: mac_abi::textedit::EditorId,
        info:   mac_abi::textedit::EditorInfo,
        bounds: Rect,
        port:   service::Port
    },
    Text
    {
        font:         mac_abi::quickdraw::FontSelection,
        baseline:     Point,
        text:         TextData,
        color:        u32,
        port:         Arc<service::Port>,
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
            Self::Caret { port,
                          .. } => port.storage() + 96,
            Self::Bitmap(b) => b.storage() + b.port.storage(),
            Self::Text { text,
                         port,
                         .. } => text.storage() + port.storage() + 32
        }
    }

    fn paint(&self, canvas: &mut Canvas, fonts: Option<&FontManager>,
             legacy: &mut impl FnMut(&mut Canvas, &DrawCommand))
    {
        match self
        {
            Self::Caret { bounds,
                          port,
                          info,
                          .. } =>
            {
                if info.active != 0
                   && info.caret_visible != 0
                   && info.selection_start == info.selection_end
                {
                    service::with_clip(canvas, port, |canvas| canvas.invert_rect(*bounds));
                }
            }
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
                                           text.as_str(),
                                           *color);
                    });
                }
            }
        }
    }
}

pub struct Content
{
    pub id:       SceneTarget,
    pub commands: Vec<RenderCommand>,
    pub port:     service::Port,
    fragments:    Vec<Fragment>,
    patch:        Option<Region>,
    pub outline:  Option<Option<Rect>>
}
struct Fragment
{
    clip:     Region,
    commands: Vec<RenderCommand>
}
fn region_bytes(region: &Region) -> usize
{
    region.bands().iter().map(|band| 8 + band.spans.len() * 4).sum()
}
impl Content
{
    pub fn new(id: impl Into<SceneTarget>) -> Self
    {
        Self { id:        id.into(),
               commands:  Vec::new(),
               port:      service::Port::default(),
               fragments: Vec::new(),
               patch:     None,
               outline:   None }
    }

    pub fn pending_patch(id: impl Into<SceneTarget>, port: service::Port, patch: Region) -> Self
    {
        Self { port,
               patch: Some(patch),
               ..Self::new(id) }
    }

    pub fn is_patch(&self) -> bool
    {
        self.patch.is_some()
    }

    pub fn iter_commands(&self) -> impl Iterator<Item = &RenderCommand>
    {
        self.fragments.iter().flat_map(|f| f.commands.iter()).chain(&self.commands)
    }

    pub fn iter_commands_mut(&mut self) -> impl Iterator<Item = &mut RenderCommand>
    {
        self.fragments.iter_mut().flat_map(|f| f.commands.iter_mut()).chain(&mut self.commands)
    }

    pub fn blink_caret(&mut self, editor: mac_abi::textedit::EditorId,
                       before: mac_abi::textedit::EditorInfo, after: mac_abi::textedit::EditorInfo)
                       -> Option<Region>
    {
        fn toggle(commands: &mut [RenderCommand], mask: Option<&Region>,
                  editor: mac_abi::textedit::EditorId, before: mac_abi::textedit::EditorInfo,
                  after: mac_abi::textedit::EditorInfo, damage: &mut Region, found: &mut bool)
        {
            for command in commands
            {
                if let RenderCommand::Caret { editor: id,
                                              info,
                                              bounds,
                                              port } = command
                   && *id == editor
                   && *info == before
                {
                    *info = after;
                    *found = true;
                    let mut region = Region::from_rect(*bounds);
                    if let Some(clip) = &port.state.clip
                    {
                        region = region.combine(clip, BooleanOp::Intersection);
                    }
                    if let Ok(region) = region.offset(port.origin.h, port.origin.v)
                    {
                        let region =
                            mask.map_or_else(|| region.clone(),
                                             |mask| region.combine(mask, BooleanOp::Intersection));
                        *damage = damage.combine(&region, BooleanOp::Union);
                    }
                }
            }
        }
        let mut damage = Region::from_rect(Rect::ZERO);
        let mut found = false;
        for fragment in &mut self.fragments
        {
            toggle(&mut fragment.commands,
                   Some(&fragment.clip),
                   editor,
                   before,
                   after,
                   &mut damage,
                   &mut found);
        }
        toggle(&mut self.commands, None, editor, before, after, &mut damage, &mut found);
        found.then_some(damage)
    }

    pub fn retain_commands(&mut self, mut keep: impl FnMut(&RenderCommand) -> bool)
    {
        for fragment in &mut self.fragments
        {
            fragment.commands.retain(&mut keep);
        }
        self.commands.retain(keep);
    }

    /// Prepare every allocation and budget check before publishing any scene
    /// change. A later patch subtracts its mask from prior fragments, so repeated
    /// selection changes replace pixels rather than accumulating a paint history.
    pub fn commit_patch(&mut self, mut pending: Self, bounds: Rect, used: usize, budget: usize)
                        -> Result<Region, Status>
    {
        let mask = pending.patch.take().ok_or(BAD_ARGUMENT)?;
        let committed_clip = mask.clone();
        self.fragments.try_reserve(2).map_err(|_| mac_abi::NO_MEMORY)?;
        let mut clips = Vec::new();
        let mut before = 0;
        for (index, fragment) in self.fragments.iter().enumerate()
        {
            if !fragment.clip.overlaps(&mask)
            {
                continue;
            }
            clips.try_reserve(1).map_err(|_| mac_abi::NO_MEMORY)?;
            before += region_bytes(&fragment.clip);
            clips.push((index, fragment.clip.combine(&mask, BooleanOp::Difference)));
        }
        let tail = (!self.commands.is_empty()).then(|| {
                                                  Region::from_rect(bounds).combine(&mask,
                                                                             BooleanOp::Difference)
                                              });
        let after = clips.iter().map(|(_, r)| region_bytes(r)).sum::<usize>()
                    + tail.as_ref().map_or(0, region_bytes)
                    + region_bytes(&mask);
        if used.saturating_add(after.saturating_sub(before)) > budget
        {
            return Err(mac_abi::NO_MEMORY);
        }
        for (index, clip) in clips
        {
            self.fragments[index].clip = clip;
        }
        self.fragments.retain(|f| !f.clip.is_empty());
        if let Some(clip) = tail.filter(|clip| !clip.is_empty())
        {
            self.fragments.push(Fragment { clip,
                                           commands: core::mem::take(&mut self.commands) });
        }
        else
        {
            self.commands.clear();
        }
        self.fragments.push(Fragment { clip:     committed_clip,
                                       commands: pending.commands });
        self.port = pending.port;
        Ok(mask)
    }

    pub fn storage(&self) -> usize
    {
        self.port.storage()
        + self.iter_commands().map(RenderCommand::storage).sum::<usize>()
        + self.fragments.iter().map(|f| region_bytes(&f.clip)).sum::<usize>()
        + self.patch.as_ref().map_or(0, region_bytes)
    }

    pub fn paint(&self, canvas: &mut Canvas, fonts: Option<&FontManager>,
                 mut legacy: impl FnMut(&mut Canvas, &DrawCommand))
    {
        for fragment in &self.fragments
        {
            service::with_region_clip(canvas, &fragment.clip, |canvas| {
                for command in &fragment.commands
                {
                    command.paint(canvas, fonts, &mut legacy);
                }
            });
        }
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
    pending:   Option<Content>,
    outline:   Option<Rect>
}
impl DesktopLayer
{
    pub fn new(bounds: Rect) -> Self
    {
        let mut committed = Content::new(SceneTarget::Desktop);
        committed.port.state.clip = Some(super::region::Region::from_rect(bounds));
        Self { bounds,
               committed,
               pending: None,
               outline: None }
    }

    /// Validate and execute desktop-only QuickDraw operations. True means a
    /// replacement scene was committed and should be presented immediately.
    pub fn request(&mut self, call: &mut mac_abi::quickdraw::GraphicsCall) -> Result<bool, Status>
    {
        use mac_abi::quickdraw::*;
        if call.target.decode().map_err(|_| BAD_ARGUMENT)? != DrawingTarget::Desktop
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

    pub fn is_updating(&self) -> bool
    {
        self.pending.is_some()
    }

    pub fn outline(&self) -> Option<Rect>
    {
        self.outline
    }

    pub fn set_outline(&mut self, next: Option<Rect>) -> bool
    {
        if self.outline == next
        {
            return false;
        }
        self.outline = next;
        true
    }

    pub fn pending_outline(&self) -> Option<Option<Rect>>
    {
        self.pending.as_ref().and_then(|p| p.outline)
    }

    pub fn begin_mask(&mut self, mask: Region, used: usize, budget: usize) -> Result<(), Status>
    {
        if self.pending.is_some()
        {
            return Err(BAD_ARGUMENT);
        }
        let mask = mask.combine(&Region::from_rect(self.bounds), BooleanOp::Intersection);
        if mask.is_empty()
        {
            return Err(BAD_ARGUMENT);
        }
        let pending = Content::pending_patch(self.committed.id, self.committed.port.clone(), mask);
        if used.saturating_add(pending.storage()) > budget
        {
            return Err(mac_abi::NO_MEMORY);
        }
        self.pending = Some(pending);
        Ok(())
    }

    pub fn commit_mask(&mut self, used: usize, budget: usize) -> Result<Region, Status>
    {
        if !self.pending.as_ref().is_some_and(Content::is_patch)
        {
            return Err(BAD_ARGUMENT);
        }
        self.committed.commit_patch(self.pending.take().unwrap(), self.bounds, used, budget)
    }

    pub fn cancel_mask(&mut self) -> Result<(), Status>
    {
        if !self.pending.as_ref().is_some_and(Content::is_patch)
        {
            return Err(BAD_ARGUMENT);
        }
        self.cancel_update()
    }

    pub fn end_update(&mut self) -> Result<(), Status>
    {
        if self.pending.as_ref().is_some_and(Content::is_patch)
        {
            return Err(BAD_ARGUMENT);
        }
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
    fn command(bounds: Rect, color: u32) -> RenderCommand
    {
        RenderCommand::Legacy(alloc::boxed::Box::new(DrawCommand { kind: PAINT_RECT,
            bounds,color,text_length:0,text:[0;mac_abi::INLINE_STRING_BYTES] }))
    }
    #[test]
    fn patches_replace_history_preserve_holes_and_skip_unaffected_commands()
    {
        let bounds = Rect::new(0, 0, 64, 48);
        let mut scene = Content::new(WindowId(1));
        scene.commands.push(command(bounds, 0xff0000));
        let mask =
            Region::from_rect(Rect::new(4, 4, 12, 12)).combine(&Region::from_rect(Rect::new(24,
                                                                                            4,
                                                                                            32,
                                                                                            12)),
                                                               BooleanOp::Union);
        let mut maximum = 0;
        for iteration in 0..200
        {
            let mut patch =
                Content::pending_patch(scene.id, service::Port::default(), mask.clone());
            patch.commands.push(command(bounds, iteration));
            let used = scene.storage() + patch.storage();
            scene.commit_patch(patch, bounds, used, 1024 * 1024).unwrap();
            assert_eq!(scene.fragments.len(), 2, "same mask must replace its earlier patch");
            let mut pixels = vec![0; 64 * 48];
            scene.paint(&mut Canvas::new(&mut pixels, 64, 48), None, paint_legacy);
            for y in 0..48
            {
                for x in 0..64
                {
                    assert_eq!(pixels[y * 64 + x],
                               if mask.contains(Point { h: x as i16,
                                                        v: y as i16 })
                               {
                                   iteration
                               }
                               else
                               {
                                   0xff0000
                               });
                }
            }
            maximum = maximum.max(scene.storage());
        }
        assert!(maximum < 4096, "repeat gestures must not grow retained history");
        let mut pixels = vec![0; 64 * 48];
        let mut canvas = Canvas::new(&mut pixels, 64, 48);
        canvas.set_clip(Rect::new(4, 4, 12, 12));
        let mut replays = 0;
        scene.paint(&mut canvas, None, |canvas, c| {
                 replays += 1;
                 paint_legacy(canvas, c);
             });
        assert_eq!(replays, 1, "a local patch must not replay the old background");
        let mut rejected = Content::pending_patch(scene.id, service::Port::default(), mask);
        rejected.commands.push(command(bounds, 0));
        let before = scene.storage();
        assert_eq!(scene.commit_patch(rejected, bounds, before, 0), Err(mac_abi::NO_MEMORY));
        assert_eq!(scene.storage(), before);
    }
    #[test]
    fn desktop_masks_are_atomic_cancelable_bounded_and_preserve_transparency()
    {
        let bounds = Rect::new(0, 21, 64, 48);
        let mut layer = DesktopLayer::new(bounds);
        layer.begin_update().unwrap();
        rectangle(&mut layer, bounds, 0xff0000);
        layer.end_update().unwrap();
        let mask = Region::from_rect(Rect::new(10, 25, 30, 40));
        layer.begin_mask(mask.clone(), layer.storage(), 1 << 20).unwrap();
        rectangle(&mut layer, bounds, 0x00ff00);
        assert_eq!(pixel(&layer), 0xff0000);
        layer.cancel_mask().unwrap();
        assert_eq!(pixel(&layer), 0xff0000);
        for _ in 0..200
        {
            layer.begin_mask(mask.clone(), layer.storage(), 1 << 20).unwrap();
            rectangle(&mut layer, bounds, 0x00ff00);
            layer.commit_mask(layer.storage(), 1 << 20).unwrap();
            assert_eq!(pixel(&layer), 0x00ff00);
        }
        assert_eq!(layer.committed.fragments.len(), 2);
        // An empty masked scene removes old ink and exposes the background.
        layer.begin_mask(mask, layer.storage(), 1 << 20).unwrap();
        layer.commit_mask(layer.storage(), 1 << 20).unwrap();
        assert_eq!(pixel(&layer), 0x878787);
        let mut pixels = vec![0x878787; 64 * 48];
        layer.paint(&mut Canvas::new(&mut pixels, 64, 48), None, paint_legacy);
        assert_eq!(pixels[30 * 64 + 40], 0xff0000);
        assert_eq!(pixels[4 * 64 + 40], 0x878787);
        layer.begin_update().unwrap();
        assert_eq!(layer.cancel_mask(), Err(BAD_ARGUMENT));
        layer.cancel_update().unwrap();
        assert_eq!(layer.begin_mask(Region::from_rect(Rect::new(0, 0, 2, 2)), 0, 1 << 20),
                   Err(BAD_ARGUMENT));
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
        let mut call = GraphicsCall { target: Target::window(WindowId(0)),
                                      operation: QD_DESKTOP_BEGIN,
                                      ..Default::default() };
        assert_eq!(layer.request(&mut call), Err(BAD_ARGUMENT));
        call.target = Target::desktop();
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
        assert!(windows.dispose_window(id, &mut events, 1).is_ok());
        pixels.fill(0x878787);
        layer.paint(&mut Canvas::new(&mut pixels, 640, 480), None, paint_legacy);
        assert_eq!(pixels[120 * 640 + 150], 0xabcdef);
    }
}

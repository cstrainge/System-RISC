//! Retained drawing shared by window content and the window content.
#![forbid(unsafe_code)]
use alloc::{string::String, sync::Arc, vec::Vec};

use mac_abi::{BAD_ARGUMENT, DrawCommand, geometry::{Point, Rect}, Status, WindowId};

use super::{Canvas,
            fonts::FontManager,
            fonts_service,
            region::{BooleanOp, Region},
            service};

/// Retained scenes have an explicit kind, independent of object token bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneTarget
{
    Window(WindowId)
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
            mac_abi::quickdraw::DrawingTarget::Window(id) => Ok(Self::Window(id)),
            mac_abi::quickdraw::DrawingTarget::World(_) => Err(BAD_ARGUMENT)
        }
    }

    pub fn window(self) -> Result<WindowId, Status>
    {
        match self
        {
            Self::Window(id) => Ok(id)
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
             -> Result<(), Status>
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
            Self::Control(c) => c.paint_clipped(canvas)?,
            Self::ScrollBox(c) => c.paint(canvas)?,
            Self::Graphics(d) =>
            {
                d.paint(canvas)?;
            }
            Self::Bitmap(d) =>
            {
                d.paint(canvas)?;
            }
            Self::Icon(d) =>
            {
                d.paint(canvas)?;
            }
            Self::Picture(d) =>
            {
                d.paint(canvas, fonts.ok_or(mac_abi::NOT_FOUND)?)?;
            }
            Self::Image(d) => d.paint(canvas),
            Self::Text { font,
                         baseline,
                         text,
                         color,
                         port,
                         vertical_ink } =>
            {
                let mut result = Ok(());
                {
                    service::with_clip(canvas, port, |canvas| {
                        if result.is_err()
                        {
                            return;
                        }
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
                        let Some(fonts) = fonts
                        else
                        {
                            result = Err(mac_abi::NOT_FOUND);
                            return;
                        };
                        result = fonts.draw(fonts_service::selection(*font),
                                            canvas,
                                            *baseline,
                                            text.as_str(),
                                            *color)
                                      .map(|_| ());
                    });
                }
                result?;
            }
        }
        Ok(())
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

    /// Admit command storage before publishing an additional retained drawing.
    pub fn record(&mut self, command: RenderCommand) -> Result<(), Status>
    {
        self.commands.try_reserve(1).map_err(|_| mac_abi::NO_MEMORY)?;
        self.commands.push(command);
        Ok(())
    }

    pub fn pending_patch(id: impl Into<SceneTarget>, port: service::Port, patch: Region) -> Self
    {
        Self { port,
               patch: Some(patch),
               ..Self::new(id) }
    }

    pub(crate) fn patch_mask(&self) -> Option<&Region>
    {
        self.patch.as_ref()
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
                       -> Result<Option<Region>, Status>
    {
        fn damage(commands: &[RenderCommand], mask: Option<&Region>,
                  editor: mac_abi::textedit::EditorId, before: mac_abi::textedit::EditorInfo,
                  total: &mut Region, found: &mut bool)
                  -> Result<(), Status>
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
                    let mut region = Region::from_rect(*bounds);
                    if let Some(clip) = &port.state.clip
                    {
                        region = region.combine(clip, BooleanOp::Intersection)
                                       .map_err(super::regions_service::status)?;
                    }
                    region = region.offset(port.origin.h, port.origin.v)
                                   .map_err(super::regions_service::status)?;
                    if let Some(mask) = mask
                    {
                        region = region.combine(mask, BooleanOp::Intersection)
                                       .map_err(super::regions_service::status)?;
                    }
                    *total = total.combine(&region, BooleanOp::Union)
                                  .map_err(super::regions_service::status)?;
                    *found = true;
                }
            }
            Ok(())
        }
        let mut total = Region::new();
        let mut found = false;
        for fragment in &self.fragments
        {
            damage(&fragment.commands,
                   Some(&fragment.clip),
                   editor,
                   before,
                   &mut total,
                   &mut found)?;
        }
        damage(&self.commands, None, editor, before, &mut total, &mut found)?;
        if !found
        {
            return Ok(None);
        }
        // All geometry is admitted before publishing any caret snapshot change.
        for command in self.iter_commands_mut()
        {
            if let RenderCommand::Caret { editor: id,
                                          info,
                                          .. } = command
               && *id == editor
               && *info == before
            {
                *info = after;
            }
        }
        Ok(Some(total))
    }

    pub fn retain_commands(&mut self, mut keep: impl FnMut(&RenderCommand) -> bool)
    {
        for fragment in &mut self.fragments
        {
            fragment.commands.retain(&mut keep);
        }
        self.commands.retain(keep);
    }

    /// Refresh retained tree projections only when membership/geometry/drawing
    /// changes. No ownership lives here: order and clipping come from CTRL.
    /// Fragment masks are unchanged, preserving partial-update isolation.
    pub(crate) fn sync_control_tree(&mut self,
                                    controls: &crate::toolbox::controls::objects::Manager,
                                    mut damage: impl FnMut(Rect))
    {
        fn sync(commands: &mut [RenderCommand],
                controls: &crate::toolbox::controls::objects::Manager,
                damage: &mut impl FnMut(Rect))
        {
            for command in commands.iter_mut()
            {
                if let RenderCommand::Control(d) = command
                {
                    if d.id == 0 || !controls.live(d.id)
                    {
                        continue;
                    }
                    let before = d.visible_bounds();
                    d.set_visible(controls.visible_clip(d.id));
                    let after = d.visible_bounds();
                    if before != after
                    {
                        damage(before);
                        damage(after);
                    }
                }
            }
            // In-place insertion order over tree-control command slots only.
            // Raw drawing and non-tree controls keep their original slots.
            for index in 0..commands.len()
            {
                let RenderCommand::Control(d) = &commands[index]
                else
                {
                    continue;
                };
                if !controls.in_tree(d.id)
                {
                    continue;
                }
                let mut current = index;
                loop
                {
                    let Some(previous) = (0..current).rev().find(|i| {
                        matches!(&commands[*i], RenderCommand::Control(d) if controls.in_tree(d.id))
                    }) else { break; };
                    let (RenderCommand::Control(a), RenderCommand::Control(b)) =
                        (&commands[previous], &commands[current])
                    else
                    {
                        break;
                    };
                    if !controls.paint_order(a.id, b.id).is_gt()
                    {
                        break;
                    }
                    damage(a.visible_bounds());
                    damage(b.visible_bounds());
                    commands.swap(previous, current);
                    current = previous;
                }
            }
        }
        for fragment in &mut self.fragments
        {
            sync(&mut fragment.commands, controls, &mut damage);
        }
        sync(&mut self.commands, controls, &mut damage);
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
            clips.push((index,
                        fragment.clip
                                .combine(&mask, BooleanOp::Difference)
                                .map_err(super::regions_service::status)?));
        }
        let tail = (!self.commands.is_empty()).then(|| {
                                                  Region::from_rect(bounds).combine(&mask,
                                                                             BooleanOp::Difference)
                                              })
                                              .transpose()
                                              .map_err(super::regions_service::status)?;
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
                 -> Result<(), Status>
    {
        for fragment in &self.fragments
        {
            let mut result = Ok(());
            service::with_region_clip(canvas, &fragment.clip, |canvas| {
                for command in &fragment.commands
                {
                    if result.is_err()
                    {
                        break;
                    }
                    result = command.paint(canvas, fonts, &mut legacy);
                }
            });
            result?;
        }
        for command in &self.commands
        {
            command.paint(canvas, fonts, &mut legacy)?;
        }
        Ok(())
    }
}

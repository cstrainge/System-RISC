//! Retained drawing shared by window content and the window content.
#![forbid(unsafe_code)]
use alloc::{string::String, sync::Arc, vec::Vec};
use core::ops::{Deref, DerefMut, Range};

use mac_abi::{BAD_ARGUMENT, DrawCommand, Status, WindowId,
              geometry::{Point, Rect}};

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

/// The original command array and its optional affine frame ownership move
/// together, including when patches move arrays into fragments. Borrow only
/// slices; mutation must preserve this SAME buffer and its backing.
#[derive(Default)]
pub struct Commands
{
    values:  Vec<RenderCommand>,
    backing: Option<CommandBacking>
}
enum CommandBacking
{
    Core(CoreBacking),
    Report
    {
        lease: crate::recovery::ReportFrameLease,
        limit: usize,
        text:  ReportTextStock
    }
}
/// Inert report-only command/text stock. Commands destroy immutable readers
/// before parking this SAME allocation; sources mutate only while unique.
pub(crate) struct ReportFrame
{
    commands: Vec<RenderCommand>,
    limit:    usize,
    text:     ReportTextStock
}
struct ReportTextStock
{
    sources: Vec<Arc<String>>,
    count:   usize,
    bytes:   usize,
    used:    usize
}
impl ReportTextStock
{
    fn prepare(count: usize, bytes: usize) -> Result<Self, Status>
    {
        let mut sources = Vec::new();
        sources.try_reserve_exact(count).map_err(|_| mac_abi::NO_MEMORY)?;
        for _ in 0..count
        {
            let mut text = String::new();
            text.try_reserve_exact(bytes).map_err(|_| mac_abi::NO_MEMORY)?;
            sources.push(Arc::try_new(text).map_err(|_| mac_abi::NO_MEMORY)?);
        }
        Ok(Self { sources,
                  count,
                  bytes,
                  used: 0 })
    }

    fn unique(&mut self) -> Result<(), Status>
    {
        if self.count == 0
           || self.sources.len() != self.count
           || self.bytes == 0
           || self.used > self.count
        {
            return Err(mac_abi::BAD_ABI);
        }
        for source in &mut self.sources
        {
            if source.capacity() < self.bytes || source.len() > self.bytes
            {
                return Err(mac_abi::BAD_ABI);
            }
            Arc::get_mut(source).ok_or(mac_abi::SERVICE_BUSY)?;
        }
        Ok(())
    }

    fn snapshot(&mut self, text: &str) -> Result<TextData, Status>
    {
        if text.len() > self.bytes
        {
            return Err(mac_abi::NO_MEMORY);
        }
        let source = self.sources.get_mut(self.used).ok_or(mac_abi::NO_MEMORY)?;
        if source.capacity() < self.bytes
        {
            return Err(mac_abi::BAD_ABI);
        }
        let value = Arc::get_mut(source).ok_or(mac_abi::SERVICE_BUSY)?;
        value.clear();
        value.push_str(text);
        let range = 0..value.len();
        value.get(range.clone()).ok_or(mac_abi::BAD_ABI)?;
        self.used += 1;
        Ok(TextData::Shared { source: source.clone(),
                              range })
    }
}
impl ReportFrame
{
    pub(crate) fn prepare(limit: usize, text_count: usize, text_bytes: usize)
                          -> Result<Self, Status>
    {
        if limit == 0 || limit > 512 || text_count == 0 || text_count > limit || text_bytes == 0
        {
            return Err(mac_abi::BAD_ABI);
        }
        let mut commands = Vec::new();
        commands.try_reserve_exact(limit).map_err(|_| mac_abi::NO_MEMORY)?;
        let text = ReportTextStock::prepare(text_count, text_bytes)?;
        Ok(Self { commands,
                  limit,
                  text })
    }

    pub(crate) fn validate(&mut self, limit: usize) -> Result<(), Status>
    {
        if !self.commands.is_empty() || self.limit != limit || self.commands.capacity() < limit
        {
            return Err(mac_abi::BAD_ABI);
        }
        self.text.unique()
    }

    pub(crate) fn reset_text(&mut self)
    {
        // Caller preflighted every original source before taking this frame.
        // Each next slot is filled fully before its first immutable clone.
        self.text.used = 0;
    }
}
struct CoreBacking
{
    lease: crate::recovery::CoreFrameLease,
    text:  Vec<CoreTextSnapshot>
}
struct CoreTextSnapshot
{
    item:     Option<usize>,
    source:   Arc<String>,
    port:     Arc<service::Port>,
    captured: bool
}
/// Inert allocation stock. No Activity, Window identity or owner survives here.
pub(crate) struct CoreFrame
{
    commands: Vec<RenderCommand>,
    text:     Vec<CoreTextSnapshot>
}
pub(crate) struct CoreFramePair(pub(crate) [CoreFrame; 2]);

impl CoreFrame
{
    pub(crate) fn prepare(items: usize) -> Result<Self, Status>
    {
        let mut commands = Vec::new();
        commands.try_reserve_exact(512).map_err(|_| mac_abi::NO_MEMORY)?;
        let mut text = Vec::new();
        text.try_reserve_exact(items).map_err(|_| mac_abi::NO_MEMORY)?;
        for _ in 0..items
        {
            let mut source = String::new();
            source.try_reserve_exact(mac_abi::INLINE_STRING_BYTES)
                  .map_err(|_| mac_abi::NO_MEMORY)?;
            text.push(CoreTextSnapshot { item: None,
                source: Arc::try_new(source).map_err(|_| mac_abi::NO_MEMORY)?,
                port: Arc::try_new(service::Port::default()).map_err(|_| mac_abi::NO_MEMORY)?,
                captured: false });
        }
        Ok(Self { commands,
                  text })
    }

    pub(crate) fn unique(&mut self) -> Result<(), Status>
    {
        if !self.commands.is_empty() || self.commands.capacity() < 512
        {
            return Err(mac_abi::BAD_ABI);
        }
        for item in &mut self.text
        {
            Arc::get_mut(&mut item.source).ok_or(mac_abi::SERVICE_BUSY)?;
            Arc::get_mut(&mut item.port).ok_or(mac_abi::SERVICE_BUSY)?;
        }
        Ok(())
    }
}

/// Source and Port read pins precede Activity's final-release wake in drop order.
/// Only the actual pending frame can construct this native internal view.
pub(crate) struct CoreTextItem
{
    source:   Arc<String>,
    port:     Arc<service::Port>,
    activity: crate::recovery::Activity,
    window:   WindowId
}
impl CoreTextItem
{
    pub(crate) fn text(&self) -> &str
    {
        &self.source
    }

    pub(crate) fn line(&self, range: Range<usize>) -> Result<&str, Status>
    {
        self.source.get(range).ok_or(mac_abi::BAD_ABI)
    }
}

impl Commands
{
    pub(crate) fn record_core_line(&mut self, item: &CoreTextItem,
                                   call: &mac_abi::quickdraw::FontCall, range: Range<usize>,
                                   vertical_ink: Option<(i32, i32)>, window: WindowId,
                                   port: &service::Port, activity: &crate::recovery::Activity)
                                   -> Result<(), Status>
    {
        if !self.validates_core_item(item, window, port, activity)
        {
            return Err(mac_abi::ACCESS_DENIED);
        }
        item.line(range.clone())?;
        if self.values.len() >= 512
        {
            return Err(mac_abi::NO_MEMORY);
        }
        if self.values.capacity() <= self.values.len()
        {
            return Err(mac_abi::BAD_ABI);
        }
        self.values.push(RenderCommand::Text { font: call.selection,
                                               baseline: call.baseline,
                                               text: TextData::Shared { source: item.source
                                                                                    .clone(),
                                                                        range },
                                               color: call.color,
                                               port: item.port.clone(),
                                               vertical_ink });
        Ok(())
    }

    pub(crate) fn from_core(frame: CoreFrame, lease: crate::recovery::CoreFrameLease) -> Self
    {
        Self { values:  frame.commands,
               backing: Some(CommandBacking::Core(CoreBacking { lease,
                                                                text: frame.text })) }
    }

    pub(crate) fn from_report(frame: ReportFrame, lease: crate::recovery::ReportFrameLease)
                              -> Self
    {
        Self { values:  frame.commands,
               backing: Some(CommandBacking::Report { lease,
                                                      limit: frame.limit,
                                                      text: frame.text }) }
    }

    fn is_report(&self) -> bool
    {
        matches!(self.backing, Some(CommandBacking::Report { .. }))
    }

    fn report_binding(&self, activity: &crate::recovery::Activity, window: WindowId) -> bool
    {
        matches!(&self.backing, Some(CommandBacking::Report { lease, .. })
                 if lease.matches(activity, window))
    }

    pub(crate) fn capture_report_port(&self, activity: &crate::recovery::Activity,
                                      window: WindowId, kind: crate::recovery::ReportPortKind,
                                      actual: &service::Port)
                                      -> Result<crate::recovery::ReportPortView, Status>
    {
        let Some(CommandBacking::Report { lease,
                                          .. }) = &self.backing
        else
        {
            return Err(mac_abi::ACCESS_DENIED);
        };
        lease.capture_port(activity, window, kind, actual)
    }

    pub(crate) fn report_text(&mut self, view: &crate::recovery::ReportPortView, window: WindowId,
                              actual: &service::Port, value: &str)
                              -> Result<(TextData, Arc<service::Port>), Status>
    {
        let Some(CommandBacking::Report { lease,
                                          text,
                                          .. }) = &mut self.backing
        else
        {
            return Err(mac_abi::ACCESS_DENIED);
        };
        // Keep the original per-call order: metrics/admission happened in the
        // writer, then copy text, then prepare the exact immutable Port.
        let source = text.snapshot(value)?;
        let port = lease.text_port(view, window, actual)?;
        Ok((source, port))
    }

    fn admit_additional(&mut self, additional: usize) -> Result<(), Status>
    {
        let limit = match &self.backing
        {
            Some(CommandBacking::Report { limit,
                                          .. }) => Some(*limit),
            Some(CommandBacking::Core(_)) => Some(512),
            None => None
        };
        if limit.is_some_and(|limit| self.values.capacity() < limit)
        {
            return Err(mac_abi::BAD_ABI);
        }
        let end = self.values.len().checked_add(additional).ok_or(mac_abi::NO_MEMORY)?;
        if limit.is_some_and(|limit| end > limit)
        {
            return Err(mac_abi::NO_MEMORY);
        }
        if self.backing.is_none()
        {
            self.values.try_reserve(additional).map_err(|_| mac_abi::NO_MEMORY)?;
        }
        Ok(())
    }

    fn try_push(&mut self, command: RenderCommand) -> Result<(), Status>
    {
        self.admit_additional(1)?;
        self.values.push(command);
        Ok(())
    }

    /// Admit the complete prepared batch before changing the original commands.
    /// Backed frames keep their SAME allocation; only ordinary carriers grow.
    fn try_extend(&mut self, commands: Vec<RenderCommand>) -> Result<(), Status>
    {
        self.admit_additional(commands.len())?;
        for command in commands
        {
            self.values.push(command);
        }
        Ok(())
    }

    fn clear(&mut self)
    {
        self.values.clear();
    }

    fn retain(&mut self, keep: impl FnMut(&RenderCommand) -> bool)
    {
        self.values.retain(keep);
    }

    pub(crate) fn is_core(&self) -> bool
    {
        matches!(self.backing, Some(CommandBacking::Core(_)))
    }

    pub(crate) fn core_binding(&self, activity: &crate::recovery::Activity, window: WindowId)
                               -> bool
    {
        matches!(&self.backing, Some(CommandBacking::Core(core))
                 if core.lease.matches(activity, window))
    }

    pub(crate) fn prepare_core(&mut self, window: WindowId, alert: &super::super::dialogs::Alert,
                               activity: &crate::recovery::Activity)
                               -> Result<(), Status>
    {
        let Some(CommandBacking::Core(core)) = self.backing.as_mut()
        else
        {
            return Err(mac_abi::BAD_ABI);
        };
        if !core.lease.matches(activity, window) || !self.values.is_empty()
        {
            return Err(mac_abi::ACCESS_DENIED);
        }
        let count = alert.items.iter().filter(|item| item.kind == 8).count();
        if count > core.text.len()
        {
            return Err(mac_abi::BAD_ABI);
        }
        // Admit every source/Port before changing any snapshot.
        for item in &mut core.text
        {
            Arc::get_mut(&mut item.source).ok_or(mac_abi::SERVICE_BUSY)?;
            Arc::get_mut(&mut item.port).ok_or(mac_abi::SERVICE_BUSY)?;
        }
        for (snapshot, (_, item)) in
            core.text.iter().zip(alert.items.iter().enumerate().filter(|(_, item)| item.kind == 8))
        {
            if item.text.len() > mac_abi::INLINE_STRING_BYTES
               || snapshot.source.capacity() < item.text.len()
            {
                return Err(mac_abi::BAD_ABI);
            }
        }
        for snapshot in &mut core.text
        {
            snapshot.item = None;
            snapshot.captured = false;
        }
        for (snapshot, (index, item)) in
            core.text
                .iter_mut()
                .zip(alert.items.iter().enumerate().filter(|(_, item)| item.kind == 8))
        {
            let source = Arc::get_mut(&mut snapshot.source).ok_or(mac_abi::SERVICE_BUSY)?;
            source.clear();
            source.push_str(&item.text);
            snapshot.item = Some(index);
        }
        Ok(())
    }

    pub(crate) fn core_item(&mut self, window: WindowId, index: usize, port: &service::Port,
                            activity: &crate::recovery::Activity)
                            -> Result<CoreTextItem, Status>
    {
        let Some(CommandBacking::Core(core)) = self.backing.as_mut()
        else
        {
            return Err(mac_abi::BAD_ABI);
        };
        if !core.lease.matches(activity, window)
        {
            return Err(mac_abi::ACCESS_DENIED);
        }
        let item =
            core.text.iter_mut().find(|item| item.item == Some(index)).ok_or(mac_abi::BAD_ABI)?;
        if item.captured
        {
            return Err(mac_abi::BAD_ABI);
        }
        *Arc::get_mut(&mut item.port).ok_or(mac_abi::SERVICE_BUSY)? = port.clone();
        item.captured = true;
        Ok(CoreTextItem { source: item.source.clone(),
                          port: item.port.clone(),
                          activity: activity.clone(),
                          window })
    }

    fn validates_core_item(&self, item: &CoreTextItem, window: WindowId, port: &service::Port,
                           activity: &crate::recovery::Activity)
                           -> bool
    {
        self.backing.as_ref().is_some_and(|backing| {
                                 let CommandBacking::Core(core) = backing
                                 else
                                 {
                                     return false;
                                 };
                                 core.lease.matches(activity, window)
                                 && item.window == window
                                 && item.activity.same_session(activity)
                                 && item.port.origin == port.origin
                                 && item.port.state == port.state
                                 && core.text.iter().any(|snapshot| {
                                                        snapshot.captured
                                                        && Arc::ptr_eq(&snapshot.source,
                                                                       &item.source)
                                                        && Arc::ptr_eq(&snapshot.port, &item.port)
                                                    })
                             })
    }
}
impl Deref for Commands
{
    type Target = [RenderCommand];

    fn deref(&self) -> &Self::Target
    {
        &self.values
    }
}
impl DerefMut for Commands
{
    fn deref_mut(&mut self) -> &mut Self::Target
    {
        &mut self.values
    }
}
impl Drop for Commands
{
    fn drop(&mut self)
    {
        if let Some(backing) = self.backing.take()
        {
            // Drop every retained text/Port/control reader before parking the
            // SAME allocation and releasing its genuine Activity wake pin.
            self.values.clear();
            match backing
            {
                CommandBacking::Core(core) =>
                {
                    core.lease.park(CoreFrame { commands: core::mem::take(&mut self.values),
                                                text:     core.text })
                }
                CommandBacking::Report { lease,
                                         limit,
                                         text } =>
                {
                    lease.park(ReportFrame { commands: core::mem::take(&mut self.values),
                                             limit,
                                             text })
                }
            }
        }
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
    pub commands: Commands,
    pub port:     service::Port,
    fragments:    Vec<Fragment>,
    patch:        Option<Region>,
    pub outline:  Option<Option<Rect>>
}
struct Fragment
{
    clip:     Region,
    commands: Commands
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
               commands:  Commands::default(),
               port:      service::Port::default(),
               fragments: Vec::new(),
               patch:     None,
               outline:   None }
    }

    pub(crate) fn empty_for_recovery_binding(&self) -> bool
    {
        self.commands.is_empty()
        && self.commands.backing.is_none()
        && self.fragments.is_empty()
        && self.patch.is_none()
    }

    pub(crate) fn has_report_commands(&self) -> bool
    {
        // A bound report starts with its owned base; patching moves that
        // SAME base into the first fragment. Ordinary scenes never mix in a
        // report owner. Keep this classification O(1) for all ordinary draws.
        self.commands.is_report() || self.fragments.first().is_some_and(|f| f.commands.is_report())
    }

    pub(crate) fn report_binding(&self, activity: &crate::recovery::Activity, window: WindowId)
                                 -> bool
    {
        self.has_report_commands()
        && self.fragments.iter().all(|f| f.commands.report_binding(activity, window))
        && (self.commands.report_binding(activity, window)
            || (self.commands.backing.is_none() && self.commands.is_empty()))
    }

    /// Admit command storage before publishing an additional retained drawing.
    pub fn record(&mut self, command: RenderCommand) -> Result<(), Status>
    {
        if self.has_report_commands() && !self.commands.is_report()
        {
            // A patch moved the original owner into its fragment. Never grow
            // an unowned tail beside the report's admitted command owners.
            return Err(mac_abi::BAD_ABI);
        }
        self.commands.try_push(command)
    }

    /// Replace a prepared control batch after admitting its complete growth.
    /// The carrier keeps the original Core/Report backing and cannot allocate
    /// after the first live command is removed.
    pub(crate) fn replace_controls(&mut self,
                                   drawings: &mut Vec<crate::toolbox::controls::Drawing>,
                                   mut removed: impl FnMut(&crate::toolbox::controls::Drawing))
                                   -> Result<(), Status>
    {
        if self.has_report_commands() && !self.commands.is_report()
        {
            return Err(mac_abi::BAD_ABI);
        }
        let replaced = self.commands.iter().filter(|command| {
            matches!(command, RenderCommand::Control(old) if drawings.iter().any(|new| new.id == old.id))
        }).count();
        self.commands.admit_additional(drawings.len().saturating_sub(replaced))?;
        self.retain_commands(|command| {
                if let RenderCommand::Control(old) = command
                   && drawings.iter().any(|new| new.id == old.id)
                {
                    removed(old);
                    false
                }
                else
                {
                    true
                }
            });
        for drawing in drawings.drain(..)
        {
            self.commands.values.push(RenderCommand::Control(drawing));
        }
        Ok(())
    }

    /// Append a prepared resident batch through the same carrier checks as a
    /// single record. An unbacked report tail cannot acquire a new allocation.
    pub(crate) fn try_extend(&mut self, commands: Vec<RenderCommand>) -> Result<(), Status>
    {
        if self.has_report_commands() && !self.commands.is_report()
        {
            return Err(mac_abi::BAD_ABI);
        }
        self.commands.try_extend(commands)
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
        self.fragments.iter().flat_map(|f| f.commands.iter()).chain(self.commands.iter())
    }

    pub fn iter_commands_mut(&mut self) -> impl Iterator<Item = &mut RenderCommand>
    {
        self.fragments
            .iter_mut()
            .flat_map(|f| f.commands.iter_mut())
            .chain(self.commands.iter_mut())
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
                for command in fragment.commands.iter()
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
        for command in self.commands.iter()
        {
            command.paint(canvas, fonts, &mut legacy)?;
        }
        Ok(())
    }
}

//! One production traversal for immediate trusted painting and suspended User
//! plans. Positions are meaningful only under the outer compositor's readset.
#![forbid(unsafe_code)]
use mac_abi::{BAD_ABI, SERVICE_BUSY};

use super::*;
use crate::toolbox::controls::{self, paint as control_paint};

#[derive(Clone, Copy, PartialEq, Eq)]
struct Position
{
    fragment: usize,
    band:     usize,
    span:     usize,
    command:  usize,
    tail:     bool
}
impl Default for Position
{
    fn default() -> Self
    {
        Self { fragment: 0,
               band:     0,
               span:     0,
               command:  0,
               tail:     false }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Geometry
{
    width:  usize,
    height: usize,
    origin: Point,
    clip:   Rect
}
impl Geometry
{
    fn of(canvas: &Canvas<'_>) -> Self
    {
        Self { width:  canvas.width,
               height: canvas.height,
               origin: canvas.origin(),
               clip:   canvas.clip() }
    }
}
enum Inner
{
    None,
    Control(control_paint::Cursor),
    ScrollBox(controls::scrollbox::PaintCursor)
}
/// Affine scalar position, not a callback ACK or a canonical admission proof.
pub(crate) struct ContentResume
{
    position: Position,
    inner:    control_paint::Resume
}
pub(crate) struct ContentPaintPlan
{
    plan:     control_paint::PaintPlan,
    position: Position
}
impl ContentPaintPlan
{
    pub(crate) fn into_parts(mut self)
                             -> Result<(control_paint::PaintPlan, ContentResume), Status>
    {
        let inner = self.plan.take_resume()?;
        Ok((self.plan,
            ContentResume { position: self.position,
                            inner }))
    }
}
pub(crate) enum ContentProgress
{
    Complete,
    User(ContentPaintPlan)
}
pub(crate) struct ContentCursor
{
    position: Position,
    inner:    Inner,
    waiting:  Option<Position>,
    geometry: Option<Geometry>,
    target:   Option<SceneTarget>,
    complete: bool
}
impl Default for ContentCursor
{
    fn default() -> Self
    {
        Self { position: Position::default(),
               inner:    Inner::None,
               waiting:  None,
               geometry: None,
               target:   None,
               complete: false }
    }
}
impl ContentCursor
{
    /// The outer compositor clears its background only before this first turn.
    pub(crate) fn started(&self) -> bool
    {
        self.geometry.is_some()
    }

    /// The caller must first apply the real producer's reply under the same
    /// validated readset/claim. This moves no Pending, ticket or physical owner.
    pub(crate) fn advance_user(&mut self, resume: ContentResume) -> Result<(), Status>
    {
        if self.waiting != Some(resume.position) || self.position != resume.position
        {
            return Err(BAD_ABI);
        }
        match &mut self.inner
        {
            Inner::Control(cursor) => cursor.advance_user(resume.inner)?,
            Inner::ScrollBox(cursor) => cursor.advance_user(resume.inner)?,
            Inner::None => return Err(BAD_ABI)
        }
        self.waiting = None;
        Ok(())
    }

    fn command_done(&mut self) -> Result<(), Status>
    {
        self.position.command = self.position.command.checked_add(1).ok_or(BAD_ABI)?;
        self.inner = Inner::None;
        Ok(())
    }

    fn next_span(&mut self) -> Result<(), Status>
    {
        self.position.span = self.position.span.checked_add(1).ok_or(BAD_ABI)?;
        self.position.command = 0;
        self.inner = Inner::None;
        Ok(())
    }
}
fn set_clip(canvas: &mut Canvas<'_>, clip: Rect)
{
    let origin = canvas.origin();
    canvas.set_origin(0, 0);
    canvas.set_clip(clip);
    canvas.set_origin(origin.h, origin.v);
}
impl Content
{
    /// The ordinary pixel cache cannot substitute for any User author in this
    /// pass, including Table auxiliary sources or a ScrollBox's bar.
    pub(crate) fn has_user_sources(&self) -> bool
    {
        self.iter_commands().any(|command| {
                                match command
                                {
                                    RenderCommand::Control(drawing) => drawing.has_user_sources(),
                                    RenderCommand::ScrollBox(drawing) => drawing.has_user_sources(),
                                    _ => false
                                }
                            })
    }

    pub(crate) fn paint_step(&self, cursor: &mut ContentCursor, canvas: &mut Canvas,
                             fonts: Option<&FontManager>,
                             mut legacy: impl FnMut(&mut Canvas, &DrawCommand))
                             -> Result<ContentProgress, Status>
    {
        self.paint_step_mode(cursor, canvas, fonts, &mut legacy, true)
    }

    pub(super) fn paint_step_mode(&self, cursor: &mut ContentCursor, canvas: &mut Canvas,
                                  fonts: Option<&FontManager>,
                                  legacy: &mut impl FnMut(&mut Canvas, &DrawCommand), staged: bool)
                                  -> Result<ContentProgress, Status>
    {
        let geometry = Geometry::of(canvas);
        if cursor.waiting.is_some()
        {
            return Err(SERVICE_BUSY);
        }
        if cursor.geometry.is_some_and(|old| old != geometry)
           || cursor.target.is_some_and(|old| old != self.id)
        {
            return Err(SERVICE_BUSY);
        }
        cursor.geometry = Some(geometry);
        cursor.target = Some(self.id);
        if cursor.complete
        {
            return Ok(ContentProgress::Complete);
        }
        loop
        {
            let (commands, clip) = if cursor.position.tail
            {
                (&self.commands, geometry.clip)
            }
            else
            {
                if cursor.position.fragment == self.fragments.len()
                {
                    cursor.position.tail = true;
                    cursor.position.command = 0;
                    continue;
                }
                let fragment = self.fragments.get(cursor.position.fragment).ok_or(BAD_ABI)?;
                let bands = fragment.clip.bands();
                let Some(band) = bands.get(cursor.position.band)
                else
                {
                    cursor.position.fragment =
                        cursor.position.fragment.checked_add(1).ok_or(BAD_ABI)?;
                    cursor.position.band = 0;
                    cursor.position.span = 0;
                    cursor.position.command = 0;
                    continue;
                };
                let origin = geometry.origin;
                if i32::from(band.top) + i32::from(origin.v) >= i32::from(geometry.clip.bottom)
                {
                    cursor.position.band = bands.len();
                    continue;
                }
                if i32::from(band.bottom) + i32::from(origin.v) <= i32::from(geometry.clip.top)
                {
                    cursor.position.band = cursor.position.band.checked_add(1).ok_or(BAD_ABI)?;
                    cursor.position.span = 0;
                    continue;
                }
                let Some(span) = band.spans.get(cursor.position.span)
                else
                {
                    cursor.position.band = cursor.position.band.checked_add(1).ok_or(BAD_ABI)?;
                    cursor.position.span = 0;
                    cursor.position.command = 0;
                    continue;
                };
                if i32::from(span.left) + i32::from(origin.h) >= i32::from(geometry.clip.right)
                {
                    cursor.position.span = band.spans.len();
                    continue;
                }
                if i32::from(span.right) + i32::from(origin.h) <= i32::from(geometry.clip.left)
                {
                    cursor.next_span()?;
                    continue;
                }
                // This is the original with_region_clip coordinate policy.
                let rect = Rect::new(span.left.saturating_add(origin.h),
                                     band.top.saturating_add(origin.v),
                                     span.right.saturating_add(origin.h),
                                     band.bottom.saturating_add(origin.v));
                let mut clip = Rect::ZERO;
                super::super::geometry::sect_rect(geometry.clip, rect, &mut clip);
                if clip.is_empty()
                {
                    cursor.next_span()?;
                    continue;
                }
                (&fragment.commands, clip)
            };
            let Some(command) = commands.get(cursor.position.command)
            else
            {
                if cursor.position.tail
                {
                    cursor.complete = true;
                    return Ok(ContentProgress::Complete);
                }
                cursor.next_span()?;
                continue;
            };
            set_clip(canvas, clip);
            let result = match command
            {
                RenderCommand::Control(drawing) =>
                {
                    if matches!(cursor.inner, Inner::None)
                    {
                        cursor.inner = Inner::Control(control_paint::Cursor::default());
                    }
                    match &mut cursor.inner
                    {
                        Inner::Control(inner) => control_paint::run(drawing, inner, canvas, staged),
                        _ => Err(BAD_ABI)
                    }
                }
                RenderCommand::ScrollBox(drawing) =>
                {
                    if matches!(cursor.inner, Inner::None)
                    {
                        cursor.inner =
                            Inner::ScrollBox(controls::scrollbox::PaintCursor::default());
                    }
                    match &mut cursor.inner
                    {
                        Inner::ScrollBox(inner) => drawing.paint_step_mode(inner, canvas, staged),
                        _ => Err(BAD_ABI)
                    }
                }
                other =>
                {
                    other.paint(canvas, fonts, legacy).map(|()| control_paint::Progress::Complete)
                }
            };
            canvas.set_origin(0, 0);
            canvas.set_clip(geometry.clip);
            canvas.set_origin(geometry.origin.h, geometry.origin.v);
            match result?
            {
                control_paint::Progress::Complete => cursor.command_done()?,
                control_paint::Progress::User(plan) =>
                {
                    cursor.waiting = Some(cursor.position);
                    return Ok(ContentProgress::User(ContentPaintPlan { plan,
                                                                       position:
                                                                           cursor.position }));
                }
            }
        }
    }
}

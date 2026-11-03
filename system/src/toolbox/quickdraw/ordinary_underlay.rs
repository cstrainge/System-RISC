//! Committed ordinary pixels and transactional staging for the normal renderer.
//! An ordinary replay never receives recovery authority. A refused transaction
//! leaves its exact queued regions and the previously committed pixels intact.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_abi::*;

use super::{Canvas, RenderCommand, SceneTarget, Toolbox, WHITE, background, paint};
use crate::toolbox::quickdraw::{geometry::{Rect, sect_rect, union_rect},
                                region::{BooleanOp, Region}};

#[derive(Default)]
pub(super) struct OrdinaryUnderlay
{
    committed:                Vec<u32>,
    staging:                  Vec<u32>,
    pub(super) deferred:      Vec<(SceneTarget, Region)>,
    pub(super) full_deferred: bool,
    changed:                  Vec<Rect>,
    previous_system:          Vec<Rect>,
    valid:                    bool,
    generation:               u64,
    attempted_generation:     u64,
    refused_available:        Option<usize>,
    refusal:                  Option<Status>
}
/// Actual admitted compositor storage; refusal/drop leaves live lengths and
/// pixels unchanged. The exclusive borrow spans the real GPU transaction.
pub(super) struct PreparedPixels<'a>
{
    underlay: &'a mut OrdinaryUnderlay,
    count:    usize
}
impl PreparedPixels<'_>
{
    pub(super) fn commit(self)
    {
        // prepare_mode reserved both exact capacities while this mutable borrow
        // excludes mutation. These resizes cannot allocate or refuse.
        self.underlay.committed.resize(self.count, 0);
        self.underlay.staging.resize(self.count, 0);
        self.underlay.valid = false;
        self.underlay.full_deferred = true;
        self.underlay.previous_system.clear();
        self.underlay.refused_available = None;
        self.underlay.refusal = None;
    }
}
impl OrdinaryUnderlay
{
    pub(super) fn prepare_mode(&mut self, count: usize) -> Result<PreparedPixels<'_>, Status>
    {
        if count == 0 || count > crate::display::MAX_PIXELS
        {
            return Err(BAD_ARGUMENT);
        }
        for pixels in [&mut self.committed, &mut self.staging]
        {
            pixels.try_reserve_exact(count.saturating_sub(pixels.len())).map_err(|_| NO_MEMORY)?;
        }
        Ok(PreparedPixels { underlay: self,
                            count })
    }

    pub(super) fn prepare(&mut self, system_windows: usize, scenes: usize) -> Result<(), Status>
    {
        for pixels in [&mut self.committed, &mut self.staging]
        {
            if pixels.len() != crate::display::pixels()
            {
                pixels.try_reserve_exact(crate::display::pixels().saturating_sub(pixels.len()))
                      .map_err(|_| NO_MEMORY)?;
                pixels.resize(crate::display::pixels(), 0);
            }
        }
        self.previous_system
            .try_reserve_exact(system_windows.saturating_sub(self.previous_system.len()))
            .map_err(|_| NO_MEMORY)?;
        self.changed.try_reserve_exact(1).map_err(|_| NO_MEMORY)?;
        self.prepare_scenes(scenes)
    }

    pub(super) fn prepare_scenes(&mut self, scenes: usize) -> Result<(), Status>
    {
        self.deferred
            .try_reserve_exact(scenes.saturating_sub(self.deferred.len()))
            .map_err(|_| NO_MEMORY)
    }

    pub(super) fn changed(&mut self)
    {
        self.generation = self.generation.wrapping_add(1);
    }

    pub(super) fn pending(&self) -> bool
    {
        self.full_deferred || !self.deferred.is_empty()
    }

    pub(super) fn retry_due(&self, available: usize) -> bool
    {
        self.pending() && self.refused_available.is_none_or(|before| {
            available > before || self.generation != self.attempted_generation
        })
    }

    pub(super) fn queue_if_deferred(&mut self, id: SceneTarget, region: Region)
                                    -> Result<(), Region>
    {
        let Some((_, previous)) = self.deferred.iter_mut().find(|(target, _)| *target == id)
        else
        {
            return Err(region);
        };
        // This is the same bounded metadata-refusal fallback as the ordinary
        // active damage queue. No successfully retained region is discarded.
        *previous = previous.combine(&region, BooleanOp::Union).unwrap_or_else(|_| {
            Region::from_rect(union_rect(previous.bounds(), region.bounds()))
        });
        Ok(())
    }

    pub(super) fn forget(&mut self, id: SceneTarget)
    {
        self.deferred.retain(|(target, _)| *target != id);
    }

    pub(super) fn invert(&mut self, rect: Rect)
    {
        if !self.valid
        {
            return;
        }
        let rect = clip(rect);
        for y in rect.top as usize..rect.bottom as usize
        {
            let start = y * crate::display::width() + rect.left as usize;
            let end = start + (rect.right - rect.left) as usize;
            for pixel in &mut self.committed[start..end]
            {
                *pixel ^= 0x00ff_ffff;
            }
            for pixel in &mut self.staging[start..end]
            {
                *pixel ^= 0x00ff_ffff;
            }
        }
    }
}
fn screen() -> Rect
{
    Rect::new(0, 0, crate::display::width() as i16, crate::display::height() as i16)
}
fn clip(rect: Rect) -> Rect
{
    let mut result = Rect::ZERO;
    sect_rect(rect, screen(), &mut result);
    result
}
fn copy_rect(source: &[u32], destination: &mut [u32], rect: Rect)
{
    let rect = clip(rect);
    for y in rect.top as usize..rect.bottom as usize
    {
        let start = y * crate::display::width() + rect.left as usize;
        let end = start + (rect.right - rect.left) as usize;
        destination[start..end].copy_from_slice(&source[start..end]);
    }
}
impl Toolbox
{
    /// Commit the initial ordinary scene before foreign startup code can exert
    /// pressure. Use the normal renderer on the actual admitted staging lease;
    /// never present pixels, publish an observation or compose a cursor here.
    pub(crate) fn prepare_ordinary_baseline(&mut self) -> Result<(), Status>
    {
        if self.presentation.ordinary_underlay.valid
        {
            return Ok(());
        }
        let mut staging = core::mem::take(&mut self.presentation.ordinary_underlay.staging);
        let result = crate::context::without_recovery(|| {
            crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
                if staging.len() != crate::display::pixels()
                   || self.presentation.ordinary_underlay.committed.len()
                      != crate::display::pixels()
                {
                    return Err(NOT_SUPPORTED);
                }
                let cache_budget = crate::toolbox::quickdraw::content_cache::PIXEL_BUDGET
                    .saturating_sub(self.scroll_boxes.raster_storage())
                    .saturating_sub(crate::toolbox::quickdraw::menu_cache::BYTES);
                self.presentation.content_cache.preserve_headroom(crate::kernel::heap_available(),
                                                                  2 * 1024 * 1024,
                                                                  cache_budget);
                self.paint_ordinary_full(&mut staging)
            })
        });
        if result.is_ok()
        {
            copy_rect(&staging, &mut self.presentation.ordinary_underlay.committed, screen());
            self.presentation.ordinary_underlay.valid = true;
            self.presentation.ordinary_underlay.full_deferred = false;
            self.presentation.ordinary_underlay.deferred.clear();
            self.presentation.ordinary_underlay.refused_available = None;
            self.presentation.ordinary_underlay.refusal = None;
        }
        // A refusal leaves committed state invalid and returns the exact actual
        // staging allocation to its owner; no fallback pixels are fabricated.
        self.presentation.ordinary_underlay.staging = staging;
        result
    }

    pub(super) fn system_scene(&self, id: SceneTarget) -> bool
    {
        id.window()
          .ok()
          .and_then(|id| self.windows.window(id))
          .is_some_and(|window| window.uses_system_resources())
    }

    /// Ordinary scenes keep their existing logical ceiling. Genuine service
    /// scenes have no second arbitrary byte ceiling: their actual allocations
    /// remain bounded by the sealed protected allocator, never by other apps'
    /// retained commands. This is not a protected-pool-fit assertion.
    pub(super) fn retained_admission(&self, id: SceneTarget) -> (usize, usize)
    {
        super::window_layout::scene_admission(&self.windows, &self.presentation.content,
                                              &self.presentation.pending_content, id)
    }

    fn paint_ordinary_full(&mut self, pixels: &mut [u32]) -> Result<(), Status>
    {
        let mut canvas = Canvas::new(pixels, crate::display::width(), crate::display::height());
        background(&mut canvas);
        let font_snapshot = crate::kernel::fonts();
        let fonts = font_snapshot.as_deref();
        let cdef = self.cdef;
        self.presentation.content_cache.begin_frame();
        self.presentation.content_cache.retain(|id| {
                                           id.window()
                                             .ok()
                                             .and_then(|id| self.windows.window(id))
                                             .is_some()
                                           && !self.presentation
                                                   .content
                                                   .iter()
                                                   .find(|content| content.id == id)
                                                   .is_some_and(|content| {
                                                       content.iter_commands().any(|command| {
                                              matches!(command, RenderCommand::ScrollBox(_))
                                          })
                                                   })
                                       });
        let content = &self.presentation.content;
        let cache = &mut self.presentation.content_cache;
        self.windows.draw_layer(
                                 &mut canvas,
                                 |window| !window.uses_system_resources(),
                                 |window, canvas| {
                                     let content =
                                         content.iter().find(|content| content.id == window.id);
                                     let replay = |canvas: &mut Canvas<'_>| {
                                         canvas.paint_rect(window.local_bounds(), WHITE);
                                         if let Some(content) = content
                                         {
                                             content.paint(canvas, fonts, |canvas, command| {
                                                        paint(canvas, command, cdef)
                                                    })?;
                                         }
                                         Ok(())
                                     };
                                     if content.is_some_and(|content| {
                                                   content.iter_commands()
                    .any(|command| matches!(command, RenderCommand::ScrollBox(_)))
                                               })
                                     {
                                         replay(canvas)
                                     }
                                     else
                                     {
                                         cache.try_paint(window.id.0, window.bounds, canvas, replay)
                                     }
                                 }
        )?;
        self.presentation
            .menu_cache
            .paint(&mut canvas,
                   (self.foreground,
                    self.menus.selected_menu(),
                    self.menus.application_selected(),
                    self.menus.system_selected()),
                   |canvas| self.menus.draw_bar(&mut self.system_bar, &mut self.app_menu, canvas))
            .map_err(|error| error.status)?;
        Ok(())
    }

    fn paint_ordinary_damage(&mut self, pixels: &mut [u32], damage: &[(SceneTarget, Region)],
                             deferred: &[(SceneTarget, Region)], changed: &mut Vec<Rect>,
                             copy_full: &mut bool)
                             -> Result<(), Status>
    {
        let scopes = self.windows.resource_scopes();
        let font_snapshot = crate::kernel::fonts();
        let fonts = font_snapshot.as_deref();
        let cdef = self.cdef;
        let cache = &mut self.presentation.content_cache;
        let mut canvas = Canvas::new(pixels, crate::display::width(), crate::display::height());
        let ordinary =
            |window: &crate::toolbox::windows::manager::Window| !window.uses_system_resources();
        for (id, region) in
            deferred.iter().chain(damage).filter(|(id, _)| {
                                             id.window()
                                               .ok()
                                               .and_then(|id| self.windows.window(id))
                                               .is_none_or(|window| !window.uses_system_resources())
                                         })
        {
            let mut result = Ok(());
            if let Ok(window_id) = id.window()
            {
                let Some(window) = self.windows.window(window_id)
                else
                {
                    continue;
                };
                let Some(content) =
                    self.presentation.content.iter().find(|content| content.id == *id)
                else
                {
                    continue;
                };
                let has_viewport =
                    content.iter_commands()
                           .any(|command| matches!(command, RenderCommand::ScrollBox(_)));
                self.windows.for_each_layer_visible_rect(window_id, &ordinary, |visible| {
                                for band in region.bands()
                                {
                                    for span in &band.spans
                                    {
                                        if result.is_err()
                                        {
                                            continue;
                                        }
                                        let mut rect =
                                            Rect::new(span.left, band.top, span.right, band.bottom);
                                        rect.offset(window.bounds.left, window.bounds.top);
                                        let mut hit = Rect::ZERO;
                                        if !sect_rect(rect, visible, &mut hit)
                                        {
                                            continue;
                                        }
                                        canvas.reset_port();
                                        canvas.set_clip(hit);
                                        canvas.set_origin(window.bounds.left, window.bounds.top);
                                        result = scopes.with_window(window, || {
                                                     let replay = |canvas: &mut Canvas<'_>| {
                                                         canvas.paint_rect(window.local_bounds(),
                                                                           WHITE);
                                                         content.paint(canvas,
                                                                       fonts,
                                                                       |canvas, command| {
                                                                           paint(canvas, command,
                                                                                 cdef)
                                                                       })
                                                     };
                                                     if has_viewport
                                                     {
                                                         replay(&mut canvas)?;
                                                     }
                                                     else
                                                     {
                                                         cache.try_paint(window_id.0,
                                                                         window.bounds,
                                                                         &mut canvas, replay)?;
                                                     }
                                                     window.paint_content_overlay(&mut canvas)
                                                 })
                                                 .and_then(core::convert::identity);
                                        super::accumulate_presentation_damage(changed, copy_full,
                                                                              hit);
                                    }
                                }
                            });
            }
            result?;
        }
        Ok(())
    }

    /// The same renderer runs transactionally without recovery authority. Only
    /// complete successful output becomes committed ordinary backing.
    pub(super) fn update_ordinary_underlay(&mut self, damage: &mut Vec<(SceneTarget, Region)>,
                                           full: bool, available: usize,
                                           screen_damage: &mut Vec<Rect>,
                                           screen_copy_full: &mut bool)
                                           -> Result<bool, Status>
    {
        let mut staging = core::mem::take(&mut self.presentation.ordinary_underlay.staging);
        let mut changed = core::mem::take(&mut self.presentation.ordinary_underlay.changed);
        let deferred = core::mem::take(&mut self.presentation.ordinary_underlay.deferred);
        changed.clear();
        let requested_full = full;
        let full = full
                   || self.presentation.ordinary_underlay.full_deferred
                   || !self.presentation.ordinary_underlay.valid;
        let mut copy_full = full;
        let result = if staging.len() != crate::display::pixels()
                        || self.presentation.ordinary_underlay.committed.len()
                           != crate::display::pixels()
        {
            Err(NOT_SUPPORTED)
        }
        else
        {
            crate::context::without_recovery(|| {
                if full
                {
                    self.paint_ordinary_full(&mut staging)
                }
                else
                {
                    self.paint_ordinary_damage(&mut staging, damage, &deferred, &mut changed,
                                               &mut copy_full)
                }
            })
        };
        self.presentation.ordinary_underlay.deferred = deferred;
        if result.is_ok()
        {
            if copy_full
            {
                copy_rect(&staging, &mut self.presentation.ordinary_underlay.committed, screen());
            }
            else
            {
                for &rect in &changed
                {
                    copy_rect(&staging, &mut self.presentation.ordinary_underlay.committed, rect);
                }
            }
            self.presentation.ordinary_underlay.valid = true;
            self.presentation.ordinary_underlay.full_deferred = false;
            self.presentation.ordinary_underlay.deferred.clear();
            self.presentation.ordinary_underlay.refused_available = None;
            self.presentation.ordinary_underlay.refusal = None;
            let windows = &self.windows;
            let committed = &self.presentation.ordinary_underlay.committed;
            let back = crate::platform::back_buffer();
            let mut expose = |rect| {
                windows.for_each_ordinary_underlay_rect(rect, |visible| {
                           super::accumulate_presentation_damage(screen_damage, screen_copy_full,
                                                                 visible);
                           copy_rect(committed, back, visible);
                       })
            };
            if copy_full
            {
                expose(screen());
            }
            else
            {
                for &rect in &changed
                {
                    expose(rect);
                }
            }
            damage.retain(|(id, _)| self.system_scene(*id));
        }
        else
        {
            if self.presentation.ordinary_underlay.valid
               && staging.len() == crate::display::pixels()
            {
                copy_rect(&self.presentation.ordinary_underlay.committed, &mut staging, screen());
                if requested_full
                {
                    // A real structural/full request must remove the prior
                    // transient popup/tracking pixels even if ordinary replay
                    // refuses. System windows and current menus are drawn next.
                    copy_rect(&self.presentation.ordinary_underlay.committed,
                              crate::platform::back_buffer(),
                              screen());
                    *screen_copy_full = true;
                }
            }
            let refusal = result.as_ref().err().copied();
            if self.presentation.ordinary_underlay.refusal != refusal
            {
                if let Some(status) = refusal
                {
                    serial!("ORDINARY_REPLAY_DEFERRED status={status}; committed ordinary pixels \
                             retained");
                }
                self.presentation.ordinary_underlay.refusal = refusal;
            }
            self.presentation.ordinary_underlay.full_deferred |= full;
            self.presentation.ordinary_underlay.refused_available = Some(available);
            self.presentation.ordinary_underlay.attempted_generation =
                self.presentation.ordinary_underlay.generation;
            let mut index = 0;
            while index < damage.len()
            {
                if self.system_scene(damage[index].0)
                {
                    index += 1;
                    continue;
                }
                if self.presentation.ordinary_underlay.deferred.len()
                   == self.presentation.ordinary_underlay.deferred.capacity()
                {
                    break;
                } // Exact owned region stays in the active queue.
                let entry = damage.swap_remove(index);
                self.presentation.ordinary_underlay.deferred.push(entry);
            }
        }
        self.presentation.ordinary_underlay.staging = staging;
        self.presentation.ordinary_underlay.changed = changed;
        result.map(|()| full)
    }

    pub(super) fn paint_system_layer(&mut self, damage: &mut Vec<Rect>, copy_full: &mut bool)
                                     -> Result<(), Status>
    {
        if !self.presentation.ordinary_underlay.valid
        {
            return Err(SERVICE_BUSY);
        }
        let back = crate::platform::back_buffer();
        for &bounds in &self.presentation.ordinary_underlay.previous_system
        {
            super::accumulate_presentation_damage(damage, copy_full, bounds);
            copy_rect(&self.presentation.ordinary_underlay.committed, back, bounds);
        }
        for window in self.windows.back_to_front().filter(|window| window.uses_system_resources())
        {
            super::accumulate_presentation_damage(damage, copy_full, window.structure());
            copy_rect(&self.presentation.ordinary_underlay.committed, back, window.structure());
        }
        let font_snapshot = crate::kernel::fonts();
        let fonts = font_snapshot.as_deref();
        let content = &self.presentation.content;
        let cdef = self.system_cdef;
        let mut canvas = Canvas::new(back, crate::display::width(), crate::display::height());
        self.windows.draw_layer(&mut canvas,
                                 |window| window.uses_system_resources(),
                                 |window, canvas| {
                                     canvas.paint_rect(window.local_bounds(), WHITE);
                                     if let Some(content) =
                                         content.iter().find(|content| content.id == window.id)
                                     {
                                         content.paint(canvas, fonts, |canvas, command| {
                                                    paint(canvas, command, cdef)
                                                })?;
                                     }
                                     Ok(())
                                 })?;
        self.presentation.ordinary_underlay.previous_system.clear();
        for window in self.windows.back_to_front().filter(|window| window.uses_system_resources())
        {
            if self.presentation.ordinary_underlay.previous_system.len()
               == self.presentation.ordinary_underlay.previous_system.capacity()
            {
                return Err(SERVICE_BUSY);
            }
            self.presentation.ordinary_underlay.previous_system.push(window.structure());
        }
        Ok(())
    }

    pub(super) fn paint_system_damage(&mut self, damage: &[(SceneTarget, Region)],
                                      changed: &mut Vec<Rect>, copy_full: &mut bool)
                                      -> Result<(), Status>
    {
        let scopes = self.windows.resource_scopes();
        let font_snapshot = crate::kernel::fonts();
        let fonts = font_snapshot.as_deref();
        let cdef = self.system_cdef;
        let mut canvas = Canvas::new(crate::platform::back_buffer(),
                                     crate::display::width(),
                                     crate::display::height());
        for (id, region) in damage
        {
            let Ok(id) = id.window()
            else
            {
                continue;
            };
            let Some(window) =
                self.windows.window(id).filter(|window| window.uses_system_resources())
            else
            {
                continue;
            };
            let Some(content) = self.presentation.content.iter().find(|content| content.id == id)
            else
            {
                continue;
            };
            let mut result = Ok(());
            self.windows.for_each_visible_rect(id, |visible| {
                            for band in region.bands()
                            {
                                for span in &band.spans
                                {
                                    if result.is_err()
                                    {
                                        continue;
                                    }
                                    let mut rect =
                                        Rect::new(span.left, band.top, span.right, band.bottom);
                                    rect.offset(window.bounds.left, window.bounds.top);
                                    let mut hit = Rect::ZERO;
                                    if !sect_rect(rect, visible, &mut hit)
                                    {
                                        continue;
                                    }
                                    canvas.reset_port();
                                    canvas.set_clip(hit);
                                    canvas.set_origin(window.bounds.left, window.bounds.top);
                                    result =
                                        scopes.with_window(window, || {
                                                  canvas.paint_rect(window.local_bounds(), WHITE);
                                                  content.paint(&mut canvas,
                                                                fonts,
                                                                |canvas, command| {
                                                                    paint(canvas, command, cdef)
                                                                })?;
                                                  window.paint_content_overlay(&mut canvas)
                                              })
                                              .and_then(core::convert::identity);
                                    super::accumulate_presentation_damage(changed, copy_full, hit);
                                }
                            }
                        });
            result?;
        }
        Ok(())
    }
}

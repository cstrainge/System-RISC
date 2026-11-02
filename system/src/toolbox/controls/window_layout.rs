//! One admission boundary for explicit Table assignment and window-root layout.
//! Preparation and retained allocation precede control/window publication.
use super::*;
use crate::toolbox::controls::objects::{Manager, Prepared};

pub(super) fn ordinary_retained_bytes(windows: &WindowManager, content: &[Content],
                                      pending: &[Content])
                                      -> usize
{
    content.iter()
           .chain(pending)
           .filter(|c| {
               !c.id
                 .window()
                 .ok()
                 .and_then(|id| windows.window(id))
                 .is_some_and(|w| w.uses_system_resources())
           })
           .fold(0usize, |bytes, c| bytes.saturating_add(c.storage()))
}

pub(super) fn scene_admission(windows: &WindowManager, content: &[Content], pending: &[Content],
                              id: SceneTarget)
                              -> (usize, usize)
{
    let system_scene = |id: SceneTarget| {
        id.window()
          .ok()
          .and_then(|id| windows.window(id))
          .is_some_and(|w| w.uses_system_resources())
    };
    let system = system_scene(id);
    let used = content.iter()
                      .chain(pending)
                      .filter(|c| system_scene(c.id) == system)
                      .fold(0usize, |bytes, c| bytes.saturating_add(c.storage()));
    if system { (used, usize::MAX) } else { (used, MAX_RETAINED_BYTES) }
}

fn relayout(windows: &WindowManager, content: &mut [Content], pending: &[Content],
            controls: &mut Manager, id: WindowId, viewport: Rect)
            -> Result<Vec<Rect>, Status>
{
    let prepared = controls.prepare_window(id.0,
                                   viewport,
                                   pending.iter().any(|c| c.id == SceneTarget::from(id)),
                                   None,
                                   crate::machine::traps::gateway,
                                   |id| crate::toolbox::runtime::Toolbox::control_definition(id))?;
    let (used, limit) = scene_admission(windows, content, pending, SceneTarget::from(id));
    publish(content, controls, id.0, prepared, used, limit)
}

fn publish(content: &mut [Content], controls: &mut Manager, window: u64, mut prepared: Prepared,
           used: usize, limit: usize)
           -> Result<Vec<Rect>, Status>
{
    let mut damage = Vec::new();
    if prepared.drawings.is_empty()
    {
        controls.commit_layout(prepared);
        return Ok(damage);
    }
    let limited_damage = prepared.limited_damage;
    let drawings = &mut prepared.drawings;
    let content =
        content.iter_mut().find(|c| c.id == SceneTarget::from(window)).ok_or(NOT_FOUND)?;
    let mut removed_bytes = 0usize;
    let mut removed_count = 0usize;
    for command in content.iter_commands()
    {
        if matches!(command, RenderCommand::Control(d) if drawings.iter().any(|new| new.id == d.id))
        {
            removed_bytes = removed_bytes.checked_add(command.storage()).ok_or(NO_MEMORY)?;
            removed_count += 1;
        }
    }
    // A missing retained control needs its complete first drawing, even if
    // the state transition itself changed only a scrollbar highlight.
    let limited_damage = limited_damage.filter(|_| removed_count != 0);
    let mut next_bytes = used.checked_sub(removed_bytes).ok_or(CORRUPT_DATA)?;
    for d in drawings.iter()
    {
        next_bytes = next_bytes.checked_add(d.storage()).ok_or(NO_MEMORY)?;
    }
    let count = content.iter_commands()
                       .count()
                       .checked_sub(removed_count)
                       .and_then(|n| n.checked_add(drawings.len()))
                       .ok_or(NO_MEMORY)?;
    if next_bytes > limit || count > 512
    {
        return Err(NO_MEMORY);
    }
    damage.try_reserve_exact(removed_count.checked_add(drawings.len())
                                          .and_then(|n| n.checked_add(1))
                                          .ok_or(NO_MEMORY)?)
          .map_err(|_| NO_MEMORY)?;
    if limited_damage.is_none()
    {
        for drawing in drawings.iter() { damage.push(drawing.visible_bounds()); }
    }
    // Carrier admission precedes both removal and all infallible appends.
    content.replace_controls(drawings, |drawing| {
        if limited_damage.is_none() { damage.push(drawing.visible_bounds()); }
    })?;
    if let Some(rect) = limited_damage
    {
        damage.push(rect);
    }
    controls.commit_layout(prepared);
    let mut changed_clip = None;
    content.sync_control_tree(controls, |rect| {
               changed_clip = Some(changed_clip.map_or(rect, |old| {
                                       super::super::quickdraw::geometry::union_rect(old, rect)
                                   }));
           });
    if let Some(rect) = changed_clip
    {
        damage.push(rect);
    }
    Ok(damage)
}
impl Toolbox
{
    pub(super) fn publish_control_layout(&mut self, window: u64, prepared: Prepared, used: usize,
                                         limit: usize)
                                         -> Result<(), Status>
    {
        let feedback = prepared.feedback;
        let damage = publish(&mut self.presentation.content, &mut self.controls, window,
                             prepared, used, limit)?;
        let changed = !damage.is_empty() || feedback.is_some();
        if let Some(feedback) = feedback
        {
            self.apply_control_outline(window, feedback)?;
        }
        for rect in damage.into_iter().filter(|r| !r.is_empty())
        {
            self.content_changed_rect(window, rect);
        }
        if changed
        {
            self.present();
        }
        Ok(())
    }

    /// Consume only this caller's pending roots, under its actual service and
    /// allocation authority. A root is one atomic publication; an unrelated
    /// blocked root does not prevent another root from progressing.
    pub(super) fn automatic_control_layout(&mut self)
    {
        // A resident recovery dialog must not spend its protected allocation
        // authority on the application's ordinary deferred layout work.
        if crate::context::recovery_activity().is_some()
        {
            return;
        }
        let now = Self::uptime();
        let owner = self.owner;
        let roots = match self.controls.take_layout_work(owner, now)
        {
            Ok(Some(roots)) => roots,
            Ok(None) | Err(_) => return
        };
        let mut changed = false;
        for root in roots
        {
            let result = (|| -> Result<Vec<Rect>, Status> {
                let window = self.windows.window(WindowId(root.window)).ok_or(NOT_FOUND)?;
                if !self.owns(WindowId(root.window))
                {
                    return Err(ACCESS_DENIED);
                }
                let prepared = self.controls.prepare_automatic(
                    root, owner, window.local_bounds(),
                    self.presentation.pending_content.iter().any(|c| c.id == SceneTarget::from(root.window)),
                    crate::machine::traps::gateway,
                    |id| crate::toolbox::runtime::Toolbox::control_definition(id))?;
                let (used, limit) = self.retained_admission(SceneTarget::from(root.window));
                publish(&mut self.presentation.content, &mut self.controls, root.window, prepared,
                        used, limit)
            })();
            match result
            {
                Ok(damage) =>
                {
                    changed |= !damage.is_empty();
                    for rect in damage.into_iter().filter(|r| !r.is_empty())
                    {
                        self.content_changed_rect(root.window, rect);
                    }
                }
                Err(status) =>
                {
                    if self.controls.layout_refused(root.id, status, now)
                    {
                        serial!("CONTROL_LAYOUT_DEFERRED root={} window={} status={} {}; \
                                 previous geometry retained",
                                root.id,
                                root.window,
                                status,
                                match status
                                {
                                    SERVICE_BUSY =>
                                        "finish painting or release capture; automatic retry \
                                         follows",
                                    NO_MEMORY =>
                                        "layout storage unavailable; automatic retry is rate \
                                         limited",
                                    BAD_ARGUMENT =>
                                        "layout dimensions cannot be represented; correct sizing \
                                         settings",
                                    BAD_ABI =>
                                        "definition contract mismatch; rebuild all native packages",
                                    ACCESS_DENIED => "layout owner authority refused",
                                    _ => "layout preparation failed; correct settings before retry"
                                });
                    }
                }
            }
        }
        self.controls.finish_layout_work(owner);
        if changed
        {
            self.present();
        }
    }

    /// A native wait must return in time to retry a refused layout batch.
    pub(crate) fn control_layout_wait(&self, requested: u32) -> u32
    {
        self.controls.layout_wait_ticks(self.owner, Self::uptime(), requested)
    }

    pub(super) fn track_window(&mut self, point: Point, released: bool, when: u32)
                               -> Option<Action>
    {
        let content = &mut self.presentation.content;
        let pending = &self.presentation.pending_content;
        let controls = &mut self.controls;
        let mut damage = None;
        let result =
            self.windows.track(point, released, &mut self.events, when, |windows, id, viewport| {
                            let prepared =
                                controls.prepare_window(id.0,
                                                        viewport,
                                                        pending.iter()
                                                               .any(|c| {
                                                                   c.id == SceneTarget::from(id)
                                                               }),
                                                        None,
                                                        crate::machine::traps::gateway,
                                                        |id| {
                                                            crate::toolbox::runtime::Toolbox::control_definition(id)
                                                        })?;
                            let (used, limit) = scene_admission(windows,
                                                                content,
                                                                pending,
                                                                SceneTarget::from(id));
                            let changed = publish(content, controls, id.0, prepared, used, limit)?;
                            damage = Some((id, changed));
                            Ok(())
                        });
        self.finish_relayout(result, damage)
    }

    /// Programmatic SizeWindow/MoveWindow: the same staged layout as a user
    /// grow, so a refused layout leaves the window and controls unchanged.
    pub(super) fn set_window_bounds(&mut self, id: WindowId, bounds: Rect) -> Option<Action>
    {
        let content = &mut self.presentation.content;
        let pending = &self.presentation.pending_content;
        let controls = &mut self.controls;
        let mut damage = None;
        let result = self.windows.set_bounds(id, bounds, |windows, id, viewport| {
                                     damage = Some((id,
                                                    relayout(windows, content, pending,
                                                             controls, id, viewport)?));
                                     Ok(())
                                 });
        self.finish_relayout(result, damage)
    }

    fn finish_relayout(&mut self, result: Result<Option<Action>, Status>,
                       damage: Option<(WindowId, Vec<Rect>)>)
                       -> Option<Action>
    {
        match result
        {
            Ok(action) =>
            {
                if let Some((id, damage)) = damage
                {
                    for rect in damage.into_iter().filter(|r| !r.is_empty())
                    {
                        self.content_changed_rect(id, rect);
                    }
                }
                action
            }
            Err(status) =>
            {
                serial!("WINDOW_LAYOUT_REFUSED: status={} {}; window and controls unchanged; \
                         release cancelled, retry available",
                        status,
                        match status
                        {
                            NO_MEMORY => "layout resources could not be admitted",
                            SERVICE_BUSY =>
                                "finish painting or cancel active control tracking before retrying",
                            BAD_ARGUMENT => "layout dimensions cannot be represented",
                            ACCESS_DENIED => "layout owner authority was refused",
                            BAD_ABI => "definition contract mismatch; rebuild all native packages",
                            _ => "layout preparation failed"
                        });
                None
            }
        }
    }
}

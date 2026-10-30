//! One admission boundary for explicit Table assignment and window-root layout.
//! Preparation and retained allocation precede control/window publication.
use super::*;
use crate::toolbox::controls::objects::{Manager, Prepared};

pub(super) fn scene_admission(windows: &WindowManager, content: &[Content], pending: &[Content],
                              desktop_bytes: usize, id: SceneTarget)
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
    if system
    {
        (used, usize::MAX)
    }
    else
    {
        (used.saturating_add(desktop_bytes), MAX_RETAINED_BYTES)
    }
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
    let replaced_current = content.commands.iter().filter(|command| {
        matches!(command, RenderCommand::Control(d) if drawings.iter().any(|new| new.id == d.id))
    }).count();
    content.commands
           .try_reserve(drawings.len().saturating_sub(replaced_current))
           .map_err(|_| NO_MEMORY)?;
    damage.try_reserve_exact(removed_count.checked_add(drawings.len())
                                          .and_then(|n| n.checked_add(1))
                                          .ok_or(NO_MEMORY)?)
          .map_err(|_| NO_MEMORY)?;
    // No fallible operation after the first live scene change.
    content.retain_commands(|command| {
               if let RenderCommand::Control(d) = command
                  && drawings.iter().any(|new| new.id == d.id)
               {
                   damage.push(d.visible_bounds());
                   return false;
               }
               true
           });
    for d in drawings.drain(..)
    {
        damage.push(d.visible_bounds());
        content.commands.push(RenderCommand::Control(d));
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
impl Desktop
{
    pub(super) fn publish_control_layout(&mut self, window: u64, prepared: Prepared, used: usize,
                                         limit: usize)
                                         -> Result<(), Status>
    {
        let damage = publish(&mut self.content, &mut self.controls, window, prepared, used, limit)?;
        let changed = !damage.is_empty();
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
            let result =
                (|| -> Result<Vec<Rect>, Status> {
                    let window = self.windows.window(WindowId(root.window)).ok_or(NOT_FOUND)?;
                    if !self.owns(WindowId(root.window))
                    {
                        return Err(ACCESS_DENIED);
                    }
                    let prepared = self.controls.prepare_automatic(
                    root, owner, window.local_bounds(),
                    self.pending_content.iter().any(|c| c.id == SceneTarget::from(root.window)),
                    crate::machine::traps::gateway,
                    |id| self.definitions.resolve(*b"CDEF", id).map_err(crate::loader::status))?;
                    let (used, limit) = self.retained_admission(SceneTarget::from(root.window));
                    publish(&mut self.content, &mut self.controls, root.window, prepared, used,
                            limit)
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
        let content = &mut self.content;
        let pending = &self.pending_content;
        let controls = &mut self.controls;
        let definitions = &mut self.definitions;
        let desktop = &self.desktop;
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
                                                            definitions.resolve(*b"CDEF", id)
                                                               .map_err(crate::loader::status)
                                                        })?;
                            let (used, limit) = scene_admission(windows,
                                                                content,
                                                                pending,
                                                                desktop.storage(),
                                                                SceneTarget::from(id));
                            let changed = publish(content, controls, id.0, prepared, used, limit)?;
                            damage = Some((id, changed));
                            Ok(())
                        });
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

//! One capture, hit-test and paint path for every menu-bar title.
use super::*;
use crate::toolbox::processes::menu::{Action as ApplicationAction, ApplicationMenu};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target
{
    Resource(u64),
    Apple,
    Application
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Command
{
    Resource
    {
        handle: u64, selection: Selection
    },
    Apple
    {
        item: i16, action: AppleAction
    },
    Application
    {
        item: i16, action: ApplicationAction
    }
}
impl Command
{
    fn target(self) -> Target
    {
        match self
        {
            Self::Resource { handle,
                             .. } => Target::Resource(handle),
            Self::Apple { .. } => Target::Apple,
            Self::Application { .. } => Target::Application
        }
    }

    fn item(self) -> i16
    {
        match self
        {
            Self::Resource { selection,
                             .. } => selection.item as i16,
            Self::Apple { item,
                          .. }
            | Self::Application { item,
                                .. } => item
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Tracking
{
    active:      Option<Target>,
    highlighted: i16,
    keyboard:    bool,
    standalone:  bool
}

fn title_paint_bounds(state: &MenuDefinitionState) -> Rect
{
    let title = state.title_bounds;
    Rect::new(title.left - 1,
              1,
              title.right + if state.profile == APPLICATION_PROFILE { 4 } else { -1 },
              19)
}

impl Manager
{
    fn state<'a>(&'a self, target: Target, system: &'a SystemBar,
                 applications: &'a ApplicationMenu)
                 -> Option<&'a MenuDefinitionState>
    {
        match target
        {
            Target::Resource(id) => self.objects.get(id).map(|menu| &*menu.state),
            Target::Apple => Some(&system.state),
            Target::Application => Some(applications.state())
        }
    }

    fn state_mut<'a>(&'a mut self, target: Target, system: &'a mut SystemBar,
                     applications: &'a mut ApplicationMenu)
                     -> Option<&'a mut MenuDefinitionState>
    {
        match target
        {
            Target::Resource(id) => self.objects.get_mut(id).map(|menu| &mut *menu.state),
            Target::Apple => Some(&mut system.state),
            Target::Application => Some(applications.state_mut())
        }
    }

    fn title_at(&self, point: Point, system: &SystemBar, applications: &ApplicationMenu)
                -> Option<Target>
    {
        if !(0..20).contains(&point.v)
        {
            return None;
        }
        let accepts = |state: &MenuDefinitionState| {
            state.enabled != 0 && state.title_length != 0 && state.title_bounds.contains(point)
        };
        if accepts(&system.state)
        {
            return Some(Target::Apple);
        }
        if accepts(applications.state())
        {
            return Some(Target::Application);
        }
        self.bar()?.order.iter().copied().find_map(|id| {
                                             self.objects
                                                 .get(id)
                                                 .filter(|menu| {
                                                     !is_apple(&menu.state) && accepts(&menu.state)
                                                 })
                                                 .map(|_| Target::Resource(id))
                                         })
    }

    pub fn is_tracking(&self) -> bool
    {
        self.tracking.active.is_some()
    }

    pub(crate) fn application_selected(&self) -> bool
    {
        self.bar_selected(Target::Application)
    }

    pub(crate) fn system_selected(&self) -> bool
    {
        self.bar_selected(Target::Apple)
    }

    fn bar_selected(&self, target: Target) -> bool
    {
        self.tracking.active == Some(target)
        && (!self.tracking.keyboard || self.tracking.highlighted != 0)
    }

    pub fn selected_menu(&self) -> Option<i16>
    {
        let Target::Resource(id) = self.tracking.active?
        else
        {
            return None;
        };
        self.objects.get(id).map(|menu| menu.state.menu_id)
    }

    pub(super) fn track_popup(&mut self, id: u64, selected: i16)
    {
        self.tracking = Tracking { active:      Some(Target::Resource(id)),
                                   highlighted: selected,
                                   keyboard:    false,
                                   standalone:  true };
    }

    pub(crate) fn popup_key(&mut self, id: u64, direction: i16) -> Result<(), Status>
    {
        if !self.tracking.standalone || self.tracking.active != Some(Target::Resource(id))
        {
            return Err(NOT_FOUND);
        }
        let state = &self.objects.get(id).ok_or(NOT_FOUND)?.state;
        let count = state.item_count as i16;
        if count == 0
        {
            return Err(BAD_ARGUMENT);
        }
        let mut next = self.tracking.highlighted;
        for _ in 0..count
        {
            next = if direction < 0
            {
                if next <= 1 { count } else { next - 1 }
            }
            else if next >= count
            {
                1
            }
            else
            {
                next + 1
            };
            let row = &state.items[next as usize - 1];
            if row.enabled != 0 && row.separator == 0
            {
                self.tracking.highlighted = next;
                return Ok(());
            }
        }
        Ok(())
    }

    pub(crate) fn popup_choice(&self, id: u64) -> Option<Selection>
    {
        (self.tracking.standalone && self.tracking.active == Some(Target::Resource(id)))
            .then(|| self.selection(id, self.tracking.highlighted)).flatten()
    }

    pub(super) fn cancel_popup(&mut self, id: u64)
    {
        if self.tracking.active == Some(Target::Resource(id))
        {
            self.cancel_tracking();
        }
    }

    pub fn cancel_tracking(&mut self)
    {
        self.tracking = Tracking::default();
    }

    pub(crate) fn begin(&mut self, point: Point, system: &SystemBar,
                        applications: &ApplicationMenu)
                        -> bool
    {
        let Some(active) = self.title_at(point, system, applications)
        else
        {
            return false;
        };
        self.tracking = Tracking { active: Some(active),
                                   ..Tracking::default() };
        true
    }

    pub(crate) fn hover(&mut self, point: Point, system: &mut SystemBar,
                        applications: &mut ApplicationMenu)
                        -> bool
    {
        let Some(active) = self.tracking.active
        else
        {
            return false;
        };
        let previous = self.tracking;
        if !self.tracking.standalone && (0..20).contains(&point.v)
        {
            if let Some(target) = self.title_at(point, system, applications)
            {
                self.tracking.active = Some(target);
            }
            self.tracking.highlighted = 0;
        }
        else
        {
            let (entry, gate) = (self.entry, self.gate);
            self.tracking.highlighted = self.state_mut(active, system, applications)
                                            .map_or(0, |state| {
                                                invoke(entry,
                                                       gate,
                                                       state,
                                                       HIT_ITEM,
                                                       ((u32::from(point.v as u16) << 16)
                                                        | u32::from(point.h as u16))
                                                       as i64,
                                                       core::ptr::null_mut())
                                                as i16
                                            });
        }
        previous != self.tracking
    }

    fn command(&self, target: Target, item: i16, system: &SystemBar,
               applications: &ApplicationMenu)
               -> Option<Command>
    {
        let state = self.state(target, system, applications)?;
        let index = usize::try_from(item).ok()?.checked_sub(1)?;
        let row = state.items.get(index).filter(|_| index < state.item_count as usize)?;
        if state.enabled == 0 || row.enabled == 0 || row.separator != 0
        {
            return None;
        }
        match target
        {
            Target::Resource(handle) =>
            {
                self.selection(handle, item).map(|selection| {
                                                Command::Resource { handle,
                                                                    selection }
                                            })
            }
            Target::Apple =>
            {
                if self.owner
                       .as_ref()
                       .is_some_and(|owner| owner.owner().incarnation() != system.owner)
                {
                    return None;
                }
                let action = system.action(item)?;
                if let AppleAction::Application { handle,
                                                  selection } = action
                   && !self.command_valid(handle, selection)
                {
                    return None;
                }
                Some(Command::Apple { item,
                                      action })
            }
            Target::Application =>
            {
                applications.action(item).map(|action| {
                                             Command::Application { item,
                                                                    action }
                                         })
            }
        }
    }

    pub(crate) fn release_tracking(&mut self, point: Point, system: &mut SystemBar,
                                   applications: &mut ApplicationMenu)
                                   -> Option<Command>
    {
        // A modal standalone popup has its own genuine consumer and frozen row
        // mapping. If that consumer is no longer active, ordinary bar handling
        // must not publish its private rows as application MenuCommands.
        if self.tracking.standalone
        {
            self.cancel_tracking();
            return None;
        }
        self.hover(point, system, applications);
        let command = self.feedback_target(system, applications);
        if command.is_none()
        {
            self.cancel_tracking();
        }
        command
    }

    pub(crate) fn feedback_target(&self, system: &SystemBar, applications: &ApplicationMenu)
                                  -> Option<Command>
    {
        self.command(self.tracking.active?, self.tracking.highlighted, system, applications)
    }

    pub(crate) fn begin_key_feedback(&mut self, selection: Selection, system: &SystemBar,
                                     applications: &ApplicationMenu)
                                     -> Option<Command>
    {
        let (target, item) = if let Some(item) = system.key_item(selection)
        {
            (Target::Apple, item)
        }
        else
        {
            (Target::Resource(self.menu_id(selection.menu_id)?), selection.item as i16)
        };
        let command = self.command(target, item, system, applications)?;
        self.tracking = Tracking { active:      Some(target),
                                   highlighted: 0,
                                   keyboard:    true,
                                   standalone:  false };
        Some(command)
    }

    pub(crate) fn feedback_valid(&self, owner: u64, command: Command, system: &SystemBar,
                                 applications: &ApplicationMenu)
                                 -> bool
    {
        self.tracking.active == Some(command.target())
        && (command.target() != Target::Apple || system.owner == owner)
        && self.command(command.target(), command.item(), system, applications) == Some(command)
    }

    pub(crate) fn feedback_highlight(&mut self, item: i16)
    {
        self.tracking.highlighted = item;
    }

    pub(crate) fn feedback_bounds(&self, command: Command, keyboard: bool, system: &SystemBar,
                                  applications: &ApplicationMenu)
                                  -> Option<Rect>
    {
        let state = self.state(command.target(), system, applications)?;
        if keyboard
        {
            Some(title_paint_bounds(state))
        }
        else
        {
            state.items
                 .get(usize::try_from(command.item()).ok()?.checked_sub(1)?)
                 .map(|item| item.bounds)
        }
    }

    pub(crate) fn paint_feedback(&mut self, command: Command, keyboard: bool, selected: bool,
                                 system: &mut SystemBar, applications: &mut ApplicationMenu,
                                 canvas: &mut Canvas<'_>)
                                 -> Result<(), Status>
    {
        let clip =
            self.feedback_bounds(command, keyboard, system, applications).ok_or(NOT_FOUND)?;
        self.tracking.highlighted = if selected { command.item() } else { 0 };
        let bar_selected = self.bar_selected(command.target());
        let (entry, gate) = (self.entry, self.gate);
        let state = self.state_mut(command.target(), system, applications).ok_or(NOT_FOUND)?;
        state.highlighted = if selected { command.item() } else { 0 };
        state.bar_selected = u32::from(bar_selected);
        state.first = 0;
        let mut surface = Surface::from_canvas(canvas);
        surface.clip = clip;
        let status = invoke(entry,
                            gate,
                            state,
                            if keyboard { DRAW_BAR } else { DRAW_ITEM },
                            i64::from(command.item()),
                            &mut surface);
        if status == 0 { Ok(()) } else { Err(i32::try_from(status).unwrap_or(BAD_ABI)) }
    }

    fn draw_title(&mut self, target: Target, first: bool, system: &mut SystemBar,
                  applications: &mut ApplicationMenu, canvas: &mut Canvas<'_>)
                  -> Result<(), PaintError>
    {
        let selected = self.bar_selected(target);
        let (entry, gate) = (self.entry, self.gate);
        let Some(state) = self.state_mut(target, system, applications)
        else
        {
            return Ok(());
        };
        state.first = u32::from(first);
        state.bar_selected = u32::from(selected);
        let mut surface = Surface::from_canvas(canvas);
        painted(state.menu_id, invoke(entry, gate, state, DRAW_BAR, 0, &mut surface))
    }

    pub(crate) fn draw_bar(&mut self, system: &mut SystemBar, applications: &mut ApplicationMenu,
                           canvas: &mut Canvas<'_>)
                           -> Result<(), PaintError>
    {
        canvas.reset_port();
        self.draw_title(Target::Apple, true, system, applications, canvas)?;
        let count = self.bar().map_or(0, |bar| bar.order.len());
        for index in 0..count
        {
            if let Some(id) = self.id_at(index)
               && self.objects.get(id).is_some_and(|menu| !is_apple(&menu.state))
            {
                self.draw_title(Target::Resource(id), false, system, applications, canvas)?;
            }
        }
        self.draw_title(Target::Application, false, system, applications, canvas)
    }

    pub(crate) fn draw_popup(&mut self, system: &mut SystemBar,
                             applications: &mut ApplicationMenu, canvas: &mut Canvas<'_>)
                             -> Result<(), PaintError>
    {
        let Some(target) = self.tracking.active.filter(|_| !self.tracking.keyboard)
        else
        {
            return Ok(());
        };
        let highlighted = self.tracking.highlighted;
        let (entry, gate) = (self.entry, self.gate);
        let Some(state) = self.state_mut(target, system, applications)
        else
        {
            return Ok(());
        };
        state.highlighted = highlighted;
        canvas.reset_port();
        let mut surface = Surface::from_canvas(canvas);
        painted(state.menu_id, invoke(entry, gate, state, DRAW_POPUP, 0, &mut surface))
    }
}

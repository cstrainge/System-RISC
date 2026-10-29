//! Timer-driven menu acknowledgement. MDEF paints only the changed row/title.
use mac_abi::{Point, Rect, menus::Selection};

use super::{Desktop, Input};
use crate::toolbox::{events::BTN_STATE, processes::menu::Action, quickdraw::Canvas};

const PHASE_TICKS: u32 = 3;
const FLASH_COUNT: u8 = 3;
const PHASE_COUNT: u8 = FLASH_COUNT * 2;
#[derive(Clone, Copy)]
enum Target
{
    Menu
    {
        id: u64, selection: Selection, keyboard: bool
    },
    Application
    {
        item: i16, action: Action
    },
    Apple
    {
        item: i16, action: crate::toolbox::menus::AppleAction, keyboard: bool
    }
}
#[derive(Clone, Copy)]
pub(super) struct Feedback
{
    owner:     u64,
    when:      u32,
    point:     Point,
    modifiers: u16,
    bounds:    Rect,
    target:    Target,
    phase:     u8,
    deadline:  u32,
    painted:   bool,
    presented: bool
}
impl Feedback
{
    fn new(owner: u64, bounds: Rect, target: Target, when: u32, point: Point, modifiers: u16)
           -> Self
    {
        Self { owner,
               when,
               point,
               modifiers,
               bounds,
               target,
               phase: 0,
               deadline: 0,
               painted: false,
               presented: false }
    }
}
impl Desktop
{
    pub(super) fn start_menu_feedback(&mut self, selection: Selection, keyboard: bool, when: u32)
    {
        if keyboard && let Some((item, action)) = self.system_bar.begin_key(selection)
        {
            self.start_apple_menu_feedback(item, action, true, when);
            return;
        }
        let target = if keyboard
        {
            self.menus.begin_key_feedback(selection)
        }
        else
        {
            self.menus.feedback_target()
        };
        if let Some((id, bounds)) = target
        {
            self.menus.feedback_highlight(0);
            self.menu_feedback = Some(Feedback::new(self.foreground,
                                                    bounds,
                                                    Target::Menu { id,
                                                                   selection,
                                                                   keyboard },
                                                    when,
                                                    self.events.mouse,
                                                    self.events.modifiers));
        }
        else
        {
            self.menus.cancel_tracking();
            self.redraw = true;
        }
    }

    pub(super) fn start_application_menu_feedback(&mut self, action: Action, when: u32)
    {
        if let Some((item, bounds)) = self.app_menu.feedback_target()
        {
            self.app_menu.feedback_highlight(0);
            self.menu_feedback = Some(Feedback::new(self.foreground,
                                                    bounds,
                                                    Target::Application { item,
                                                                          action },
                                                    when,
                                                    self.events.mouse,
                                                    self.events.modifiers));
        }
        else
        {
            self.app_menu.cancel();
            self.redraw = true;
        }
    }

    pub(super) fn start_apple_menu_feedback(&mut self, item: i16,
                                            action: crate::toolbox::menus::AppleAction,
                                            keyboard: bool, when: u32)
    {
        if let Some(bounds) = self.system_bar.feedback_bounds(item, keyboard)
        {
            self.system_bar.highlight(0);
            self.menu_feedback = Some(Feedback::new(self.foreground,
                                                    bounds,
                                                    Target::Apple { item,
                                                                    action,
                                                                    keyboard },
                                                    when,
                                                    self.events.mouse,
                                                    self.events.modifiers));
        }
        else
        {
            self.system_bar.cancel();
            self.redraw = true;
        }
    }

    pub(super) fn cancel_menu_feedback(&mut self)
    {
        let Some(feedback) = self.menu_feedback.take()
        else
        {
            return;
        };
        let caller = self.owner;
        self.enter(feedback.owner);
        match feedback.target
        {
            Target::Menu { .. } if self.owner == feedback.owner => self.menus.cancel_tracking(),
            Target::Application { .. } => self.app_menu.cancel(),
            Target::Apple { .. } => self.system_bar.cancel(),
            _ =>
            {}
        }
        self.enter(caller);
        self.menu_cache.invalidate();
        self.redraw = true;
    }

    pub(super) fn menu_feedback_input(&mut self, input: Input) -> bool
    {
        if self.menu_feedback.is_none()
        {
            return false;
        }
        match input
        {
            Input::Move(point) => self.events.mouse = point,
            Input::Button { point,
                            down } =>
            {
                self.events.mouse = point;
                if down
                {
                    self.events.modifiers &= !BTN_STATE;
                }
                else
                {
                    self.events.modifiers |= BTN_STATE;
                }
            }
            Input::Key { character: 27,
                         down: true,
                         .. } => self.cancel_menu_feedback(),
            _ =>
            {}
        }
        true
    }

    /// Advance at most one visible phase. Delayed ticks cannot skip flashes.
    pub(super) fn tick_menu_feedback(&mut self) -> bool
    {
        let Some(mut feedback) = self.menu_feedback.take()
        else
        {
            return false;
        };
        let now = Self::when();
        // Idle polls do no menu lookup or context switching between phases.
        if feedback.owner == self.foreground
           && feedback.presented
           && feedback.phase < PHASE_COUNT
           && (now.wrapping_sub(feedback.deadline) as i32) < 0
        {
            self.menu_feedback = Some(feedback);
            return false;
        }
        let caller = self.owner;
        self.enter(feedback.owner);
        let valid =
            self.foreground == feedback.owner
            && self.owner == feedback.owner
            && match feedback.target
            {
                Target::Menu { id,
                               selection,
                               .. } => self.menus.feedback_valid(id, selection),
                Target::Application { item,
                                      action } => self.app_menu.feedback_valid(item, action),
                Target::Apple { item,
                                action,
                                .. } =>
                {
                    self.system_bar.feedback_valid(feedback.owner, item, action)
                    && match action
                    {
                        crate::toolbox::menus::AppleAction::AboutSystem => true,
                        crate::toolbox::menus::AppleAction::Application { handle,
                                                                          selection } =>
                        {
                            self.menus.command_valid(handle, selection)
                        }
                    }
                }
            };
        if !valid
           || self.dialogs.active()
           || self.core_dialogs.active()
           || self.standard_file.active(feedback.owner).is_some()
           || (self.crash.info().window.0 != 0
               && self.crash.info().dismissed == 0
               && self.windows.window(self.crash.info().window).is_some())
        {
            self.menu_feedback = Some(feedback);
            self.cancel_menu_feedback();
            self.enter(caller);
            return true;
        }
        if feedback.phase < PHASE_COUNT
           && feedback.presented
           && now.wrapping_sub(feedback.deadline) as i32 >= 0
        {
            feedback.phase += 1;
            feedback.painted = false;
            feedback.presented = false;
        }
        let keyboard = matches!(feedback.target,
                                Target::Menu { keyboard: true,
                                               .. }
                                | Target::Apple { keyboard: true,
                                                  .. });
        if feedback.phase == PHASE_COUNT && (!keyboard || feedback.presented)
        {
            match feedback.target
            {
                Target::Menu { selection,
                               .. } =>
                {
                    self.menus.cancel_tracking();
                    self.menu_cache.invalidate();
                    self.redraw |= !keyboard;
                    self.menu_selection(selection, feedback.when, feedback.point,
                                        feedback.modifiers);
                }
                Target::Application { action,
                                      .. } =>
                {
                    self.app_menu.cancel();
                    self.redraw = true;
                    self.application_action(action);
                }
                Target::Apple { action,
                                .. } =>
                {
                    self.system_bar.cancel();
                    self.menu_cache.invalidate();
                    self.redraw |= !keyboard;
                    match action
                    {
                        crate::toolbox::menus::AppleAction::Application { selection,
                                                                          .. } =>
                        {
                            self.menu_selection(selection, feedback.when, feedback.point,
                                                feedback.modifiers)
                        }
                        crate::toolbox::menus::AppleAction::AboutSystem =>
                        {
                            if let Err(status) = self.show_about_system()
                            {
                                serial!("ABOUT_SYSTEM_OPEN_FAILED status={status}; menu tracking \
                                         released");
                            }
                        }
                    }
                }
            }
            self.enter(caller);
            return true;
        }
        let mut changed = false;
        if !feedback.painted
           && !self.frame_incomplete
           && self.presentation_damage.len() < self.presentation_damage.capacity()
        {
            let selected = feedback.phase % 2 != 0;
            let mut canvas = Canvas::new(crate::platform::back_buffer(),
                                         crate::display::WIDTH,
                                         crate::display::HEIGHT);
            let result = match feedback.target
            {
                Target::Menu { id,
                               selection,
                               keyboard } =>
                {
                    self.menus.feedback_highlight(if selected { selection.item as i16 } else { 0 });
                    if keyboard
                    {
                        self.menu_cache.invalidate();
                    }
                    self.menus.paint_feedback(id, selection.item, keyboard, &mut canvas)
                }
                Target::Application { item,
                                      .. } =>
                {
                    self.app_menu.feedback_highlight(if selected { item } else { 0 });
                    self.app_menu.paint_feedback(item, &mut canvas)
                }
                Target::Apple { item,
                                keyboard,
                                .. } =>
                {
                    self.system_bar.highlight(if selected { item } else { 0 });
                    if keyboard
                    {
                        self.menu_cache.invalidate();
                    }
                    self.system_bar.paint_feedback(item, keyboard, &mut canvas)
                }
            };
            if let Err(status) = result
            {
                serial!("MENU_FEEDBACK_FAILED status={} phase={}", status, feedback.phase);
                self.frame_incomplete = true;
                self.menu_feedback = Some(feedback);
                self.cancel_menu_feedback();
                self.enter(caller);
                return true;
            }
            self.presentation_damage.push(feedback.bounds);
            feedback.painted = true;
            changed = true;
        }
        self.menu_feedback = Some(feedback);
        self.enter(caller);
        changed
    }

    /// Start each hold interval only after the phase reaches the front buffer.
    pub(super) fn acknowledge_menu_feedback(&mut self)
    {
        if let Some(feedback) = &mut self.menu_feedback
           && feedback.painted
           && !feedback.presented
        {
            feedback.presented = true;
            feedback.deadline = Self::when().wrapping_add(PHASE_TICKS);
        }
    }
}

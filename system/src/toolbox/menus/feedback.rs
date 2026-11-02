//! Timer-driven menu acknowledgement. MDEF paints only the changed row/title.
use mac_abi::{geometry::{Point, Rect},
              menus::Selection};

use super::{Toolbox, Input};
use crate::toolbox::{events::BTN_STATE, menus::Command, quickdraw::Canvas};

const PHASE_TICKS: u32 = 3;
const FLASH_COUNT: u8 = 3;
const PHASE_COUNT: u8 = FLASH_COUNT * 2;
#[derive(Clone, Copy)]
pub(super) struct Feedback
{
    owner:     u64,
    when:      u32,
    point:     Point,
    modifiers: u16,
    bounds:    Rect,
    command:   Command,
    keyboard:  bool,
    phase:     u8,
    deadline:  u32,
    painted:   bool,
    presented: bool
}
impl Feedback
{
    fn new(owner: u64, bounds: Rect, command: Command, keyboard: bool, when: u32, point: Point,
           modifiers: u16)
           -> Self
    {
        Self { owner,
               when,
               point,
               modifiers,
               bounds,
               command,
               keyboard,
               phase: 0,
               deadline: 0,
               painted: false,
               presented: false }
    }
}
impl Toolbox
{
    pub(super) fn start_menu_key_feedback(&mut self, selection: Selection, when: u32)
    {
        let command = self.menus.begin_key_feedback(selection, &self.system_bar, &self.app_menu);
        if let Some(command) = command
        {
            self.start_menu_command_feedback(command, true, when);
        }
        else
        {
            self.menus.cancel_tracking();
            self.presentation.redraw = true;
        }
    }

    pub(super) fn start_menu_command_feedback(&mut self, command: Command, keyboard: bool,
                                              when: u32)
    {
        if let Some(bounds) =
            self.menus.feedback_bounds(command, keyboard, &self.system_bar, &self.app_menu)
        {
            self.menus.feedback_highlight(0);
            self.menu_feedback = Some(Feedback::new(self.foreground, bounds, command, keyboard,
                                                    when, self.events.mouse,
                                                    self.events.modifiers));
        }
        else
        {
            self.menus.cancel_tracking();
            self.presentation.redraw = true;
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
        if self.owner == feedback.owner
        {
            self.menus.cancel_tracking();
        }
        self.enter(caller);
        self.presentation.menu_cache.invalidate();
        self.presentation.redraw = true;
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
        let valid = self.foreground == feedback.owner
                    && self.owner == feedback.owner
                    && self.menus.feedback_valid(feedback.owner, feedback.command,
                                                 &self.system_bar, &self.app_menu);
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
        let keyboard = feedback.keyboard;
        if feedback.phase == PHASE_COUNT && (!keyboard || feedback.presented)
        {
            self.menus.cancel_tracking();
            self.presentation.menu_cache.invalidate();
            self.presentation.redraw |= !keyboard;
            match feedback.command
            {
                Command::Resource { selection,
                                    .. } =>
                {
                    self.menu_selection(selection, feedback.when, feedback.point,
                                        feedback.modifiers);
                }
                Command::Application { action,
                                       .. } =>
                {
                    self.application_action(action);
                }
                Command::Apple { action,
                                 .. } =>
                {
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
           && !self.presentation.frame_incomplete
           && self.presentation.presentation_damage.len() < self.presentation.presentation_damage.capacity()
        {
            let selected = feedback.phase % 2 != 0;
            let mut canvas = Canvas::new(crate::platform::back_buffer(),
                                         crate::display::WIDTH,
                                         crate::display::HEIGHT);
            if keyboard
            {
                self.presentation.menu_cache.invalidate();
            }
            let result =
                self.menus.paint_feedback(feedback.command, keyboard, selected,
                                          &mut self.system_bar, &mut self.app_menu, &mut canvas);
            if let Err(status) = result
            {
                serial!("MENU_FEEDBACK_FAILED status={} phase={}", status, feedback.phase);
                self.presentation.frame_incomplete = true;
                self.menu_feedback = Some(feedback);
                self.cancel_menu_feedback();
                self.enter(caller);
                return true;
            }
            self.presentation.presentation_damage.push(feedback.bounds);
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

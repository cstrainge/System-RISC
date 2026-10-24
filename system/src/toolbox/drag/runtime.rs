//! Shared desktop integration. Pointer feedback never invalidates scene content.
#![forbid(unsafe_code)]
use mac_abi::{drag::*, *};

use crate::toolbox::{events::input::Input,
                     windows::{WindowId, WindowPart}};
impl super::super::runtime::Desktop
{
    pub(super) fn drag_hot(&self, point: Point) -> Option<u32>
    {
        let hit = self.windows.find_window(point);
        match hit.part
        {
            WindowPart::Content => self.drag.receiver_at(hit.window?.0),
            WindowPart::Desk => self.drag.receiver_at(0),
            _ => None
        }
    }

    pub(super) fn drag_effects(&mut self, before: Option<Rect>)
    {
        let after = self.drag.feedback();
        if before != after
        {
            for rect in before.into_iter().chain(after)
            {
                self.outline_damage.extend(crate::toolbox::quickdraw::geometry::outline_edges(rect)
                    .into_iter().filter(|r| !r.is_empty()));
            }
        }
        for rect in self.drag.highlight_damage().into_iter().flatten()
        {
            for inset in [0, 1]
            {
                let mut bounds = rect;
                bounds.inset(inset, inset);
                self.outline_damage
                    .extend(crate::toolbox::quickdraw::geometry::outline_edges(bounds));
            }
        }
        while let Some(notice) = self.drag.next_notice()
        {
            self.post_drag_notice(notice);
        }
    }

    pub(super) fn drag_tick(&mut self)
    {
        let before = self.drag.feedback();
        if let Err(status) = self.drag.tick(Self::when())
        {
            serial!("DRAG_INPUT_DEFERRED operation=tick status={status}; gesture state retained");
        }
        self.drag_effects(before);
    }

    pub(super) fn drag_input(&mut self, input: Input, when: u32) -> bool
    {
        if !self.drag.is_tracking()
        {
            return false;
        }
        let before = self.drag.feedback();
        let point = self.events.mouse;
        let hot = self.drag_hot(point);
        let result = match input
        {
            Input::Key { mac_code: 0x35,
                         down: true,
                         .. } => self.drag.escape(when).map(|_| ()),
            Input::Button { point,
                            down: false } =>
            {
                self.drag.release(point, self.events.modifiers, hot, when)
            }
            _ => self.drag.motion(point, self.events.modifiers, hot, when)
        };
        if let Err(status) = result
        {
            serial!("DRAG_INPUT_DEFERRED operation=input status={status}; gesture state retained");
        }
        self.drag_effects(before);
        true
    }

    pub fn drag_request(&mut self, call: &mut Call, input: &[u8], output: &mut [u8])
                        -> Result<(), Status>
    {
        let caller = crate::context::current_thread_context().identity().app.0;
        let owner = self.processes.ledger(caller)?;
        if call.operation == CREATE
        {
            // Original NewDrag creates an untied draft. A named source must
            // still be a live window owned by the creating application.
            if call.window != 0 && !self.owns(WindowId(call.window))
            {
                return Err(if self.windows.window(WindowId(call.window)).is_some()
                           {
                               ACCESS_DENIED
                           }
                           else
                           {
                               NOT_FOUND
                           });
            }
        }
        if call.operation == CREATE && caller == crate::context::finder_app().0
        {
            self.drag.ensure_receiver(&owner,
                                       crate::context::current_thread_context().identity()
                                                                               .thread
                                                                               .0,
                                       0)?;
        }
        if matches!(call.operation, ACCEPT | READ | ACKNOWLEDGE | ABANDON | CANCEL_RECEIVE)
           && call.receiver == 0
        {
            call.receiver = self.drag.receiver_at(call.window).ok_or(NOT_FOUND)?;
        }
        let source_read_only = if call.operation == FILE_ITEMS
                                  || call.operation == OFFER && call.kind == *b"fref"
        {
            crate::kernel::with_files(|files| {
                crate::toolbox::drag::source_read_only(files, call, input)
            }).map_err(|e| e.status())??
        }
        else
        {
            false
        };
        if call.operation == BEGIN
        {
            call.flags = u32::from(self.events.modifiers);
        }
        let before = self.drag.feedback();
        let result =
            self.drag.request(&owner,
                              crate::context::current_thread_context().identity().thread.0,
                              call,
                              input,
                              output,
                              Self::when(),
                              |call| {
                                  crate::kernel::with_files(|files| {
                                      crate::toolbox::drag::validate_target(files, call)
                                  }).map_err(|error| error.status())?
                              });
        if result.is_ok() && source_read_only
        {
            self.drag.source_read_only(call.id);
        }
        self.drag_effects(before);
        if result.is_ok() && call.operation == BEGIN && self.drag.needs_pointer_start(call.id)
        {
            let before = self.drag.feedback();
            let point = self.events.mouse;
            let hot = self.drag_hot(point);
            let motion = self.drag.motion(point, self.events.modifiers, hot, Self::when());
            if self.events.modifiers & mac_abi::events::BTN_STATE != 0
            {
                let release = self.drag.release(point, self.events.modifiers, hot, Self::when());
                self.drag_effects(before);
                motion?;
                release?;
                return result;
            }
            self.drag_effects(before);
            motion?;
        }
        result
    }
}

impl crate::toolbox::drag::lifetime::Host for crate::toolbox::runtime::Desktop
{
    fn release_drag(&mut self, ticket: mac_object_table::domain::ReleasedResource,
                    kind: crate::toolbox::drag::lifetime::Registration)
                    -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        let before = self.drag.release_registered(ticket, kind, Self::when())?;
        self.drag_effects(before);
        Ok(())
    }
}

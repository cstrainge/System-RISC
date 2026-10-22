//! Shared desktop integration. Pointer feedback never invalidates scene content.
#![forbid(unsafe_code)]
use mac_abi::{drag::*, *};

use crate::toolbox::{events::input::Input,
                     windows::{WindowId, WindowPart}};
impl super::super::runtime::Desktop
{
    fn drag_hot(&self, point: Point) -> Option<u32>
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
        self.drag.tick(Self::when());
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
        match input
        {
            Input::Key { mac_code: 0x35,
                         down: true,
                         .. } =>
            {
                self.drag.escape(when);
            }
            Input::Button { point,
                            down: false } =>
            {
                self.drag.release(point, self.events.modifiers, hot, when)
            }
            _ => self.drag.motion(point, self.events.modifiers, hot, when)
        }
        self.drag_effects(before);
        true
    }

    pub fn drag_request(&mut self, call: &mut Call, input: &[u8], output: &mut [u8])
                        -> Result<(), Status>
    {
        let caller = crate::context::current_thread_context().identity().app.0;
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
        if call.operation == CREATE && caller == 1
        {
            self.drag.ensure_receiver(caller,
                                       crate::context::current_thread_context().identity()
                                                                               .thread
                                                                               .0,
                                       0)?;
        }
        if matches!(call.operation, ACCEPT | READ | ACKNOWLEDGE) && call.receiver == 0
        {
            call.receiver = self.drag.receiver_at(call.window).ok_or(NOT_FOUND)?;
        }
        // Validate mounted-volume policy once per offer/response, never per motion.
        let mut source_read_only = false;
        if call.operation == FILE_ITEMS
        {
            if input.is_empty()
               || input.len() % FILE_ITEM_BYTES != 0
               || input.len() / FILE_ITEM_BYTES > MAX_ITEMS
            {
                return Err(BAD_ARGUMENT);
            }
            let mut checked = [0u32; 128];
            let mut count = 0;
            for bytes in input.chunks_exact(FILE_ITEM_BYTES)
            {
                let volume = u32::from_le_bytes(bytes[..4].try_into().unwrap());
                if !checked[..count].contains(&volume)
                {
                    source_read_only |= drag_volume_read_only(volume)?;
                    if count == checked.len()
                    {
                        return Err(NO_MEMORY);
                    }
                    checked[count] = volume;
                    count += 1;
                }
            }
        }
        if call.operation == OFFER && call.kind == *b"fref"
        {
            if input.len() != FILE_ITEM_BYTES
            {
                return Err(BAD_ARGUMENT);
            }
            let volume = u32::from_le_bytes(input[..4].try_into().unwrap());
            source_read_only = drag_volume_read_only(volume)?;
        }
        if call.operation == ACCEPT
           && call.flags & 1 != 0
           && call.volume != 0
           && drag_volume_read_only(call.volume)?
        {
            return Err(READ_ONLY);
        }
        if call.operation == BEGIN
        {
            call.flags = u32::from(self.events.modifiers);
        }
        let before = self.drag.feedback();
        let result =
            self.drag.request(caller,
                              crate::context::current_thread_context().identity().thread.0,
                              call,
                              input,
                              output,
                              Self::when());
        if result.is_ok() && source_read_only
        {
            self.drag.source_read_only(call.id);
        }
        self.drag_effects(before);
        if result.is_ok() && call.operation == BEGIN
        {
            let before = self.drag.feedback();
            let point = self.events.mouse;
            let hot = self.drag_hot(point);
            self.drag.motion(point, self.events.modifiers, hot, Self::when());
            if self.events.modifiers & mac_abi::events::BTN_STATE != 0
            {
                self.drag.release(point, self.events.modifiers, hot, Self::when());
            }
            self.drag_effects(before);
        }
        result
    }
}

fn drag_volume_read_only(volume: u32) -> Result<bool, Status>
{
    crate::kernel::with_files(|files| {
        let mut request = mac_abi::files::FileCall { operation: mac_abi::files::VOLUME_INFO,
                                                     volume,
                                                     ..Default::default() };
        files.request(&mut request, "", &mut [])?;
        Ok(request.volume_info.read_only != 0)
    }).map_err(|e| e.status())?
}

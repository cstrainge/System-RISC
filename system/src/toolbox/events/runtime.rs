//! Desktop integration for the pointer-free resident Event Manager service.
#![forbid(unsafe_code)]
use mac_abi::{Status, events::EventCall};

impl super::Desktop
{
    pub fn event_service(&mut self, call: &mut EventCall) -> Status
    {
        if matches!(call.operation,
                    mac_abi::events::NATIVE_PEEK
                    | mac_abi::events::OS_PEEK
                    | mac_abi::events::OS_GET)
        {
            self.cursor_dirty |= self.responsiveness.foreground(self.foreground, Self::when());
            self.cursor_dirty |= self.responsiveness.polled(self.owner, Self::when());
        }
        self.poll_inputs();
        // Quotient/remainder conversion avoids overflowing an intermediate
        // multiplication long before the underlying platform counter wraps.
        let uptime = Self::uptime();
        let position =
            (call.operation == mac_abi::events::NATIVE_POST).then(|| {
                                                                self.event_position(call.event,
                                                                                    self.owner)
                                                            });
        let status = self.events.service(call, uptime as u32, uptime);
        if let Some(position) = position
        {
            self.events.position_last(position);
        }
        status
    }
}

impl super::Desktop
{
    pub(super) fn event_position(&self, event: mac_abi::Event, owner: u64)
                                 -> mac_abi::events::EventPosition
    {
        let needs_front = matches!(event.kind,
                                   mac_abi::EventKind::Null
                                   | mac_abi::EventKind::KeyDown
                                   | mac_abi::EventKind::KeyUp
                                   | mac_abi::EventKind::AutoKey)
                          || event.kind == mac_abi::EventKind::MouseUp && event.message == 0;
        let front = if needs_front
        {
            self.windows
                .back_to_front()
                .filter(|w| self.window_owner(w.id) == Some(owner))
                .last()
                .map(|w| w.id)
        }
        else
        {
            None
        };
        let Some(id) = crate::toolbox::events::position::target(event, front)
        else
        {
            return Default::default();
        };
        if self.window_owner(id) != Some(owner)
        {
            return Default::default();
        }
        self.windows
            .window(id)
            .map(|w| crate::toolbox::events::position::project(event, id, w.bounds))
            .unwrap_or_default()
    }

    pub fn positioned_event(&mut self, call: &mut mac_abi::events::PositionedEventCall) -> Status
    {
        use mac_abi::{events::*, *};
        call.waited = 0;
        match call.operation
        {
            POSITION_NEXT | POSITION_WAIT | POSITION_PEEK =>
            {
                let (event, position, present) =
                    self.next_positioned(call.mask, call.operation != POSITION_PEEK, true);
                call.event = event;
                call.position = position;
                call.present = u32::from(present);
            }
            POSITION_INPUT =>
            {
                self.poll_inputs();
                call.event = self.events.null_event(Self::when());
                call.position = if call.window == 0
                {
                    self.event_position(call.event, self.owner)
                }
                else
                {
                    let id = WindowId(call.window);
                    if !self.owns(id) || self.windows.window(id).is_none()
                    {
                        return if self.windows.window(id).is_some()
                        {
                            ACCESS_DENIED
                        }
                        else
                        {
                            NOT_FOUND
                        };
                    }
                    crate::toolbox::events::position::project(call.event,
                                                              id,
                                                              self.windows
                                                                  .window(id)
                                                                  .unwrap()
                                                                  .bounds)
                };
                call.present = 0;
            }
            POSITION_LOCATE =>
            {
                call.position = self.event_position(call.event, self.owner);
                call.present = 1;
            }
            _ => return BAD_ARGUMENT
        }
        OK
    }
}

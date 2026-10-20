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
        self.events.service(call, uptime as u32, uptime)
    }
}

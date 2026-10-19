//! Desktop integration for the pointer-free resident Event Manager service.
#![forbid(unsafe_code)]
use mac_abi::{Status, events::EventCall};

impl super::Desktop
{
    pub fn event_service(&mut self, call: &mut EventCall) -> Status
    {
        self.poll_inputs();
        self.present();
        // Quotient/remainder conversion avoids overflowing an intermediate
        // multiplication long before the underlying platform counter wraps.
        let uptime = Self::uptime();
        self.events.service(call, uptime as u32, uptime)
    }
}

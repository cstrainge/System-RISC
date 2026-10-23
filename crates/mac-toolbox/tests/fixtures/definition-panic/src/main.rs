#![no_std]
#![no_main]
#![deny(unsafe_code)]
use mac_definition::{Message, Reply, Result, Services, StaticText};
fn handle<'a>(message: Message<'a, StaticText<'a>>, _: Services<'a>) -> Result<Reply> {
    match message {
        Message::Validate(_) => panic!("DEFINITION_PANIC_PROBE: safe author body reached; platform must report and shut down"),
        Message::Draw { .. } | Message::HitTest { .. } | Message::CalculateRegions(_) | Message::Initialize(_) | Message::Dispose(_) | Message::ChangeBounds { .. } | Message::Track { .. } => Ok(Reply::Done)
    }
}
mac_definition::define_static_text!(panic_definition, handle);

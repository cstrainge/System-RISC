//! Event Manager client bindings; the event queue and input decoding are resident.
#![no_std]
#![forbid(unsafe_code)]
pub use mac_abi::{Point, events::*};
pub use mac_traps::{Client, Error, Result};
#[derive(Clone, Copy)]
pub struct Events<'a>
{
    client: Client<'a>
}
impl<'a> Events<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client }
    }

    pub fn next(self, mask: u16) -> Result<Option<Event>>
    {
        let event = self.client.next_event(mask)?;
        Ok(if event.kind == EventKind::Null { None } else { Some(event) })
    }

    pub fn next_event(self, mask: u16) -> Result<Event>
    {
        self.client.next_event(mask)
    }
}

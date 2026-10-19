//! Thin Rust interface to resident resource-based alerts.
#![no_std]
#![forbid(unsafe_code)]
use mac_abi::{BAD_ARGUMENT, Event, WindowId, dialogs::*};
pub use mac_traps::{Client, Error, Result};
#[derive(Clone, Copy)]
pub struct Dialogs<'a>
{
    client: Client<'a>
}
impl<'a> Dialogs<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client }
    }

    /// Load ALRT + its DITL through the resource chain. `text` substitutes ^0.
    pub fn alert(self, scope: i16, resource: i16, text: &str) -> Result<WindowId>
    {
        let mut call = DialogCall { operation: OPEN_ALERT,
                                    scope,
                                    resource,
                                    ..Default::default() };
        if text.len() > call.text.len()
        {
            return Err(Error(BAD_ARGUMENT));
        }
        call.text[..text.len()].copy_from_slice(text.as_bytes());
        call.text_length = text.len() as u32;
        self.client.dialog(&mut call)?;
        Ok(WindowId(call.window))
    }

    /// Return a hit item on release, or the default/cancel item on Return/Escape.
    pub fn handle_event(self, window: WindowId, event: Event) -> Result<Option<u16>>
    {
        let mut call = DialogCall { operation: HANDLE_EVENT,
                                    window: window.0,
                                    event,
                                    ..Default::default() };
        self.client.dialog(&mut call)?;
        Ok((call.item != 0).then_some(call.item))
    }

    pub fn validate_alert(self, scope: i16, resource: i16, text: &str) -> Result<u16>
    {
        let mut call = DialogCall { operation: VALIDATE_ALERT,
                                    scope,
                                    resource,
                                    ..Default::default() };
        if text.len() > call.text.len()
        {
            return Err(Error(BAD_ARGUMENT));
        }
        call.text[..text.len()].copy_from_slice(text.as_bytes());
        call.text_length = text.len() as u32;
        self.client.dialog(&mut call)?;
        Ok(call.item)
    }

    pub fn draw(self, window: WindowId) -> Result<()>
    {
        self.operation(DRAW, window)
    }

    pub fn close(self, window: WindowId) -> Result<()>
    {
        self.operation(CLOSE, window)
    }

    fn operation(self, operation: u32, window: WindowId) -> Result<()>
    {
        self.client.dialog(&mut DialogCall { operation,
                                             window: window.0,
                                             ..Default::default() })
    }
}

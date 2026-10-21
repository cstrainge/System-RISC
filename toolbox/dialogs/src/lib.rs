//! Thin Rust interface to resident resource-based alerts.
#![no_std]
#![forbid(unsafe_code)]
use mac_abi::{WindowId, dialogs::*};
pub use mac_traps::{Client, Error, Result};
use toolbox_events::Event;
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
    pub fn alert(self, scope: mac_traps::types::ResourceScope, resource: i16, text: &str)
                 -> Result<WindowId>
    {
        let mut call = DialogCall { operation: OPEN_ALERT,
                                    scope: scope.raw(),
                                    resource,
                                    ..Default::default() };
        if text.len() > call.text.len()
        {
            return Err(Error::BadArgument);
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
                                    event: event.into_record(),
                                    ..Default::default() };
        self.client.dialog(&mut call)?;
        Ok((call.item != 0).then_some(call.item))
    }

    pub fn validate_alert(self, scope: mac_traps::types::ResourceScope, resource: i16, text: &str)
                          -> Result<u16>
    {
        let mut call = DialogCall { operation: VALIDATE_ALERT,
                                    scope: scope.raw(),
                                    resource,
                                    ..Default::default() };
        if text.len() > call.text.len()
        {
            return Err(Error::BadArgument);
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

/// A borrowed alert routes all tracking and event handling through System.
#[derive(Clone, Copy)]
pub struct AlertRef<'a>
{
    dialogs: Dialogs<'a>,
    window:  WindowId
}
#[must_use = "dropping an alert owner closes its dialog"]
pub struct Alert<'a>
{
    alert: AlertRef<'a>,
    live:  bool
}
impl<'a> Dialogs<'a>
{
    pub fn alert_owned(self, scope: mac_traps::types::ResourceScope, resource: i16, text: &str)
                       -> Result<Alert<'a>>
    {
        Ok(Alert { alert: self.alert_ref(self.alert(scope, resource, text)?),
                   live:  true })
    }

    pub fn alert_ref(self, window: WindowId) -> AlertRef<'a>
    {
        AlertRef { dialogs: self,
                   window }
    }
}
impl AlertRef<'_>
{
    pub fn window(self) -> WindowId
    {
        self.window
    }

    pub fn draw(self) -> Result<()>
    {
        self.dialogs.draw(self.window)
    }

    pub fn handle_event(self, event: Event) -> Result<Option<u16>>
    {
        self.dialogs.handle_event(self.window, event)
    }

    pub fn close(self) -> Result<()>
    {
        self.dialogs.close(self.window)
    }
}
impl<'a> Alert<'a>
{
    pub fn window(&self) -> WindowId
    {
        self.alert.window
    }

    pub fn alert(&self) -> AlertRef<'a>
    {
        self.alert
    }

    pub fn close(&mut self) -> Result<()>
    {
        if self.live
        {
            match self.alert.close()
            {
                Ok(()) | Err(Error::NotFound) => self.live = false,
                Err(error) => return Err(error)
            }
        }
        Ok(())
    }
}
impl<'a> core::ops::Deref for Alert<'a>
{
    type Target = AlertRef<'a>;

    fn deref(&self) -> &Self::Target
    {
        &self.alert
    }
}
impl Drop for Alert<'_>
{
    fn drop(&mut self)
    {
        let _ = self.close();
    }
}

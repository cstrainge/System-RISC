//! Safe single-port control protocol. QEMU sends PORT_NAME after PORT_READY.
//! Names use UTF-8; QEMU's optional final NUL is accepted, embedded NUL is not.
#![forbid(unsafe_code)]
use crate::{Diagnostic, PORT_ID};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event
{
    Progress,
    RejectName,
    Disconnected
}
pub struct Control
{
    added:      bool,
    named:      bool,
    host_open:  bool,
    guest_open: bool,
    pending:    u8
}
impl Default for Control
{
    fn default() -> Self
    {
        Self::new()
    }
}
impl Control
{
    pub const fn new() -> Self
    {
        Self { added:      false,
               named:      false,
               host_open:  false,
               guest_open: false,
               pending:    1 }
    }

    pub fn connected(&self) -> bool
    {
        self.added && self.named && self.host_open && self.guest_open
    }

    pub fn identified(&self) -> bool
    {
        self.named
    }

    pub fn ingest(&mut self, expected_name: &str, bytes: &[u8]) -> Result<Event, Diagnostic>
    {
        if bytes.len() < 8
        {
            return Err(Diagnostic::MalformedControl);
        }
        if bytes[..4] != PORT_ID.to_le_bytes()
        {
            return Err(Diagnostic::Profile);
        }
        let event = u16::from_le_bytes([bytes[4], bytes[5]]);
        let value = u16::from_le_bytes([bytes[6], bytes[7]]);
        if event != 7 && bytes.len() != 8
        {
            return Err(Diagnostic::MalformedControl);
        }
        match event
        {
            1 if value == 1 && !self.added =>
            {
                self.added = true;
                self.pending |= 2;
            }
            2 if value == 1 && self.added => return Ok(Event::Disconnected),
            4 if value <= 1 && self.added => (),
            5 if self.added => (), // Console resize has no byte-stream meaning.
            6 if value <= 1 && self.added =>
            {
                if value == 0 && self.host_open
                {
                    return Ok(Event::Disconnected);
                }
                self.host_open = value != 0;
            }
            7 if value == 1 && self.added && self.pending & 2 == 0 && !self.named =>
            {
                let mut name = &bytes[8..];
                if name.last() == Some(&0)
                {
                    name = &name[..name.len() - 1];
                }
                let name = core::str::from_utf8(name).map_err(|_| Diagnostic::MalformedControl)?;
                if name.is_empty() || name.as_bytes().contains(&0)
                {
                    return Err(Diagnostic::MalformedControl);
                }
                if name != expected_name
                {
                    return Ok(Event::RejectName);
                }
                self.named = true;
                self.pending |= 4;
            }
            _ => return Err(Diagnostic::MalformedControl)
        }
        Ok(Event::Progress)
    }

    /// Do not acknowledge until the entire control record was published.
    pub fn next(&self) -> Option<[u8; 8]>
    {
        let event: u16 = if self.pending & 1 != 0
        {
            0
        }
        else if self.pending & 2 != 0
        {
            3
        }
        else if self.pending & 4 != 0
        {
            6
        }
        else
        {
            return None;
        };
        let mut bytes = [0; 8];
        bytes[..4].copy_from_slice(&PORT_ID.to_le_bytes());
        bytes[4..6].copy_from_slice(&event.to_le_bytes());
        bytes[6..8].copy_from_slice(&1u16.to_le_bytes());
        Some(bytes)
    }

    pub fn published(&mut self)
    {
        if self.pending & 1 != 0
        {
            self.pending &= !1;
        }
        else if self.pending & 2 != 0
        {
            self.pending &= !2;
        }
        else if self.pending & 4 != 0
        {
            self.pending &= !4;
            self.guest_open = true;
        }
    }
}

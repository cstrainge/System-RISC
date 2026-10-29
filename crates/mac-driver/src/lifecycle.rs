//! Scheduled startup confirmation for modules with asynchronous prerequisites.
use mac_abi::BAD_ARGUMENT;
use mac_module::lifecycle as wire;

use crate::{Client, Error, Provider, Result};

/// A deferred initializer admits workers but leaves its offered services hidden.
/// Keep this capability with the actual scheduled worker until negotiation and
/// all required retained storage have been admitted.
pub struct Startup
{
    provider: Provider
}
impl Startup
{
    pub fn defer(client: Client) -> Result<Self>
    {
        let startup = Self { provider: client.resolve(wire::CLASS, wire::VERSION)? };
        startup.call(wire::DEFER, &[])?;
        Ok(startup)
    }

    pub fn confirm(&self) -> Result<()>
    {
        self.call(wire::CONFIRM, &[])
    }

    /// Retain a named refusal without publishing provisional interfaces. The
    /// same admitted worker may retry actual negotiation before confirmation.
    pub fn refuse(&self, status: i32, cause: &str) -> Result<()>
    {
        if status >= 0
        {
            return Err(Error(BAD_ARGUMENT));
        }
        self.message(wire::REFUSE, status, cause)
    }

    pub fn report(&self, status: i32, cause: &str) -> Result<()>
    {
        if status > 0
        {
            return Err(Error(BAD_ARGUMENT));
        }
        self.message(wire::REPORT, status, cause)
    }

    fn message(&self, operation: u32, status: i32, cause: &str) -> Result<()>
    {
        if cause.is_empty() || cause.len() > wire::MAX_CAUSE || cause.chars().any(char::is_control)
        {
            return Err(Error(BAD_ARGUMENT));
        }
        let mut input = [0; 4 + wire::MAX_CAUSE];
        input[..4].copy_from_slice(&status.to_le_bytes());
        input[4..4 + cause.len()].copy_from_slice(cause.as_bytes());
        self.call(operation, &input[..4 + cause.len()])
    }

    fn call(&self, operation: u32, input: &[u8]) -> Result<()>
    {
        let mut reply = [0; wire::REPLY_BYTES];
        let length = self.provider.synchronous(operation, input, &mut reply)?;
        if length != 0
        {
            return Err(Error(mac_abi::BAD_ABI));
        }
        Ok(())
    }
}

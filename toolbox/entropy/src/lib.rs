//! Device-neutral System entropy. Callers receive only actual provider bytes.
#![no_std]
#![forbid(unsafe_code)]
use mac_traps::Client;
pub use mac_traps::entropy::{Failure, FillFailure};

#[derive(Clone, Copy)]
pub struct Entropy<'a>
{
    client: Client<'a>
}
impl<'a> Entropy<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client }
    }

    /// A readiness observation is not a dependency lease or a promise that
    /// later requests cannot lose their provider.
    pub fn ready(self) -> Result<(), Failure>
    {
        self.client.entropy_ready()
    }

    pub fn fill_some(self, bytes: &mut [u8]) -> Result<usize, Failure>
    {
        self.client.entropy_fill_some(bytes)
    }

    pub fn fill(self, bytes: &mut [u8]) -> Result<(), FillFailure>
    {
        self.client.entropy_fill(bytes)
    }
}

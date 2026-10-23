//! System-owned colors by semantic role, cached for a snapshot's revision.
pub use mac_abi::theme::Palette;
use mac_traps::{Client, Result};
pub fn current(client: Client<'_>) -> Result<Palette>
{
    client.theme()
}

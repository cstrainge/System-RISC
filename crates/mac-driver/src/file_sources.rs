//! Backing-file access for filesystem providers. Source capabilities remain
//! resident-owned until the enclosing mount's teardown has been acknowledged.
#![forbid(unsafe_code)]
use interface::{Arguments, Info, Operation};
pub use mac_file_source_interface as interface;

use crate::{Client, Completion, Error, Phase, Provider, Result};

#[derive(Clone, Copy)]
pub struct FileSources
{
    provider: Provider
}
impl FileSources
{
    pub fn new(client: Client) -> Result<Self>
    {
        Ok(Self { provider: client.resolve(interface::CLASS, interface::VERSION)? })
    }

    /// INFO is resident metadata only; it never performs backing I/O.
    pub fn info(self, source: u32) -> Result<Info>
    {
        let input = Arguments { source,
                                offset: 0,
                                length: 0 }.encode(Operation::Info)
                                           .map_err(|_| Error(mac_abi::BAD_ARGUMENT))?;
        let mut output = [0; interface::INFO_BYTES];
        let mut ticket = 0;
        match self.provider.invoke(&mut ticket,
                                    Phase::Start,
                                    Operation::Info as u32,
                                    &input,
                                    &mut output)?
        {
            Completion::Complete(length) =>
            {
                Info::decode(output.get(..length).ok_or(Error(mac_abi::BAD_ABI))?)
                    .map_err(|_| Error(mac_abi::BAD_ABI))
            }
            Completion::Pending =>
            {
                let _ = self.provider.invoke(&mut ticket,
                                             Phase::Cancel,
                                             Operation::Info as u32,
                                             &[],
                                             &mut []);
                Err(Error(mac_abi::BAD_ABI))
            }
        }
    }

    /// Retain the ticket through Pending, including cancellation. Only START
    /// supplies input; every callback borrows a fresh output buffer. A successful
    /// read may be short at EOF and never changes a public File Manager position.
    pub fn read(self, source: u32, offset: u64, ticket: &mut u32, phase: Phase, output: &mut [u8])
                -> Result<Completion>
    {
        let input = if phase == Phase::Start
        {
            Some(Arguments {source,offset,length:u32::try_from(output.len())
                .map_err(|_| Error(mac_abi::BAD_ARGUMENT))?}.encode(Operation::Read)
                .map_err(|_| Error(mac_abi::BAD_ARGUMENT))?)
        }
        else
        {
            None
        };
        self.provider.invoke(ticket,
                             phase,
                             Operation::Read as u32,
                             input.as_ref().map_or(&[][..], |bytes| bytes.as_slice()),
                             output)
    }
}

//! Cli Extension claims only the CLAP catalog file type.
#![no_std]
#![forbid(unsafe_code)]

use mac_abi::{BAD_ARGUMENT, NOT_SUPPORTED, processes::execution as wire};
use mac_driver::{Client, Completion, Driver, Error, Phase, Request, Result};

pub struct Extension;
pub static EXTENSION: Extension = Extension;

impl Driver for Extension
{
    fn initialize(&self, _: Client) -> Result<()>
    {
        Ok(())
    }

    fn request(&self, request: Request<'_>, output: &mut [u8]) -> Result<Completion>
    {
        if request.class.0 != wire::CLASS || request.phase != Phase::Start
        {
            return Err(Error(NOT_SUPPORTED));
        }
        let decision = match request.operation
        {
            wire::INSPECT if request.input.len() == wire::IDENTITY_BYTES => wire::QUERY_CATALOG,
            wire::CATALOG_RESULT if request.input.len() == wire::CATALOG_BYTES =>
            {
                let file_type = &request.input[wire::IDENTITY_BYTES..wire::IDENTITY_BYTES + 4];
                if file_type == b"CLAP" { wire::CONSOLE } else { wire::PASS }
            }
            wire::ATTACH if request.input.len() == wire::ATTACH_BYTES =>
            {
                output.get_mut(..wire::STREAM_BYTES)
                      .ok_or(Error(BAD_ARGUMENT))?
                      .copy_from_slice(&request.input[wire::IDENTITY_BYTES..]);
                return Ok(Completion::Complete(wire::STREAM_BYTES));
            }
            wire::INSPECT | wire::CATALOG_RESULT | wire::ATTACH => return Err(Error(BAD_ARGUMENT)),
            _ => return Err(Error(NOT_SUPPORTED))
        };
        *output.first_mut().ok_or(Error(BAD_ARGUMENT))? = decision;
        Ok(Completion::Complete(1))
    }
}

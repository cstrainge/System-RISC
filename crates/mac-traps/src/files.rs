//! Borrowed File Manager payloads; no filesystem logic lives in the client.
use mac_abi::{files::FileCall, traps::FILE};

use crate::{Client, Result, status};

impl Client<'_>
{
    pub fn file(self, call: &mut FileCall, name: &str, output: &mut [u8]) -> Result<()>
    {
        call.name = name.as_ptr();
        call.name_length = name.len();
        call.output = output.as_mut_ptr();
        call.capacity = output.len();
        call.transferred = 0;
        let result = status(self.invoke(FILE, call));
        // The reply may outlive the borrowed arguments; never retain their pointers.
        call.name = core::ptr::null();
        call.name_length = 0;
        call.output = core::ptr::null_mut();
        call.capacity = 0;
        result
    }
}

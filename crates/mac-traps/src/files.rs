//! Borrowed File Manager payloads; no filesystem logic lives in the client.
use mac_abi::{files::FileCall, traps::FILE};

use crate::{Client, Result, status};

impl Client<'_>
{
    pub fn icon_group(self, call: &mut mac_abi::files::IconGroupCall,
                      entries: &[mac_abi::files::IconMove])
                      -> Result<()>
    {
        if entries.len() > 1024
        {
            return Err(crate::Error::NoMemory);
        }
        call.entries = if entries.is_empty() { core::ptr::null() } else { entries.as_ptr() };
        call.count = entries.len() as u32;
        let result = status(self.invoke(FILE, call));
        call.entries = core::ptr::null();
        // count is output progress on success, so retain its scalar value.
        result
    }

    pub fn move_icons(self, call: &mut mac_abi::files::IconMoveCall,
                      entries: &[mac_abi::files::IconMove])
                      -> Result<()>
    {
        if entries.len() > 1024
        {
            return Err(crate::Error::NoMemory);
        }
        call.entries = entries.as_ptr();
        call.count = entries.len() as u32;
        let result = status(self.invoke(FILE, call));
        call.entries = core::ptr::null();
        call.count = 0;
        result
    }

    /// WRITE lends an immutable buffer separately from all output borrows. The
    /// operation is sealed here so caller bytes cannot be reinterpreted as READ.
    pub fn file_write(self, call: &mut FileCall, input: &[u8]) -> Result<()>
    {
        call.operation = mac_abi::files::WRITE;
        call.name = core::ptr::null();
        call.name_length = 0;
        call.output = core::ptr::null_mut();
        call.capacity = 0;
        call.input = input.as_ptr();
        call.input_length = input.len().min(mac_abi::files::MAX_FILE_TRANSFER);
        call.transferred = 0;
        let result = status(self.invoke(FILE, call));
        call.input = core::ptr::null();
        call.input_length = 0;
        result
    }

    /// Read into an uninitialized byte span without reading its old contents.
    /// On success exactly the returned prefix is initialized. The actual FILE
    /// service owns its result across waits and copies only that prefix into
    /// the fresh caller span; the remainder is untouched. Provider descriptors
    /// without this output boundary return Unsupported before lending a span.
    pub fn file_read_uninit(self, call: &mut FileCall, output: &mut [core::mem::MaybeUninit<u8>])
                            -> Result<usize>
    {
        call.operation = mac_abi::files::READ;
        call.index = mac_abi::files::READ_UNINITIALIZED;
        call.name = core::ptr::null();
        call.name_length = 0;
        call.input = core::ptr::null();
        call.input_length = 0;
        let capacity = output.len().min(mac_abi::files::MAX_FILE_TRANSFER);
        call.output = output.as_mut_ptr().cast();
        call.capacity = capacity;
        call.transferred = 0;
        let result = status(self.invoke(FILE, call));
        call.output = core::ptr::null_mut();
        call.capacity = 0;
        call.index = 0;
        result?;
        if call.transferred > capacity
        {
            return Err(crate::Error::BadAbi);
        }
        Ok(call.transferred)
    }

    pub fn file(self, call: &mut FileCall, name: &str, output: &mut [u8]) -> Result<()>
    {
        call.input = core::ptr::null();
        call.input_length = 0;
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

impl Client<'_>
{
    pub fn file_transfer(self, call: &mut mac_abi::files::FileTransferCall,
                         sources: &[crate::types::TransferSource])
                         -> Result<()>
    {
        if sources.len() > mac_abi::files::MAX_TRANSFER_SOURCES
        {
            return Err(crate::Error::NoMemory);
        }
        // TransferSource is repr(transparent) over exactly this wire record.
        // The borrowed range lasts for this invocation; System owns its copy.
        call.sources = if sources.is_empty() { core::ptr::null() } else { sources.as_ptr().cast() };
        call.source_count = sources.len();
        let result = status(self.invoke(FILE, call));
        call.sources = core::ptr::null();
        call.source_count = 0;
        result
    }
}

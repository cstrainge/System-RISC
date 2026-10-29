//! Actual module preference storage; successful writes require writable media.
//! Authors decode/encode their private typed JSON schema around these methods.
use mac_abi::{BAD_ABI, BAD_ARGUMENT};
use mac_module::preferences as wire;

use crate::{Client, Error, Provider, Result};

pub struct Document
{
    provider: Provider,
    bytes:    [u8; wire::MAX_BYTES],
    length:   usize,
    present:  bool
}
impl Document
{
    pub fn bytes(&self) -> Option<&[u8]>
    {
        self.present.then_some(&self.bytes[..self.length])
    }
}
pub struct Preferences
{
    provider: Provider
}
impl Preferences
{
    pub fn new(client: Client) -> Result<Self>
    {
        Ok(Self { provider: client.resolve(wire::CLASS, wire::VERSION)? })
    }

    pub fn load(&self) -> Result<Document>
    {
        let mut reply = [0; wire::REPLY_BYTES];
        let length = self.provider.synchronous(wire::LOAD, &[], &mut reply)?;
        if length == 0 || reply[0] > 1 || reply[0] == 0 && length != 1
        {
            return Err(Error(BAD_ABI));
        }
        let mut doc = Document { provider: self.provider,
                                 bytes:    [0; wire::MAX_BYTES],
                                 length:   length - 1,
                                 present:  reply[0] == 1 };
        doc.bytes[..doc.length].copy_from_slice(&reply[1..length]);
        Ok(doc)
    }

    pub fn revision(&self) -> Result<u64>
    {
        let mut reply = [0; wire::REPLY_BYTES];
        if self.provider.synchronous(wire::REVISION, &[], &mut reply)? != 8
        {
            return Err(Error(BAD_ABI));
        }
        Ok(u64::from_le_bytes(reply[..8].try_into().map_err(|_| Error(BAD_ABI))?))
    }

    pub fn save(&self, previous: &Document, json: &[u8]) -> Result<()>
    {
        if previous.provider.handle != self.provider.handle
           || previous.provider.client.module() != self.provider.client.module()
           || json.len() > wire::MAX_BYTES
        {
            return Err(Error(BAD_ARGUMENT));
        }
        let mut input = [0; wire::MAX_REQUEST];
        let expected = previous.bytes();
        input[..4].copy_from_slice(&expected.map_or(wire::ABSENT, |b| b.len() as u32)
                                            .to_le_bytes());
        let length = expected.map_or(0, <[u8]>::len);
        if let Some(bytes) = expected
        {
            input[4..4 + length].copy_from_slice(bytes);
        }
        input[4 + length..4 + length + json.len()].copy_from_slice(json);
        let mut reply = [0; wire::REPLY_BYTES];
        if self.provider
               .synchronous(wire::COMPARE_EXCHANGE, &input[..4 + length + json.len()], &mut reply)?
           != 0
        {
            return Err(Error(BAD_ABI));
        }
        Ok(())
    }
}

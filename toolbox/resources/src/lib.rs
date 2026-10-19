//! Read-only Rust Resource Manager over the resident System/application chain.
#![no_std]
#![forbid(unsafe_code)]
use mac_abi::resources::*;
pub use mac_abi::{ResourceKey,
                  resources::{APPLICATION, CHAIN, ResourceInfo, SYSTEM}};
use mac_traps::ResourceRequest;
pub use mac_traps::{Client, Error, Result};
#[derive(Clone, Copy)]
pub struct Resources<'a>
{
    client: Client<'a>,
    scope:  i16
}
impl<'a> Resources<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client,
               scope: CHAIN }
    }

    pub const fn system(self) -> Self
    {
        Self { scope: SYSTEM,
               ..self }
    }

    pub const fn application(self) -> Self
    {
        Self { scope: APPLICATION,
               ..self }
    }

    pub const fn search_chain(self) -> Self
    {
        Self { scope: CHAIN,
               ..self }
    }

    fn request(self, operation: u32, key: ResourceKey, index: u32, name: &str)
               -> Result<mac_traps::ResourceReply>
    {
        self.client.resource(ResourceRequest { operation,
                                               scope: self.scope,
                                               key,
                                               index,
                                               name,
                                               ..Default::default() },
                             &mut [])
    }

    pub fn get(self, key: ResourceKey) -> Result<Resource<'a>>
    {
        self.request(GET, key, 0, "").map(|r| {
                                         Resource { client: self.client,
                                                    info:   r.info }
                                     })
    }

    pub fn named(self, kind: [u8; 4], name: &str) -> Result<Resource<'a>>
    {
        self.request(NAMED,
                     ResourceKey { kind,
                                   id: 0 },
                     0,
                     name)
            .map(|r| {
                Resource { client: self.client,
                           info:   r.info }
            })
    }

    pub fn count(self, kind: [u8; 4]) -> Result<u32>
    {
        Ok(self.request(COUNT,
                        ResourceKey { kind,
                                      id: 0 },
                        0,
                        "")?
               .index)
    }

    /// Rust enumeration is zero-based; resource type/ID identity remains classic.
    pub fn at(self, kind: [u8; 4], index: u32) -> Result<Resource<'a>>
    {
        self.request(INDEX,
                     ResourceKey { kind,
                                   id: 0 },
                     index,
                     "")
            .map(|r| {
                Resource { client: self.client,
                           info:   r.info }
            })
    }

    pub fn type_count(self) -> Result<u32>
    {
        Ok(self.request(COUNT_TYPES, ResourceKey::default(), 0, "")?.index)
    }

    pub fn type_at(self, index: u32) -> Result<[u8; 4]>
    {
        Ok(self.request(TYPE_INDEX, ResourceKey::default(), index, "")?.kind)
    }
}
pub struct Resource<'a>
{
    client: Client<'a>,
    info:   ResourceInfo
}
impl Resource<'_>
{
    pub fn info(&self) -> &ResourceInfo
    {
        &self.info
    }

    pub fn size(&self) -> usize
    {
        self.info.length
    }

    pub fn home(&self) -> i16
    {
        self.info.origin
    }

    pub fn attributes(&self) -> u16
    {
        self.info.attributes
    }

    pub fn key(&self) -> ResourceKey
    {
        self.info.key
    }

    pub fn name(&self) -> &str
    {
        self.info.name()
    }

    pub fn load(&self) -> Result<()>
    {
        self.client
            .resource(ResourceRequest { operation: LOAD,
                                        scope: self.info.origin,
                                        key: self.info.key,
                                        ..Default::default() },
                      &mut [])
            .map(|_| ())
    }

    pub fn read(&self, offset: usize, output: &mut [u8]) -> Result<usize>
    {
        Ok(self.client
               .resource(ResourceRequest { operation: READ,
                                           scope: self.info.origin,
                                           key: self.info.key,
                                           offset,
                                           ..Default::default() },
                         output)?
               .transferred)
    }

    pub fn release(self) -> Result<()>
    {
        self.client
            .resource(ResourceRequest { operation: RELEASE,
                                        scope: self.info.origin,
                                        key: self.info.key,
                                        ..Default::default() },
                      &mut [])
            .map(|_| ())
    }
}

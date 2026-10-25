//! Read-only Rust Resource Manager over the resident System/application chain.
#![no_std]
#![forbid(unsafe_code)]
use mac_abi::resources::*;
mod info;
pub use info::{APPLICATION, CHAIN, ImageScope, ResourceAttributes, ResourceInfo, ResourceKey,
               ResourceScope, SYSTEM, VersionInfo};
pub use mac_traps::{Client, Cnid, Error, Result, TypeCode, VolumeId};
use mac_traps::{ResourceRequest, types::ResourceFileHandle};
#[derive(Clone, Copy)]
pub struct Resources<'a>
{
    client: Client<'a>,
    scope:  ResourceScope
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

    /// Select a resident image's readonly identity. This borrows its scope and
    /// does not keep the process or its source alive after exit.
    pub const fn image(self, scope: ImageScope) -> Self
    {
        Self { scope: ResourceScope::Image(scope),
               ..self }
    }

    pub const fn search_chain(self) -> Self
    {
        Self { scope: CHAIN,
               ..self }
    }

    /// Read vers from an arbitrary catalog file without changing the current
    /// application/resource chain or loading its executable resources.
    pub fn file_version(self, volume: VolumeId, cnid: Cnid, id: i16) -> Result<VersionInfo>
    {
        self.client.file_version(volume.0, cnid.0, id)?.try_into()
    }

    fn request(self, operation: u32, key: ResourceKey, index: u32, name: &str)
               -> Result<mac_traps::ResourceReply>
    {
        self.client.resource(ResourceRequest { operation,
                                               scope: self.scope.raw(),
                                               key: key.into(),
                                               index,
                                               name,
                                               ..Default::default() },
                             &mut [])
    }

    /// Decode a STR resource to UTF8 through the resident Resource Manager.
    pub fn string(self, id: i16, output: &mut [u8]) -> Result<&str>
    {
        self.string_value(ResourceKey { kind: TypeCode::new(*b"STR "),
                                        id },
                          0,
                          output)
    }

    /// Rust indexes STR# elements from zero; the serialized resource remains classic.
    pub fn indexed_string(self, id: i16, index: u32, output: &mut [u8]) -> Result<&str>
    {
        self.string_value(ResourceKey { kind: TypeCode::new(*b"STR#"),
                                        id },
                          index,
                          output)
    }

    fn string_value(self, key: ResourceKey, index: u32, output: &mut [u8]) -> Result<&str>
    {
        let reply = self.client.resource(ResourceRequest { operation: STRING,
                                                            scope: self.scope.raw(),
                                                            key: key.into(),
                                                            index,
                                                            ..Default::default() },
                                          output)?;
        core::str::from_utf8(output.get(..reply.transferred).ok_or(Error::BadArgument)?)
            .map_err(|_|Error::BadArgument)
    }

    pub fn get(self, key: ResourceKey) -> Result<Resource<'a>>
    {
        self.request(GET, key, 0, "").and_then(|r| {
                                         Ok(Resource { client: self.client,
                                                       info:   r.info.try_into()? })
                                     })
    }

    pub fn named(self, kind: TypeCode, name: &str) -> Result<Resource<'a>>
    {
        self.request(NAMED,
                     ResourceKey { kind,
                                   id: 0 },
                     0,
                     name)
            .and_then(|r| {
                Ok(Resource { client: self.client,
                              info:   r.info.try_into()? })
            })
    }

    pub fn count(self, kind: TypeCode) -> Result<u32>
    {
        Ok(self.request(COUNT,
                        ResourceKey { kind,
                                      id: 0 },
                        0,
                        "")?
               .index)
    }

    /// Rust enumeration is zero-based; resource type/ID identity remains classic.
    pub fn at(self, kind: TypeCode, index: u32) -> Result<Resource<'a>>
    {
        self.request(INDEX,
                     ResourceKey { kind,
                                   id: 0 },
                     index,
                     "")
            .and_then(|r| {
                Ok(Resource { client: self.client,
                              info:   r.info.try_into()? })
            })
    }

    pub fn type_count(self) -> Result<u32>
    {
        Ok(self.request(COUNT_TYPES, ResourceKey::default(), 0, "")?.index)
    }

    pub fn type_at(self, index: u32) -> Result<TypeCode>
    {
        Ok(TypeCode::new(self.request(TYPE_INDEX, ResourceKey::default(), index, "")?.kind))
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

    pub fn home(&self) -> ResourceScope
    {
        self.info.origin
    }

    pub fn attributes(&self) -> ResourceAttributes
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
                                        scope: self.info.origin.raw(),
                                        key: self.info.key.into(),
                                        ..Default::default() },
                      &mut [])
            .map(|_| ())
    }

    pub fn read(&self, offset: usize, output: &mut [u8]) -> Result<usize>
    {
        Ok(self.client
               .resource(ResourceRequest { operation: READ,
                                           scope: self.info.origin.raw(),
                                           key: self.info.key.into(),
                                           offset,
                                           ..Default::default() },
                         output)?
               .transferred)
    }

    pub fn release(self) -> Result<()>
    {
        self.client
            .resource(ResourceRequest { operation: RELEASE,
                                        scope: self.info.origin.raw(),
                                        key: self.info.key.into(),
                                        ..Default::default() },
                      &mut [])
            .map(|_| ())
    }
}

/// An owned read-only resource-fork mark, independent of the active chain.
/// Drop closes it. Running apps may be inspected without loading their code.
pub struct ResourceFile<'a>
{
    client: Client<'a>,
    handle: ResourceFileHandle
}
#[derive(Clone, Copy, Debug)]
pub struct FileResourceInfo
{
    pub info:     ResourceInfo,
    /// Distinguishes an absent name from a present empty Pascal string.
    pub has_name: bool
}
impl<'a> Resources<'a>
{
    pub fn open_file(self, volume: VolumeId, cnid: Cnid) -> Result<ResourceFile<'a>>
    {
        let mut call = mac_abi::files::FileCall { operation: mac_abi::files::OPEN,
                                                  volume: volume.0,
                                                  cnid: cnid.0,
                                                  fork: mac_abi::files::RESOURCE_FORK,
                                                  ..Default::default() };
        self.client.file(&mut call, "", &mut [])?;
        let file = ResourceFile { client: self.client,
                                  handle: ResourceFileHandle(call.handle) };
        file.type_count()?;
        Ok(file)
    }
}
impl ResourceFile<'_>
{
    fn request(&self, operation: u32, key: ResourceKey, index: u32, name: &str, offset: usize,
               output: &mut [u8])
               -> Result<FileResourceCall>
    {
        let mut call = FileResourceCall { operation,
                                          handle: self.handle.0,
                                          key: key.into(),
                                          index,
                                          offset,
                                          ..Default::default() };
        self.client.file_resource(&mut call, name, output)?;
        Ok(call)
    }

    pub fn type_count(&self) -> Result<u32>
    {
        Ok(self.request(COUNT_TYPES, ResourceKey::default(), 0, "", 0, &mut [])?.index)
    }

    pub fn type_at(&self, index: u32) -> Result<TypeCode>
    {
        Ok(TypeCode::new(self.request(TYPE_INDEX, ResourceKey::default(), index, "", 0, &mut [])?
                             .key
                             .kind))
    }

    pub fn count(&self, kind: TypeCode) -> Result<u32>
    {
        Ok(self.request(COUNT,
                        ResourceKey { kind,
                                      id: 0 },
                        0,
                        "",
                        0,
                        &mut [])?
               .index)
    }

    fn metadata(&self, operation: u32, key: ResourceKey, index: u32, name: &str)
                -> Result<FileResourceInfo>
    {
        let call = self.request(operation, key, index, name, 0, &mut [])?;
        Ok(FileResourceInfo { info:     call.info.try_into()?,
                              has_name: call.has_name != 0 })
    }

    pub fn at(&self, kind: TypeCode, index: u32) -> Result<FileResourceInfo>
    {
        self.metadata(INDEX,
                      ResourceKey { kind,
                                    id: 0 },
                      index,
                      "")
    }

    pub fn get(&self, key: ResourceKey) -> Result<FileResourceInfo>
    {
        self.metadata(GET, key, 0, "")
    }

    pub fn named(&self, kind: TypeCode, name: &str) -> Result<FileResourceInfo>
    {
        self.metadata(NAMED,
                      ResourceKey { kind,
                                    id: 0 },
                      0,
                      name)
    }

    pub fn read(&self, key: ResourceKey, offset: usize, output: &mut [u8]) -> Result<usize>
    {
        Ok(self.request(READ, key, 0, "", offset, output)?.transferred)
    }
}
impl ResourceFile<'_>
{
    /// Retains ownership when resident close fails, allowing an explicit retry.
    pub fn close(&mut self) -> Result<()>
    {
        if self.handle.0 != 0
        {
            let mut call = mac_abi::files::FileCall { operation: mac_abi::files::CLOSE,
                                                      handle: self.handle.0,
                                                      ..Default::default() };
            self.client.file(&mut call, "", &mut [])?;
            self.handle = ResourceFileHandle(0);
        }
        Ok(())
    }
}
impl Drop for ResourceFile<'_>
{
    fn drop(&mut self)
    {
        let _ = self.close();
    }
}

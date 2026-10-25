//! A shared resource search chain for native applications and resident managers.
#![forbid(unsafe_code)]
use alloc::{collections::BTreeSet, vec::Vec};

use mac_abi::{resources::*, *};
use mac_formats::{resource::Resource, text};
pub struct Files
{
    pub system: Vec<Resource>
}
/// Borrowed catalogue access. Implementations cannot supply a resource that
/// outlives their own borrow; no global image reference escapes this surface.
pub(crate) trait ImageCatalog: Sync
{
    fn resources(&self, index: u64) -> Option<&[Resource]>;
}
#[derive(Clone, Copy)]
pub struct Manager<'a>
{
    system:             &'a [Resource],
    application:        &'a [Resource],
    fonts:              &'a [Resource],
    images:             Option<&'a dyn ImageCatalog>,
    #[cfg(target_arch = "riscv64")]
    application_origin: u64,
    #[cfg(target_arch = "riscv64")]
    image_pin:          Option<&'a crate::images::Read>
}
impl<'a> Manager<'a>
{
    pub const fn new(system: &'a [Resource], application: &'a [Resource], fonts: &'a [Resource])
                     -> Self
    {
        Self { system,
               application,
               fonts,
               images: None,
               #[cfg(target_arch = "riscv64")]
               application_origin: 0,
               #[cfg(target_arch = "riscv64")]
               image_pin: None }
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn with_images(mut self, images: &'a dyn ImageCatalog, application_origin: u64,
                              image_pin: Option<&'a crate::images::Read>)
                              -> Self
    {
        self.images = Some(images);
        self.application_origin = application_origin;
        self.image_pin = image_pin;
        self
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn retain_origin(self) -> Result<Option<crate::images::Read>, Status>
    {
        self.image_pin.map(crate::images::Read::retain).transpose()
    }

    /// A returned application resource captures its actual source identity;
    /// keeping the logical view must never retarget it to a later application.
    pub(crate) fn resolved_scope(self, scope: Scope) -> Scope
    {
        #[cfg(target_arch = "riscv64")]
        if scope == APPLICATION && self.application_origin != 0
        {
            return Scope::image(self.application_origin);
        }
        scope
    }

    pub fn all(self, scope: mac_abi::resources::Scope)
               -> Result<Vec<(Scope, &'a Resource)>, Status>
    {
        if let Some(image) = scope.image_id()
        {
            let resources =
                self.images.ok_or(BAD_ARGUMENT)?.resources(image).ok_or(BAD_ARGUMENT)?;
            return Ok(resources.iter().map(|resource| (scope, resource)).collect());
        }
        if ![CHAIN, SYSTEM, APPLICATION].contains(&scope)
        {
            return Err(BAD_ARGUMENT);
        }
        let mut resources = Vec::new();
        if scope == CHAIN || scope == APPLICATION
        {
            for r in self.application
            {
                resources.push((self.resolved_scope(APPLICATION), r));
            }
        }
        if scope == CHAIN || scope == SYSTEM
        {
            for r in self.system.iter().chain(self.fonts)
            {
                resources.push((SYSTEM, r));
            }
        }
        Ok(resources)
    }

    pub fn get(self, scope: mac_abi::resources::Scope, key: ResourceKey)
               -> Result<(Scope, &'a Resource), Status>
    {
        if let Some(image) = scope.image_id()
        {
            let resources =
                self.images.ok_or(BAD_ARGUMENT)?.resources(image).ok_or(BAD_ARGUMENT)?;
            return resources.iter()
                            .find(|resource| resource.kind == key.kind && resource.id == key.id)
                            .map(|resource| (scope, resource))
                            .ok_or(NOT_FOUND);
        }
        // Lookup is allocation-free, including nested manager/resource requests.
        if (scope == CHAIN || scope == APPLICATION)
           && let Some(r) = self.application.iter().find(|r| r.kind == key.kind && r.id == key.id)
        {
            return Ok((self.resolved_scope(APPLICATION), r));
        }
        if (scope == CHAIN || scope == SYSTEM)
           && let Some(r) =
               self.system.iter().chain(self.fonts).find(|r| r.kind == key.kind && r.id == key.id)
        {
            return Ok((SYSTEM, r));
        }
        if ![CHAIN, SYSTEM, APPLICATION].contains(&scope)
        {
            Err(BAD_ARGUMENT)
        }
        else
        {
            Err(NOT_FOUND)
        }
    }

    pub fn named(self, scope: mac_abi::resources::Scope, kind: [u8; 4], name: &str)
                 -> Result<(Scope, &'a Resource), Status>
    {
        self.all(scope)?
            .into_iter()
            .find(|(_, r)| {
                r.kind == kind && r.name.as_ref().is_some_and(|bytes| text::decode(bytes) == name)
            })
            .ok_or(NOT_FOUND)
    }

    pub fn info(origin: mac_abi::resources::Scope, resource: &Resource) -> ResourceInfo
    {
        let mut info = ResourceInfo { key: ResourceKey { kind: resource.kind,
                                                         id:   resource.id },
                                      origin,
                                      attributes: u16::from(resource.attributes),
                                      length: resource.data.len(),
                                      ..Default::default() };
        if let Some(name) = &resource.name
        {
            let name = text::decode(name);
            let mut length = name.len().min(info.name.len());
            while !name.is_char_boundary(length)
            {
                length -= 1;
            }
            info.name[..length].copy_from_slice(&name.as_bytes()[..length]);
            info.name_length = length as u32;
        }
        info
    }

    /// Resource views carry a source identity, not a persistent image pin.
    /// Each request borrows retained source data for its own duration; release
    /// drops the caller's logical view without purging System fonts.
    pub fn request(self, call: &mut ResourceCall, name: &str, output: &mut [u8])
                   -> Result<(), Status>
    {
        if !call.scope.valid()
        {
            return Err(BAD_ARGUMENT);
        }

        match call.operation
        {
            COUNT =>
            {
                call.index =
                    self.all(call.scope)?.iter().filter(|(_, r)| r.kind == call.key.kind).count()
                    as u32;
                return Ok(());
            }
            COUNT_TYPES | TYPE_INDEX =>
            {
                let mut seen = BTreeSet::new();
                let types: Vec<_> = self.all(call.scope)?
                                        .iter()
                                        .filter_map(|(_, r)| seen.insert(r.kind).then_some(r.kind))
                                        .collect();
                if call.operation == COUNT_TYPES
                {
                    call.index = types.len() as u32;
                }
                else
                {
                    call.key.kind = *types.get(call.index as usize).ok_or(NOT_FOUND)?;
                }
                return Ok(());
            }
            _ =>
            {}
        }
        let (origin, resource) = match call.operation
        {
            GET | INFO | READ | LOAD | RELEASE | STRING => self.get(call.scope, call.key)?,
            NAMED => self.named(call.scope, call.key.kind, name)?,
            INDEX =>
            {
                self.all(call.scope)?
                    .into_iter()
                    .filter(|(_, r)| r.kind == call.key.kind)
                    .nth(call.index as usize)
                    .ok_or(NOT_FOUND)?
            }
            _ => return Err(NOT_SUPPORTED)
        };
        call.info = Self::info(origin, resource);
        if call.operation == STRING
        {
            let data = &resource.data;
            let mut at = 0;
            if resource.kind == *b"STR#"
            {
                let count = u16::from_be_bytes(data.get(..2)
                                                   .ok_or(BAD_ARGUMENT)?
                                                   .try_into()
                                                   .map_err(|_| BAD_ARGUMENT)?);
                if call.index >= u32::from(count)
                {
                    return Err(NOT_FOUND);
                }
                at = 2;
                for _ in 0..call.index
                {
                    at += 1 + usize::from(*data.get(at).ok_or(BAD_ARGUMENT)?);
                }
            }
            else if resource.kind != *b"STR " || call.index != 0
            {
                return Err(BAD_ARGUMENT);
            }
            let length = usize::from(*data.get(at).ok_or(BAD_ARGUMENT)?);
            let value = text::decode(data.get(at + 1..at + 1 + length).ok_or(BAD_ARGUMENT)?);
            if output.len() < value.len()
            {
                return Err(NO_MEMORY);
            }
            output[..value.len()].copy_from_slice(value.as_bytes());
            call.transferred = value.len();
        }
        if call.operation == READ
        {
            if call.offset > resource.data.len()
            {
                return Err(BAD_ARGUMENT);
            }
            let length = (resource.data.len() - call.offset).min(output.len());
            output[..length].copy_from_slice(&resource.data[call.offset..call.offset + length]);
            call.transferred = length;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests
{
    use alloc::vec;

    use super::*;
    fn r(id: i16, data: &[u8]) -> Resource
    {
        Resource { kind: *b"PICT",
                   id,
                   name: Some(vec![0x8e]),
                   attributes: 0x20,
                   data: data.to_vec() }
    }
    #[test]
    fn search_chain_scope_names_and_partial_reads_are_consistent()
    {
        let system = [r(-3, b"system"), r(2, b"second")];
        let app = [r(-3, b"application")];
        let manager = Manager::new(&system, &app, &[]);
        let key = ResourceKey { kind: *b"PICT",
                                id:   -3 };
        assert_eq!(manager.get(CHAIN, key).unwrap().1.data, b"application");
        assert_eq!(manager.get(SYSTEM, key).unwrap().1.data, b"system");
        assert_eq!(manager.named(CHAIN, *b"PICT", "é").unwrap().0, APPLICATION);
        let mut call = ResourceCall { operation: COUNT,
                                      scope: CHAIN,
                                      key,
                                      ..Default::default() };
        manager.request(&mut call, "", &mut []).unwrap();
        assert_eq!(call.index, 3);
        call.operation = READ;
        call.offset = 3;
        let mut bytes = [0; 4];
        manager.request(&mut call, "", &mut bytes).unwrap();
        assert_eq!(&bytes, b"lica");
        assert_eq!(call.transferred, 4);
        assert_eq!(call.info.name(), "é");
        assert_eq!(call.info.attributes, 0x20);
        call.offset = 999;
        assert_eq!(manager.request(&mut call, "", &mut bytes), Err(BAD_ARGUMENT));
    }
}

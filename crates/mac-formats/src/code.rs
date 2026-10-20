//! All executable resources pass through this boundary before being invoked.
//! Native envelopes load directly; 68k resources automatically request translation.
use alloc::vec::Vec;

pub use crate::code_types::*;
use crate::{Error, Result,
            native::{Descriptor, Role},
            resource::Resource};

pub struct TranslationRequest<'a>
{
    pub resource:   &'a Resource,
    pub origin:     u32,
    pub convention: CallingConvention
}

/// A future translator returns a native code-resource envelope, including its
/// architecture/calling-convention adapter. It must not treat CODE 0's table,
/// DRVR's header, or PACK dispatch tables as plain function bodies.
pub trait CodeTranslator
{
    fn translate(&mut self, request: TranslationRequest<'_>) -> Result<Vec<u8>>;
}

pub struct NoTranslator;
impl CodeTranslator for NoTranslator
{
    fn translate(&mut self, _: TranslationRequest<'_>) -> Result<Vec<u8>>
    {
        Err(Error("68k code resource requires automatic translation"))
    }
}

pub struct NativeCode
{
    pub descriptor:  Descriptor,
    pub code:        Vec<u8>,
    pub relocations: Vec<u8>
}

pub struct CodeRegistry
{
    profiles: Vec<([u8; 4], CallingConvention)>
}
impl Default for CodeRegistry
{
    fn default() -> Self
    {
        Self::new()
    }
}
impl CodeRegistry
{
    pub fn new() -> Self
    {
        Self { profiles: STANDARD_CODE_TYPES.to_vec() }
    }

    /// Managers/component records may declare other executable resource types.
    /// Ordinary Resource Manager data loads never trigger translation.
    pub fn register(&mut self, kind: [u8; 4], convention: CallingConvention)
    {
        if let Some(profile) = self.profiles.iter_mut().find(|p| p.0 == kind)
        {
            profile.1 = convention;
        }
        else
        {
            self.profiles.push((kind, convention));
        }
    }

    pub fn convention(&self, kind: [u8; 4], id: i16) -> Option<CallingConvention>
    {
        let convention = self.profiles.iter().find(|p| p.0 == kind)?.1;
        Some(if kind == *b"CODE" && id == 0
             {
                 CallingConvention::CodeJumpTable
             }
             else
             {
                 convention
             })
    }

    pub fn prepare(&self, resource: &Resource, origin: u32, translator: &mut impl CodeTranslator)
                   -> Result<NativeCode>
    {
        let convention = self.convention(resource.kind, resource.id)
                             .ok_or(Error("resource type is not declared executable"))?;
        let translated;
        let payload = if resource.data.starts_with(b"RVN1")
        {
            &resource.data[..]
        }
        else
        {
            translated = translator.translate(TranslationRequest { resource,
                                                                   origin,
                                                                   convention })?;
            &translated
        };
        if payload.get(32..36) == Some(b"\x7fELF")
        {
            return Err(Error("obsolete ELF-backed code envelope; rebuild the system"));
        }
        let size = crate::native::DESCRIPTOR_SIZE;
        if payload.len() < size
        {
            return Err(Error("truncated native code resource"));
        }
        let descriptor = Descriptor::decode(&payload[..size])?;
        if descriptor.role != Role::CodeResource
        {
            return Err(Error("native resource has the wrong executable role"));
        }
        let body = &payload[size..];
        let end = crate::native::image::inspect(body.len(),
                                                &descriptor,
                                                |at, out| {
                                                    out.copy_from_slice(crate::slice(body,
                                                                                     at,
                                                                                     out.len())?);
                                                    Ok(())
                                                },
                                                |_| {})?;
        crate::native::image::LoadPlan::new(&body[..end],
                                            &descriptor,
                                            Some(&body[end..]),
                                            descriptor.load_base)?;
        Ok(NativeCode { descriptor,
                        code: body[..end].to_vec(),
                        relocations: body[end..].to_vec() })
    }
}

#[cfg(test)]
mod tests
{
    use super::*;
    struct Translator
    {
        calls: usize
    }
    impl CodeTranslator for Translator
    {
        fn translate(&mut self, request: TranslationRequest<'_>) -> Result<Vec<u8>>
        {
            self.calls += 1;
            if request.resource.kind == *b"CODE"
            {
                assert_eq!(request.convention, CallingConvention::CodeJumpTable);
            }
            let descriptor = Descriptor { role:        Role::CodeResource,
                                          abi_version: crate::native::ABI_VERSION,
                                          load_base:   0x82000000,
                                          load_limit:  0x82100000,
                                          entry:       0x82000000 };
            let mut bytes = descriptor.encode()?.to_vec();
            bytes.extend(crate::native::image::encode(&descriptor,
                                                      [(0, 5, [0x82, 0x80].as_slice())])?);
            bytes.extend_from_slice(b"RLC1\0\0\0\0");
            Ok(bytes)
        }
    }
    #[test]
    fn every_registered_family_automatically_requests_translation_but_data_does_not()
    {
        let mut registry = CodeRegistry::new();
        let mut translator = Translator { calls: 0 };
        for &(kind, _) in STANDARD_CODE_TYPES
        {
            let resource = Resource { kind,
                                      id: 0,
                                      name: None,
                                      attributes: 0,
                                      data: alloc::vec![0x4e, 0x75] };
            assert!(registry.prepare(&resource, 1, &mut translator).is_ok());
        }
        assert_eq!(translator.calls, STANDARD_CODE_TYPES.len());
        for kind in [*b"WIND", *b"CNTL", *b"MENU", *b"DITL", *b"thng"]
        {
            assert!(registry.convention(kind, 0).is_none());
        }
        registry.register(*b"TEST", CallingConvention::Custom);
        assert_eq!(registry.convention(*b"TEST", 1), Some(CallingConvention::Custom));
        let resource = Resource { kind:       *b"WDEF",
                                  id:         0,
                                  name:       None,
                                  attributes: 0,
                                  data:       alloc::vec![0x4e, 0x75] };
        assert!(registry.prepare(&resource, 0, &mut NoTranslator).is_err());
    }
}

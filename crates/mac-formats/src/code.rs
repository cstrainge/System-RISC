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
    pub origin:     u64,
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
        Err(Error::TranslationRequired("68k code resource requires automatic translation"))
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
    // Standard profiles are immutable static data. Store only registrations and
    // overrides, rather than copying the complete table for each loader.
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
        Self { profiles: Vec::new() }
    }

    /// Managers/component records may declare other executable resource types.
    /// Ordinary Resource Manager data loads never trigger translation.
    pub fn register(&mut self, kind: [u8; 4], convention: CallingConvention) -> Result<()>
    {
        if let Some(profile) = self.profiles.iter_mut().find(|p| p.0 == kind)
        {
            profile.1 = convention;
        }
        else
        {
            self.profiles.try_reserve(1).map_err(|_| {
                                             Error::AllocationFailed("executable resource \
                                                                      registration allocation \
                                                                      failed")
                                         })?;
            self.profiles.push((kind, convention));
        }
        Ok(())
    }

    pub fn convention(&self, kind: [u8; 4], id: i16) -> Option<CallingConvention>
    {
        if kind == mac_module::ExtensionTarget::RESOURCE_TYPE
        {
            return mac_module::ExtensionTarget::from_resource_id(id).map(|target| {
                                                                        match target
                {
                    mac_module::ExtensionTarget::System => CallingConvention::SystemExtension,
                    mac_module::ExtensionTarget::Finder => CallingConvention::FinderExtension
                }
                                                                    });
        }
        let convention = self.profiles.iter().chain(STANDARD_CODE_TYPES).find(|p| p.0 == kind)?.1;
        Some(if kind == *b"CODE" && id == 0
             {
                 CallingConvention::CodeJumpTable
             }
             else
             {
                 convention
             })
    }

    pub fn prepare(&self, resource: &Resource, origin: u64, translator: &mut impl CodeTranslator)
                   -> Result<NativeCode>
    {
        let convention = self.convention(resource.kind, resource.id)
                             .ok_or(Error::Malformed("resource type is not declared executable"))?;
        let translated;
        let payload = if resource.data.starts_with(b"RVN1")
        {
            &resource.data[..]
        }
        else
        {
            if resource.kind == mac_module::ExtensionTarget::RESOURCE_TYPE
            {
                return Err(Error::Malformed("extension RV64 resource must contain a native \
                                             code-resource envelope; rebuild the extension"));
            }
            translated = translator.translate(TranslationRequest { resource,
                                                                   origin,
                                                                   convention })?;
            &translated
        };
        if payload.get(32..36) == Some(b"\x7fELF")
        {
            return Err(Error::Malformed("obsolete ELF-backed code envelope; rebuild the system"));
        }
        let size = crate::native::DESCRIPTOR_SIZE;
        if payload.len() < size
        {
            return Err(Error::Malformed("truncated native code resource"));
        }
        let descriptor = Descriptor::decode(&payload[..size])?;
        if descriptor.role != Role::CodeResource
        {
            return Err(Error::Malformed("native resource has the wrong executable role"));
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
        let mut code = Vec::new();
        code.try_reserve_exact(end)
            .map_err(|_| Error::AllocationFailed("native code resource body allocation failed"))?;
        let mut relocations = Vec::new();
        relocations.try_reserve_exact(body.len() - end).map_err(|_| {
                       Error::AllocationFailed("native code resource relocation allocation failed")
                   })?;
        code.extend_from_slice(&body[..end]);
        relocations.extend_from_slice(&body[end..]);
        Ok(NativeCode { descriptor,
                        code,
                        relocations })
    }
}


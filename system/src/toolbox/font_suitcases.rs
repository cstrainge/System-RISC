//! Owner-bound immutable family-list snapshots; inspection never installs fonts.
#![forbid(unsafe_code)]
use alloc::{string::String, vec::Vec};

use mac_abi::{quickdraw::*, *};

use crate::toolbox::handles::{CapacityPolicy, KindCode, KindLease, OwnerRef, Store};

pub(crate) fn inspect(data: &[u8], resource: &[u8]) -> Result<Vec<String>, Status>
{
    use crate::toolbox::quickdraw::fonts::{FontManager, InstalledFontData};
    let source = if matches!(data.get(..4), Some(b"\0\x01\0\0" | b"true" | b"OTTO" | b"ttcf"))
    {
        InstalledFontData::TrueType(data)
    }
    else if !resource.is_empty()
    {
        InstalledFontData::SuitcaseFork(resource)
    }
    else
    {
        return Err(NOT_SUPPORTED);
    };
    let families = FontManager::inspect_source(source).map_err(|error| {
                       serial!("FONT_INSPECTION_SOURCE_REJECTED status={} reason={:?}",
                               error.status(),
                               error.failure);
                       error.status()
                   })?;
    if families.len() > 256
    {
        return Err(NO_MEMORY);
    }
    let mut names = Vec::new();
    names.try_reserve_exact(families.len()).map_err(|_| NO_MEMORY)?;
    for family in families
    {
        if family.name.len() > 256
        {
            return Err(NO_MEMORY);
        }
        names.push(family.name);
    }
    Ok(names)
}

struct Preview
{
    owner: u64,
    state: PreviewState
}
enum PreviewState
{
    Loading(Input),
    Parsing,
    Ready(Vec<String>),
    Failed(Status)
}
pub(crate) struct Input
{
    data:              Vec<u8>,
    resource:          Vec<u8>,
    expected_data:     usize,
    expected_resource: usize
}
impl Input
{
    pub(crate) fn new(data: u32, resource: u32) -> Result<Self, Status>
    {
        if data as u64 + resource as u64 > FONT_SUITCASE_MAX_BYTES as u64
        {
            return Err(NO_MEMORY);
        }
        let mut input = Self { data:              Vec::new(),
                               resource:          Vec::new(),
                               expected_data:     data as usize,
                               expected_resource: resource as usize };
        input.data.try_reserve_exact(data as usize).map_err(|_| NO_MEMORY)?;
        input.resource.try_reserve_exact(resource as usize).map_err(|_| NO_MEMORY)?;
        Ok(input)
    }

    pub(crate) fn inspect(self) -> Result<Vec<String>, Status>
    {
        inspect(&self.data, &self.resource)
    }
}
pub(crate) struct Manager
{
    slots: Store<Preview>
}
pub(crate) trait Host
{
    fn font_suitcases(&mut self) -> &mut Manager;
}
pub(crate) fn specification(storage: bool) -> mac_object_table::registry::Specification<'static>
{
    mac_object_table::registry::Specification {
        kind: KindCode(if storage { *b"FTST" } else { *b"FTSU" }),
        label: if storage { "Font suitcase inspection state" } else { "Font suitcase inspections" },
        provider: "System Font Manager",
        capacity: CapacityPolicy::HardBound {
            slots: if storage { 1 } else { 16 },
            reason: "Bounded read-only font inspection snapshots per owner"
        },
        source: mac_object_table::registry::Source::Resident
    }
}
pub(crate) struct ReleasePreviews(pub bool);
impl<C: Host> mac_object_table::registry::Release<C> for ReleasePreviews
{
    fn release(&self, context: &mut C, ticket: mac_object_table::domain::ReleasedResource)
               -> Result<(), mac_object_table::domain::CompletionError>
    {
        if self.0
        {
            context.font_suitcases().slots.release_storage(ticket)
        }
        else
        {
            context.font_suitcases().slots.release(ticket).map(drop)
        }
    }
}
fn status(error: mac_object_table::domain::PoolError) -> Status
{
    crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(error))
}
impl Manager
{
    pub(crate) fn new((objects, storage): (KindLease, KindLease)) -> Result<Self, Status>
    {
        Ok(Self { slots: Store::owned(objects, storage).map_err(status)? })
    }

    pub(crate) fn insert(&mut self, owner: &OwnerRef, input: Input, call: &mut FontSuitcaseCall)
                         -> Result<(), Status>
    {
        call.id = self.slots
                      .insert(owner,
                              Preview { owner: owner.owner().incarnation(),
                                        state: PreviewState::Loading(input) })
                      .map_err(status)?;
        call.count = 0;
        Ok(())
    }

    pub(crate) fn take_input(&mut self, owner: u64, id: u64) -> Result<Input, Status>
    {
        let preview = self.slots.get_mut(id).ok_or(NOT_FOUND)?;
        if preview.owner != owner
        {
            return Err(ACCESS_DENIED);
        }
        match &preview.state
        {
            PreviewState::Loading(input)
                if input.data.len() == input.expected_data
                   && input.resource.len() == input.expected_resource =>
            {
                ()
            }
            PreviewState::Failed(status) => return Err(*status),
            _ => return Err(BAD_ARGUMENT)
        }
        match core::mem::replace(&mut preview.state, PreviewState::Parsing)
        {
            PreviewState::Loading(input) => Ok(input),
            _ => Err(BAD_ABI)
        }
    }

    pub(crate) fn complete(&mut self, owner: u64, call: &mut FontSuitcaseCall,
                           names: Result<Vec<String>, Status>)
                           -> Result<(), Status>
    {
        let preview = self.slots.get_mut(call.id).ok_or(NOT_FOUND)?;
        if preview.owner != owner
        {
            return Err(ACCESS_DENIED);
        }
        if !matches!(preview.state, PreviewState::Parsing)
        {
            return Err(BAD_ARGUMENT);
        }
        match names
        {
            Ok(names) if !names.is_empty() =>
            {
                call.count = names.len() as u32;
                preview.state = PreviewState::Ready(names);
                Ok(())
            }
            result =>
            {
                let status = result.err().unwrap_or(NOT_FOUND);
                preview.state = PreviewState::Failed(status);
                Err(status)
            }
        }
    }

    pub(crate) fn request(&mut self, owner: u64, call: &mut FontSuitcaseCall, bytes: &[u8])
                          -> Result<(), Status>
    {
        let preview = self.slots.get_mut(call.id).ok_or(NOT_FOUND)?;
        if preview.owner != owner
        {
            return Err(ACCESS_DENIED);
        }
        match call.operation
        {
            FONT_SUITCASE_APPEND_DATA | FONT_SUITCASE_APPEND_RESOURCE =>
            {
                let PreviewState::Loading(input) = &mut preview.state
                else
                {
                    return Err(BAD_ARGUMENT);
                };
                let (buffer, expected) = if call.operation == FONT_SUITCASE_APPEND_DATA
                {
                    (&mut input.data, input.expected_data)
                }
                else
                {
                    (&mut input.resource, input.expected_resource)
                };
                if bytes.len() > expected.saturating_sub(buffer.len())
                {
                    return Err(BAD_ARGUMENT);
                }
                buffer.extend_from_slice(bytes);
                Ok(())
            }
            FONT_SUITCASE_FAMILY =>
            {
                let names = match &preview.state
                {
                    PreviewState::Ready(names) => names,
                    PreviewState::Failed(status) => return Err(*status),
                    _ => return Err(SERVICE_BUSY)
                };
                let name = names.get(call.index as usize).ok_or(NOT_FOUND)?;
                if name.len() > call.name.len()
                {
                    return Err(NO_MEMORY);
                }
                call.name.fill(0);
                call.name[..name.len()].copy_from_slice(name.as_bytes());
                call.name_length = name.len() as u32;
                call.count = names.len() as u32;
                Ok(())
            }
            FONT_SUITCASE_DISPOSE =>
            {
                drop(self.slots.remove(call.id).ok_or(NOT_FOUND)?);
                Ok(())
            }
            _ => Err(NOT_SUPPORTED)
        }
    }
}

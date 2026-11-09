//! Property service on canonical CTRL, with original owner/account admission.
//! Immutable buffer construction and the final destruction of replaced bytes
//! run outside Desktop. Concurrent mutation returns ServiceBusy, never silently
//! overwrites an unrelated field or swaps a control's definition.
#![forbid(unsafe_code)]
use mac_abi::controls::{GET_PROPERTIES, PropertiesCall, SET_PROPERTIES};

use super::*;
pub(crate) fn native_properties(call: &mut PropertiesCall, input: &[u8], output: &mut [u8])
                                -> Status
{
    let mut run = || -> Result<(), Status> {
        if !matches!(call.operation, GET_PROPERTIES | SET_PROPERTIES)
        {
            return Err(BAD_ARGUMENT);
        }
        let (snapshot, recipient, storage, allocation) =
            crate::kernel::with_toolbox(|runtime| {
                let (recipient, storage) = runtime.service_owners()?;
                let allocation = runtime.processes.allocation(runtime.owner)?;
                if allocation.owner() != storage.owner()
                {
                    return Err(ACCESS_DENIED);
                }
                let snapshot =
                    runtime.controls.property_snapshot(call.id, recipient.owner().incarnation())?;
                Ok((snapshot, recipient, storage, allocation))
            }).map_err(|e| e.status())??;
        if call.revision != 0 && call.revision != snapshot.revision()
        {
            return Err(SERVICE_BUSY);
        }
        let target =
            crate::allocator::Target::Owner { account: allocation,
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        call.revision = snapshot.revision();
        match call.operation
        {
            GET_PROPERTIES =>
            {
                if !input.is_empty()
                {
                    return Err(BAD_ARGUMENT);
                }
                let required =
                    crate::allocator::with_target(target, || snapshot.read(call.field, output))?;
                call.required = required;
                if output.len() < required
                {
                    return Err(NO_MEMORY);
                }
                Ok(())
            }
            SET_PROPERTIES =>
            {
                if !output.is_empty()
                {
                    return Err(BAD_ARGUMENT);
                }
                let prepared =
                    crate::allocator::with_target(target, || snapshot.prepare(call.field, input))?;
                call.revision = crate::kernel::with_toolbox(|runtime| {
                                    let (current_recipient, current_storage) =
                                        runtime.service_owners()?;
                                    if !recipient.accepting()
                                       || current_recipient.owner() != recipient.owner()
                                       || current_storage.owner() != storage.owner()
                                    {
                                        return Err(ACCESS_DENIED);
                                    }
                                    runtime.controls.install_properties(&prepared)
                                }).map_err(|e| e.status())??;
                Ok(())
            }
            _ => Err(BAD_ARGUMENT)
        }
    };
    run().map_or_else(|status| status, |()| OK)
}

pub(crate) fn begin_message(
    call: &PropertiesCall, input: &[u8])
    -> Result<Option<crate::toolbox::resources::definitions::callbacks::Pending>, Status>
{
    use crate::toolbox::resources::definitions::callbacks::{self, CompletionTarget};
    if call.operation != mac_abi::controls::SEND_MESSAGE
       || call.capacity != 0
       || call.field != mac_abi::controls::ALL_PROPERTIES
    {
        return Err(BAD_ARGUMENT);
    }
    let (snapshot, allocation) =
        crate::kernel::with_toolbox(|runtime| {
            let (recipient, storage) = runtime.service_owners()?;
            let allocation = runtime.processes.allocation(runtime.owner)?;
            if !recipient.accepting() || allocation.owner() != storage.owner()
            {
                return Err(ACCESS_DENIED);
            }
            Ok((runtime.controls.property_snapshot(call.id, recipient.owner().incarnation())?,
                allocation))
        }).map_err(|e| e.status())??;
    if call.revision != 0 && call.revision != snapshot.revision()
    {
        return Err(SERVICE_BUSY);
    }
    let target = crate::allocator::Target::Owner { account: allocation.clone(),
                                                   class:
                                                       mac_kernel_model::policy::MemoryClass::Kernel };
    crate::allocator::with_target(target, || {
        let (reference, request) = snapshot.message(input)?;
        if !crate::toolbox::resources::definitions::is_user(reference)?
        {
            let callbacks::packet::Object::Control(mut state) = request.object
            else
            {
                return Err(BAD_ABI);
            };
            let callbacks::packet::Arguments::ControlMessage(bytes) = request.arguments
            else
            {
                return Err(BAD_ABI);
            };
            let data = mac_abi::controls::MessageData { data:   bytes.as_ptr(),
                                                        length: bytes.len() };
            let props = request.properties.as_ref().map_or(&[][..], |p| p.as_bytes());
            let mut call =
                mac_abi::DefinitionCall { properties:        props.as_ptr(),
                                          properties_length: props.len(),
                                          header:
                                              mac_abi::Header::for_type::<mac_abi::DefinitionCall>(),
                                          kind:              request.kind,
                                          id:                request.id,
                                          variation:         request.variation,
                                          message:           request.message,
                                          reserved:          0,
                                          param:             core::ptr::from_ref(&data).addr()
                                                             as i64,
                                          object:            core::ptr::from_mut(&mut state).cast(),
                                          surface:           core::ptr::null_mut(),
                                          gate:              crate::machine::traps::gateway,
                                          reflow:            request.reflow };
            let result = crate::toolbox::resources::definitions::invoke(reference, &mut call);
            if result != 0
            {
                return Err(i32::try_from(result).ok().filter(|s| *s < 0).unwrap_or(BAD_ABI));
            }
            Ok(None)
        }
        else
        {
            callbacks::enqueue_for_object(reference,request,CompletionTarget::Thread(crate::context::current_thread_context().identity().thread),&allocation).map(Some).map_err(callbacks::EnqueueError::status)
        }
    })
}

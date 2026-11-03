//! Native direct-definition callers use a data-only DTO. Process payloads stay
//! opaque U addresses; known trusted families use the envelope's checked copies.
use mac_abi::{definitions::*, *};

use super::{Pending,
            packet::{Arguments, Object, Request}};
use crate::machine::traps::user::DirectArguments;

pub(super) fn begin(call: &UserCall, arguments: DirectArguments) -> Result<Pending, Status>
{
    if !call.header.supports::<UserCall>() || call.reserved != 0 || !arguments.process
    {
        return Err(BAD_ARGUMENT);
    }
    let request = Request { kind:      call.kind,
                            id:        call.id,
                            variation: call.variation,
                            message:   call.message,
                            reflow:    call.reflow,
                            object:    Object::Opaque { owner:    arguments.owner,
                                                        image_id: arguments.image_id,
                                                        object:   arguments.object,
                                                        surface:  arguments.surface },
                            arguments: Arguments::Scalar(arguments.param),
                            surface:   None };
    super::enqueue(arguments.reference,
                   request,
                   crate::context::current_thread_context().identity().thread)
}

/// # Safety
/// The original user envelope owns and validates every known-family nested
/// record and pixel array. No address in this snapshot names unpinned U memory.
pub(super) unsafe fn invoke_user_trusted(call: &mut UserCall, arguments: DirectArguments)
                                         -> Result<(), Status>
{
    if arguments.process || super::super::is_user(arguments.reference)?
    {
        return Err(ACCESS_DENIED);
    }
    unsafe {
        validate_input(call)?;
    }
    call.result = unsafe { invoke(arguments.reference, call) };
    Ok(())
}

/// # Safety
/// This is the trusted S-mode DTO boundary only. The calling System/module owns
/// valid exclusive family state and surfaces for the synchronous invocation.
/// User requests must use invoke_user_trusted and the captured envelope above.
pub(crate) unsafe fn dispatch_supervisor(call: &mut UserCall) -> i64
{
    if !call.header.supports::<UserCall>() || call.reserved != 0
    {
        return BAD_ABI as i64;
    }
    let entry = match crate::loader::prepare_definition(call.kind.to_be_bytes(), call.id)
    {
        Ok(entry) => entry,
        Err(cause) => return crate::loader::status(cause) as i64
    };
    match super::super::is_user(entry)
    {
        Ok(false) => (),
        Ok(true) => return SERVICE_BUSY as i64,
        Err(status) => return status as i64
    }
    call.result = unsafe { invoke(entry, call) };
    OK as i64
}

unsafe fn invoke(entry: super::super::DefinitionRef, call: &UserCall) -> i64
{
    let mut definition = DefinitionCall { header:    Header::for_type::<DefinitionCall>(),
                                          kind:      call.kind,
                                          id:        call.id,
                                          variation: call.variation,
                                          message:   call.message,
                                          reserved:  0,
                                          param:     call.param,
                                          object:    call.object as *mut (),
                                          surface:   call.surface as *mut Surface,
                                          gate:      crate::machine::traps::gateway,
                                          reflow:    call.reflow };
    super::super::invoke(entry, &mut definition)
}

unsafe fn validate_input(call: &UserCall) -> Result<(), Status>
{
    if !call.header.supports::<UserCall>() || call.reserved != 0 || call.object == 0
    {
        return Err(BAD_ARGUMENT);
    }
    match call.kind
    {
        WDEF =>
        {
            let value = unsafe { &*(call.object as *const WindowDefinitionState) };
            text(&value.title, value.title_length)?;
            if value.structure_piece_count > 2
            {
                return Err(BAD_ARGUMENT);
            }
        }
        CDEF =>
        {
            if call.reflow > 1
            {
                return Err(BAD_ARGUMENT);
            }
            if call.message == controls::CHANGE_BOUNDS_DEFINITION
            {
                let value = unsafe { &*(call.object as *const controls::ControlChange) };
                text(&value.before.title, value.before.title_length)?;
                text(&value.after.title, value.after.title_length)?;
            }
            else if call.id == 1
            {
                mac_definition::validation::scrollbar(unsafe {
                    &*(call.object as *const controls::ScrollbarState)
                }).map_err(|cause| cause.status())?;
            }
            else
            {
                let value = unsafe { &*(call.object as *const ControlDefinitionState) };
                mac_definition::validation::standard(call.id, call.variation, value)
                    .map_err(|cause| cause.status())?;
            }
        }
        menus::MDEF =>
        {
            let value = unsafe { &*(call.object as *const menus::MenuDefinitionState) };
            text(&value.title, value.title_length)?;
            if value.item_count > menus::MAX_ITEMS as u32
            {
                return Err(BAD_ARGUMENT);
            }
            for item in &value.items[..value.item_count as usize]
            {
                text(&item.text, item.text_length)?;
            }
        }
        _ => return Err(NOT_SUPPORTED)
    }
    Ok(())
}

fn text(bytes: &[u8], length: u32) -> Result<(), Status>
{
    let text = bytes.get(..length as usize).ok_or(BAD_ARGUMENT)?;
    core::str::from_utf8(text).map(|_| ()).map_err(|_| BAD_ARGUMENT)
}

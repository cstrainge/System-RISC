use super::*;

pub(super) fn invoke(state: &mut ControlCall, payload: &mut mac_abi::controls::reflow::Payload,
                     entry: DefinitionEntry, gate: TrapGate)
                     -> Result<(), Status>
{
    let id = definition_id(state)?;
    let object = if id == 1
    {
        (&mut state.scrollbar as *mut ScrollbarState).cast()
    }
    else
    {
        (&mut state.standard as *mut ControlDefinitionState).cast()
    };
    let mut call = DefinitionCall { header: Header::for_type::<DefinitionCall>(),
                                    kind: CDEF,
                                    id,
                                    variation: variation(state.procedure, id),
                                    message: payload.message,
                                    reserved: 0,
                                    param: (payload as *mut mac_abi::controls::reflow::Payload)
                                           as usize
                                           as i64,
                                    object,
                                    surface: core::ptr::null_mut(),
                                    gate,
                                    reflow: 1 };
    let status = super::super::resources::definitions::invoke(entry, &mut call);
    if status == 0
    {
        Ok(())
    }
    else if status < 0
    {
        Err(status as Status)
    }
    else
    {
        Err(BAD_ABI)
    }
}

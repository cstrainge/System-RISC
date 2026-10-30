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
        Err(Status::try_from(status).unwrap_or(BAD_ABI))
    }
    else
    {
        Err(BAD_ABI)
    }
}

/// System lends disjoint scratch arrays, never caller pointers or object-store
/// borrows. The loaded image remains owned by the definition cache throughout.
pub(super) fn grid_axis(entry: DefinitionEntry, gate: TrapGate,
                        constraint: mac_abi::controls::reflow::AxisConstraint,
                        tracks: &mut [mac_abi::controls::reflow::GridTrack],
                        items: &mut [mac_abi::controls::reflow::GridItem])
                        -> Result<(i64, i64), Status>
{
    use mac_abi::controls::reflow as wire;
    let mut axis = wire::GridAxisCall { constraint,
                                        tracks: tracks.as_mut_ptr(),
                                        track_count: tracks.len(),
                                        items: items.as_mut_ptr(),
                                        item_count: items.len(),
                                        minimum: 0,
                                        preferred: 0 };
    let mut state = ControlDefinitionState::default();
    let mut call = DefinitionCall { header: Header::for_type::<DefinitionCall>(),
                                    kind: CDEF,
                                    id: CDEF_TABLE,
                                    variation: 0,
                                    message: wire::CALCULATE_GRID_AXIS,
                                    reserved: 0,
                                    param: (&mut axis as *mut wire::GridAxisCall) as usize
                                           as i64,
                                    object: (&mut state as *mut ControlDefinitionState).cast(),
                                    surface: core::ptr::null_mut(),
                                    gate,
                                    reflow: 1 };
    let result = super::super::resources::definitions::invoke(entry, &mut call);
    if result != 0
    {
        return Err(if result < 0 { Status::try_from(result).unwrap_or(BAD_ABI) } else { BAD_ABI });
    }
    if axis.minimum < 0 || axis.preferred < axis.minimum
    {
        return Err(BAD_ABI);
    }
    let mut end = 0i64;
    let mut minimum = 0i64;
    let mut preferred = 0i64;
    for track in tracks.iter()
    {
        if track.position != end
           || track.assigned < 0
           || track.minimum < 0
           || track.preferred < track.minimum
        {
            return Err(BAD_ABI);
        }
        end = end.checked_add(track.assigned).ok_or(BAD_ABI)?;
        minimum = minimum.checked_add(track.minimum).ok_or(BAD_ABI)?;
        preferred = preferred.checked_add(track.preferred).ok_or(BAD_ABI)?;
    }
    if (minimum, preferred) != (axis.minimum, axis.preferred)
    {
        return Err(BAD_ABI);
    }
    if constraint.kind != wire::UNBOUNDED && end > i64::from(constraint.extent)
    {
        return Err(BAD_ABI);
    }
    for item in items
    {
        let end_index =
            item.start.checked_add(item.count).filter(|_| item.count != 0).ok_or(BAD_ABI)?;
        let first = tracks.get(item.start as usize).ok_or(BAD_ABI)?;
        let last = tracks.get((end_index - 1) as usize).ok_or(BAD_ABI)?;
        let cell_end = last.position.checked_add(last.assigned).ok_or(BAD_ABI)?;
        let item_end = item.position.checked_add(item.extent).ok_or(BAD_ABI)?;
        if item.position < first.position || item.extent < 0 || item_end > cell_end
        {
            return Err(BAD_ABI);
        }
    }
    Ok((axis.minimum, axis.preferred))
}

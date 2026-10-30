//! Window-root placement. Existing CTRL objects remain the authoritative roots;
//! no second identity or ownership registry is introduced.
use mac_abi::controls::reflow as wire;

use super::*;

/// Clamp oversized offsets into the content area, then shrink to the remaining
/// span. Widen before subtraction; every returned endpoint fits local i16.
pub(super) fn axis(total: i16, before: i16, after: i16, preferred: i16, leading: bool,
                   trailing: bool)
                   -> (i16, i16)
{
    let start = before.min(total);
    let end = (i32::from(total) - i32::from(after)).max(i32::from(start)) as i16;
    let available = end - start;
    let extent = if leading && trailing { available } else { preferred.min(available) };
    let position = if leading
    {
        start
    }
    else if trailing
    {
        end - extent
    }
    else
    {
        start + (available - extent) / 2
    };
    (position, extent)
}
impl Manager
{
    pub(crate) fn is_root(&self, id: u64, owner: u64) -> Result<bool, Status>
    {
        Ok(self.get(id, owner)?.tree.parent == 0)
    }

    /// Prepare all roots against the proposed window size, or one explicit
    /// property edit against its current size. Geometry and metadata publish
    /// together only after every root and retained command has been admitted.
    pub(crate) fn prepare_window(&self, window: u64, viewport: Rect, paint_pending: bool,
                                 edit: Option<(&ControlCall, u64)>, gate: TrapGate,
                                 mut resolve: impl FnMut(i16) -> Result<DefinitionEntry, Status>)
                                 -> Result<Prepared, Status>
    {
        layout::LayoutRect::try_from(viewport).map_err(|e| e.status())?;
        if viewport.left != 0 || viewport.top != 0
        {
            return Err(BAD_ARGUMENT);
        }
        let mut prepared = Prepared::empty();
        for (id, object) in
            self.objects.iter().filter(|(_, o)| o.window == window && o.tree.parent == 0)
        {
            if edit.is_some_and(|(call, _)| call.id != id)
            {
                continue;
            }
            let Some(l) = &object.state.layout
            else
            {
                if edit.is_some()
                {
                    return Err(NOT_SUPPORTED);
                }
                continue;
            };
            let mut anchors = l.anchors;
            let mut border = l.window_border;
            if let Some((call, owner)) = edit
            {
                self.get(id, owner)?;
                if call.layout.reserved != 0
                {
                    return Err(BAD_ARGUMENT);
                }
                match call.layout.message
                {
                    wire::SET_ANCHORS =>
                    {
                        anchors =
                            call.layout.anchors.try_into().map_err(|e: layout::Error| e.status())?
                    }
                    wire::SET_WINDOW_BORDER =>
                    {
                        border = call.layout
                                     .window_border
                                     .try_into()
                                     .map_err(|e: layout::Error| e.status())?
                    }
                    _ => return Err(NOT_SUPPORTED)
                }
            }
            let changed = anchors != l.anchors || border != l.window_border;
            if anchors == layout::Anchors::NONE
            {
                if changed
                {
                    let mut state = object.state.clone();
                    let l = state.layout.as_mut().ok_or(CORRUPT_DATA)?;
                    l.anchors = anchors;
                    l.window_border = border;
                    l.window_extent = None;
                    return Ok(Prepared::metadata(id, state));
                }
                continue;
            }
            if !changed
               && !l.arrangement_pending
               && l.window_extent == Some((viewport.right, viewport.bottom))
            {
                continue;
            }
            let call = ControlCall { id,
                                     operation: REFLOW_OBJECT,
                                     layout: wire::Payload { message: wire::ASSIGN_RECT,
                                                             rect: viewport,
                                                             ..Default::default() },
                                     ..Default::default() };
            prepared.extend(self.prepare_layout(&call,
                                                object.recipient,
                                                paint_pending,
                                                gate,
                                                &mut resolve,
                                                Some((viewport, anchors, border)))?)?;
        }
        Ok(prepared)
    }
}

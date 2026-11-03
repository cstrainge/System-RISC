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
    /// Classification for the resident non-suspending geometry boundary. This
    /// grants no source/Window authority; owned layout still checks full keys.
    pub(crate) fn window_geometry_needs_staging(&self, window: u64) -> Result<bool, Status>
    {
        for (_, object) in self.objects.iter().filter(|(_, object)| object.window == window)
        {
            if object.definition.is_process_source()
               && crate::toolbox::resources::definitions::is_user(object.definition.reference())?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn is_root(&self, id: u64, owner: u64) -> Result<bool, Status>
    {
        Ok(self.get(id, owner)?.tree.parent == 0)
    }

    /// Prepare all roots against the proposed window size, or one explicit
    /// property edit against its current size. Geometry and metadata publish
    /// together only after every root and retained command has been admitted.
    pub(crate) fn prepare_window(&self, window: u64, viewport: Rect, paint_pending: bool,
                                 edit: Option<(&ControlCall, u64)>, gate: TrapGate)
                                 -> Result<Prepared, Status>
    {
        Self::check_viewport(viewport)?;
        let mut prepared = Prepared::empty();
        let author = super::super::effects::Author::trusted(gate);
        for (id, object) in
            self.objects.iter().filter(|(_, o)| o.window == window && o.tree.parent == 0)
        {
            if let Some(snapshot) =
                self.window_root_snapshot(id, object, viewport, paint_pending, edit)?
            {
                prepared.extend(super::super::effects::immediate(snapshot.calculate(&author))?)?;
            }
        }
        Ok(prepared)
    }

    pub(super) fn check_viewport(viewport: Rect) -> Result<(), Status>
    {
        layout::LayoutRect::try_from(viewport).map_err(|e| e.status())?;
        if viewport.left != 0 || viewport.top != 0
        {
            return Err(BAD_ARGUMENT);
        }
        Ok(())
    }

    pub(super) fn window_root_snapshot(&self, id: u64, object: &Object, viewport: Rect,
                                       paint_pending: bool, edit: Option<(&ControlCall, u64)>)
                                       -> Result<Option<grid::LayoutSnapshot>, Status>
    {
        if edit.is_some_and(|(call, _)| call.id != id)
        {
            return Ok(None);
        }
        let Some(l) = &object.state.layout
        else
        {
            if edit.is_some()
            {
                return Err(NOT_SUPPORTED);
            }
            return Ok(None);
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
                return Ok(Some(grid::LayoutSnapshot::Ready(Prepared::metadata(id, state))));
            }
            return Ok(None);
        }
        if !changed
           && !l.arrangement_pending
           && l.window_extent == Some((viewport.right, viewport.bottom))
        {
            return Ok(None);
        }
        let call = ControlCall { id,
                                 operation: REFLOW_OBJECT,
                                 layout: wire::Payload { message: wire::ASSIGN_RECT,
                                                         rect: viewport,
                                                         ..Default::default() },
                                 ..Default::default() };
        self.snapshot_layout_change(&call,
                                    object.recipient,
                                    paint_pending,
                                    Some((viewport, anchors, border)),
                                    None,
                                    None)
            .map(Some)
    }
}

impl Manager
{
    pub(crate) fn needs_scrollbar_definition(&self, call: &ControlCall, owner: u64)
                                             -> Result<bool, Status>
    {
        if call.operation != REFLOW_OBJECT
           || call.layout.message != wire::SET_SCROLLING
           || call.layout.scroll.axes == 0
        {
            return Ok(false);
        }
        let object = self.get(call.id, owner)?;
        let layout = object.state.layout.as_ref().ok_or(NOT_SUPPORTED)?;
        Ok(layout.scroll.as_ref().map_or(0, |s| s.data.axes) != call.layout.scroll.axes)
    }

    /// Pure election for the existing table/root-property transaction. Only
    /// actual bound Process definitions require an owned author continuation.
    pub(crate) fn user_layout_operation(&self, call: &ControlCall, owner: u64, viewport: Rect)
                                        -> Result<bool, Status>
    {
        if call.operation == HIT_CHILDREN_OBJECT
        {
            self.table(call.id, owner)?;
            if call.reflow != 0
               || !call.surface.is_null()
               || call.reserved != 0
               || call.layout.reserved != 0
            {
                return Err(BAD_ARGUMENT);
            }
            let mut id = call.id;
            loop
            {
                let object = self.get(id, owner)?;
                if object.definition.is_process_source() || object.state.process_dependencies()
                {
                    return Ok(true);
                }
                id = self.next_in(call.id, id);
                if id == 0
                {
                    return Ok(false);
                }
            }
        }
        if call.operation != REFLOW_OBJECT
        {
            return Ok(false);
        }
        let object = self.get(call.id, owner)?;
        let Some(l) = &object.state.layout
        else
        {
            return Ok(false);
        };
        let window_edit =
            matches!(call.layout.message, wire::SET_ANCHORS | wire::SET_WINDOW_BORDER)
            && (call.layout.message == wire::SET_WINDOW_BORDER || object.tree.parent == 0);
        let table = matches!(object.state.spec,
                             Spec::Standard { procedure: TABLE_PROCEDURE,
                                              .. });
        let scroll = matches!(call.layout.message,
                              wire::SET_SCROLLING | wire::SCROLL_TO | wire::REVEAL_CHILD);
        if !window_edit
           && !(table
                && (scroll || matches!(call.layout.message, wire::ASK_SIZE | wire::ASSIGN_RECT)))
        {
            return Ok(false);
        }
        if call.reflow != 0
           || !call.surface.is_null()
           || call.reserved != 0
           || call.layout.reserved != 0
        {
            return Err(BAD_ARGUMENT);
        }
        if scroll
        {
            if call.layout.message == wire::SET_SCROLLING
            {
                if call.layout.scroll.axes & !3 != 0
                {
                    return Err(BAD_ARGUMENT);
                }
                if call.layout.scroll.axes == l.scroll.as_ref().map_or(0, |s| s.data.axes)
                {
                    return Ok(false);
                }
            }
            else if l.scroll.is_none()
            {
                return Err(NOT_SUPPORTED);
            }
            if call.layout.message == wire::SCROLL_TO
               && (call.layout.scroll.x < 0 || call.layout.scroll.y < 0)
            {
                return Err(BAD_ARGUMENT);
            }
            if call.layout.message == wire::REVEAL_CHILD
               && self.get(call.layout.child, owner)?.tree.parent != call.id
            {
                return Err(BAD_ARGUMENT);
            }
            if l.scroll.as_ref().is_some_and(|scroll| scroll.definition.is_process_source())
            {
                return Ok(true);
            }
        }
        else if window_edit
        {
            if object.tree.parent != 0
            {
                return Err(BAD_ARGUMENT);
            }
            Self::check_viewport(viewport)?;
            let anchors = if call.layout.message == wire::SET_ANCHORS
            {
                call.layout.anchors.try_into().map_err(|error: layout::Error| error.status())?
            }
            else
            {
                l.anchors
            };
            let border = if call.layout.message == wire::SET_WINDOW_BORDER
            {
                call.layout
                    .window_border
                    .try_into()
                    .map_err(|error: layout::Error| error.status())?
            }
            else
            {
                l.window_border
            };
            if anchors == layout::Anchors::NONE
            {
                return Ok(false);
            }
            if anchors == l.anchors
               && border == l.window_border
               && !l.arrangement_pending
               && l.window_extent == Some((viewport.right, viewport.bottom))
            {
                return Ok(false);
            }
        }
        else if call.layout.message == wire::ASK_SIZE
        {
            let constraints: layout::Constraints =
                call.layout
                    .constraints
                    .try_into()
                    .map_err(|error: layout::Error| error.status())?;
            if (constraints == Default::default() && l.intrinsic.is_some())
               || l.measured.is_some_and(|(key, _)| key == constraints)
            {
                return Ok(false);
            }
        }
        else if call.layout.rect == object.state.spec.bounds() && !l.arrangement_pending
        {
            return Ok(false);
        }
        let mut id = call.id;
        loop
        {
            if self.get(id, owner)?.definition.is_process_source()
            {
                return Ok(true);
            }
            id = self.next_in(call.id, id);
            if id == 0
            {
                return Ok(false);
            }
        }
    }
}

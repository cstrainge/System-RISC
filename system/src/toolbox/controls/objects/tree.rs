//! One resident ownership tree. Its existing CTRL IDs also define child order,
//! clipping, hit routing and disposal. Mutations run under the desktop guard;
//! they never call a CDEF, borrow application memory or allocate after validation.
use mac_abi::controls::reflow as wire;

use super::*;

#[derive(Clone, Copy)]
pub(super) struct Node
{
    pub parent:       u64,
    pub(super) first: u64,
    last:             u64,
    previous:         u64,
    pub(super) next:  u64,
    revision:         u64,
    order:            u64,
    pub(super) cell:  wire::Cell,
    pub(super) span:  wire::Span
}
impl Node
{
    pub(super) fn new(order: u64) -> Self
    {
        Self { parent: 0,
               first: 0,
               last: 0,
               previous: 0,
               next: 0,
               revision: 1,
               order,
               cell: Default::default(),
               span: Default::default() }
    }

    pub(super) fn relation(self) -> wire::Relation
    {
        wire::Relation { parent:   self.parent,
                         revision: self.revision,
                         child:    self.first,
                         cell:     self.cell,
                         span:     self.span }
    }
}

pub(super) fn clip(a: Option<Rect>, b: Option<Rect>) -> Option<Rect>
{
    match (a, b)
    {
        (Some(a), Some(b)) => Some(super::super::intersection(a, b)),
        (a, b) => a.or(b)
    }
}

impl Manager
{
    /// Bounded, release-active structural validation at mutation boundaries.
    /// Refuse a broken relationship before changing another object; do not
    /// replace a stale/foreign reference with an unchecked store lookup.
    fn check_links(&self, id: u64, owner: u64) -> Result<(), Status>
    {
        if id == 0
        {
            return Ok(());
        }
        let n = self.get(id, owner)?.tree;
        if (n.first == 0) != (n.last == 0)
        {
            return Err(CORRUPT_DATA);
        }
        if n.first != 0
        {
            let first = self.get(n.first, owner)?.tree;
            let last = self.get(n.last, owner)?.tree;
            if first.parent != id || first.previous != 0 || last.parent != id || last.next != 0
            {
                return Err(CORRUPT_DATA);
            }
        }
        if n.parent == 0
        {
            if n.previous != 0 || n.next != 0
            {
                return Err(CORRUPT_DATA);
            }
        }
        else
        {
            let parent = self.get(n.parent, owner)?.tree;
            if n.previous == 0
            {
                if parent.first != id
                {
                    return Err(CORRUPT_DATA);
                }
            }
            else
            {
                let previous = self.get(n.previous, owner)?.tree;
                if previous.parent != n.parent || previous.next != id || previous.cell >= n.cell
                {
                    return Err(CORRUPT_DATA);
                }
            }
            if n.next == 0
            {
                if parent.last != id
                {
                    return Err(CORRUPT_DATA);
                }
            }
            else
            {
                let next = self.get(n.next, owner)?.tree;
                if next.parent != n.parent || next.previous != id || next.cell <= n.cell
                {
                    return Err(CORRUPT_DATA);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn dispose(&mut self, id: u64, owner: u64) -> Result<(), Status>
    {
        if self.get(id, owner)?.tree.parent != 0
        {
            return Err(ACCESS_DENIED);
        }
        self.remove_tree(id)
    }

    pub(super) fn table(&self, id: u64, owner: u64) -> Result<&Object, Status>
    {
        let object = self.get(id, owner)?;
        if object.state.layout.is_none()
           || !matches!(object.state.spec,
                        Spec::Standard { procedure: TABLE_PROCEDURE,
                                         .. })
        {
            return Err(NOT_SUPPORTED);
        }
        Ok(object)
    }

    pub(super) fn preflight_tree_configuration(&self, call: &ControlCall, owner: u64)
                                               -> Result<(), Status>
    {
        let object = self.get(call.id, owner)?;
        if object.tree.first == 0
        {
            return Ok(());
        }
        let geometry_changes = match call.operation
        {
            CONFIGURE_OBJECT =>
            {
                if call.procedure != TABLE_PROCEDURE
                {
                    return Err(NOT_SUPPORTED);
                }
                call.standard.bounds != object.state.spec.bounds()
            }
            REFLOW_OBJECT if call.layout.message == wire::ASSIGN_RECT =>
            {
                call.layout.rect != object.state.spec.bounds()
            }
            REFLOW_OBJECT if call.layout.message == wire::SET_VISIBLE_RECT =>
            {
                object.state.layout.as_ref().and_then(|l| l.visible) != Some(call.layout.rect)
            }
            _ => false
        };
        if geometry_changes
        {
            let mut id = object.tree.first;
            while id != 0
            {
                if self.get(id, owner)?.state.tracking.is_some()
                {
                    return Err(SERVICE_BUSY);
                }
                id = self.next_in(call.id, id);
            }
        }
        // Populated geometry goes through the subtree transaction, never a raw
        // leaf configuration that would leave child rectangles behind.
        if call.operation == CONFIGURE_OBJECT && call.standard.bounds != object.state.spec.bounds()
        {
            return Err(NOT_SUPPORTED);
        }
        Ok(())
    }

    pub(crate) fn changes_tree_geometry(&self, call: &ControlCall, owner: u64)
                                        -> Result<bool, Status>
    {
        if !self.in_tree(call.id)
        {
            return Ok(false);
        }
        let o = self.get(call.id, owner)?;
        Ok(match call.operation
        {
            CONFIGURE_OBJECT =>
            {
                Spec::from_call(call, o.state.spec.scrollbar().is_some()).bounds()
                != o.state.spec.bounds()
            }
            REFLOW_OBJECT if call.layout.message == wire::ASSIGN_RECT =>
            {
                call.layout.rect != o.state.spec.bounds()
            }
            REFLOW_OBJECT if call.layout.message == wire::SET_VISIBLE_RECT =>
            {
                o.state.layout.as_ref().and_then(|l| l.visible) != Some(call.layout.rect)
            }
            _ => false
        })
    }

    /// Iteration observes one membership revision. A concurrent successful edit
    /// invalidates the cursor rather than silently skipping or repeating children.
    pub(crate) fn next_child(&self, call: &mut ControlCall, owner: u64) -> Result<(), Status>
    {
        let parent = self.table(call.id, owner)?;
        let version = call.relation.revision;
        let child = call.relation.child;
        if version == 0 && child != 0
        {
            return Err(BAD_ARGUMENT);
        }
        if version != 0 && version != parent.tree.revision
        {
            return Err(SERVICE_BUSY);
        }
        let next = if child == 0
        {
            parent.tree.first
        }
        else
        {
            let previous = self.get(child, owner)?;
            if previous.tree.parent != call.id
            {
                return Err(BAD_ARGUMENT);
            }
            previous.tree.next
        };
        call.relation = wire::Relation { parent:   call.id,
                                         revision: parent.tree.revision,
                                         child:    next,
                                         cell:     if next == 0
                                         {
                                             Default::default()
                                         }
                                         else
                                         {
                                             self.get(next, owner)?.tree.cell
                                         },
                                         span:     if next == 0
                                         {
                                             Default::default()
                                         }
                                         else
                                         {
                                             self.get(next, owner)?.tree.span
                                         } };
        Ok(())
    }

    /// Preflight everything that may refuse before updating any edge. Occupied
    /// cells refuse; cross-window transfer is deliberately unsupported. A live
    /// capture must be explicitly cancelled before moving its subtree.
    pub(crate) fn change_parent(&mut self, call: &mut ControlCall, owner: u64, paint_pending: bool)
                                -> Result<bool, Status>
    {
        let payload = call.layout;
        if payload.reserved != 0
        {
            return Err(BAD_ARGUMENT);
        }
        if (payload.message == wire::REPARENT && payload.parent == 0)
           || (payload.message != wire::REPARENT && payload.parent != 0)
        {
            return Err(BAD_ARGUMENT);
        }
        let old = match payload.message
        {
            wire::SET_CHILD_INDEXED => 0,
            wire::REMOVE_CHILD | wire::REPARENT => call.id,
            _ => return Err(NOT_SUPPORTED)
        };
        let new = match payload.message
        {
            wire::REMOVE_CHILD => 0,
            wire::REPARENT => payload.parent,
            _ => call.id
        };
        if new == 0 && old == 0
        {
            return Err(BAD_ARGUMENT);
        }
        let child = self.get(payload.child, owner)?;
        if child.state.layout.is_none()
        {
            return Err(NOT_SUPPORTED);
        }
        if child.tree.parent != old
        {
            return Err(BAD_ARGUMENT);
        }
        if old != 0
        {
            self.table(old, owner)?;
        }
        let mut previous = 0;
        let mut next = 0;
        if new != 0
        {
            let end_row = payload.cell.row.checked_add(payload.span.rows).ok_or(BAD_ARGUMENT)?;
            let end_col =
                payload.cell.column.checked_add(payload.span.columns).ok_or(BAD_ARGUMENT)?;
            layout::Span::try_from(payload.span).map_err(|e| e.status())?;
            let parent = self.table(new, owner)?;
            if parent.window == 0 || parent.window != child.window
            {
                return Err(NOT_SUPPORTED);
            }
            let mut ancestor = new;
            while ancestor != 0
            {
                if ancestor == payload.child
                {
                    return Err(BAD_ARGUMENT);
                }
                ancestor = self.get(ancestor, owner)?.tree.parent;
            }
            let mut at = parent.tree.first;
            while at != 0
            {
                let row = self.get(at, owner)?;
                if at != payload.child
                {
                    if row.tree.cell.row < end_row
                       && row.tree.cell.column < end_col
                       && payload.cell.row
                          < row.tree.cell.row.checked_add(row.tree.span.rows).ok_or(CORRUPT_DATA)?
                       && payload.cell.column
                          < row.tree
                               .cell
                               .column
                               .checked_add(row.tree.span.columns)
                               .ok_or(CORRUPT_DATA)?
                    {
                        return Err(ALREADY_EXISTS);
                    }
                    if row.tree.cell > payload.cell
                    {
                        if next == 0
                        {
                            next = at;
                        }
                    }
                    else
                    {
                        previous = at;
                    }
                }
                at = row.tree.next;
            }
        }
        if old == new && child.tree.cell == payload.cell && child.tree.span == payload.span
        {
            return Ok(false);
        }
        for id in [payload.child, old, new, previous, next]
        {
            self.check_links(id, owner)?;
        }
        if paint_pending
        {
            return Err(SERVICE_BUSY);
        }
        let mut at = payload.child;
        loop
        {
            if self.get(at, owner)?.state.tracking.is_some()
            {
                return Err(SERVICE_BUSY);
            }
            at = self.next_in(payload.child, at);
            if at == 0
            {
                break;
            }
        }
        // Membership revisions may not wrap and make an old cursor valid again.
        for parent in [old, new]
        {
            if parent != 0
            {
                self.get(parent, owner)?.tree.revision.checked_add(1).ok_or(NO_MEMORY)?;
            }
        }
        self.detach(payload.child);
        if new != 0
        {
            if let Some(o) = self.objects.get_mut(payload.child)
            {
                o.tree.parent = new;
                o.tree.cell = payload.cell;
                o.tree.span = payload.span;
                o.tree.previous = previous;
                o.tree.next = next;
            }
            if let Some(o) = self.objects.get_mut(previous)
            {
                o.tree.next = payload.child;
            }
            if let Some(o) = self.objects.get_mut(next)
            {
                o.tree.previous = payload.child;
            }
            if let Some(o) = self.objects.get_mut(new)
            {
                if previous == 0
                {
                    o.tree.first = payload.child;
                }
                if next == 0
                {
                    o.tree.last = payload.child;
                }
            }
        }
        for parent in [old, new]
        {
            if parent != 0 && !(parent == new && old == new)
            {
                if let Some(o) = self.objects.get_mut(parent)
                {
                    o.tree.revision += 1;
                }
                self.invalidate_ancestors(parent);
            }
        }
        if old == new && old != 0
        {
            if let Some(o) = self.objects.get_mut(old)
            {
                o.tree.revision += 1;
            }
            self.invalidate_ancestors(old);
        }
        if new == 0
        {
            self.invalidate_ancestors(payload.child);
        }
        Ok(true)
    }

    fn detach(&mut self, id: u64)
    {
        let Some(node) = self.objects.get(id).map(|o| o.tree)
        else
        {
            return;
        };
        if let Some(o) = self.objects.get_mut(node.previous)
        {
            o.tree.next = node.next;
        }
        if let Some(o) = self.objects.get_mut(node.next)
        {
            o.tree.previous = node.previous;
        }
        if let Some(o) = self.objects.get_mut(node.parent)
        {
            if o.tree.first == id
            {
                o.tree.first = node.next;
            }
            if o.tree.last == id
            {
                o.tree.last = node.previous;
            }
        }
        if let Some(o) = self.objects.get_mut(id)
        {
            o.tree.parent = 0;
            o.tree.previous = 0;
            o.tree.next = 0;
            o.tree.cell = Default::default();
            o.tree.span = Default::default();
        }
    }

    pub(super) fn invalidate_arrangement_ancestors(&mut self, mut id: u64)
    {
        let origin = id;
        while let Some(o) = self.objects.get_mut(id)
        {
            if let Some(layout) = &mut o.state.layout
            {
                layout.arrangement_pending = true;
            }
            id = o.tree.parent;
        }
        self.schedule_layout(origin);
    }

    pub(super) fn invalidate_ancestors(&mut self, mut id: u64)
    {
        let origin = id;
        while let Some(o) = self.objects.get_mut(id)
        {
            if let Some(layout) = &mut o.state.layout
            {
                layout.measured = None;
                layout.intrinsic = None;
                layout.arrangement_pending = true;
            }
            id = o.tree.parent;
        }
        self.schedule_layout(origin);
    }

    /// Depth-first row/column order, without recursion or an allocated stack.
    pub(super) fn next_in(&self, root: u64, mut id: u64) -> u64
    {
        let Some(o) = self.objects.get(id)
        else
        {
            return 0;
        };
        if o.tree.first != 0
        {
            return o.tree.first;
        }
        while id != root
        {
            let Some(o) = self.objects.get(id)
            else
            {
                return 0;
            };
            if o.tree.next != 0
            {
                return o.tree.next;
            }
            id = o.tree.parent;
        }
        0
    }

    pub(super) fn remove_tree(&mut self, root: u64) -> Result<(), Status>
    {
        let mut id = root;
        loop
        {
            let o = self.objects.get(id).ok_or(NOT_FOUND)?;
            if o.tree.first != 0
            {
                id = o.tree.first;
                continue;
            }
            let parent = o.tree.parent;
            self.detach(id);
            // Store::remove keeps an invalidate-first walk's payload/ticket if
            // that walk already owns disposal; never synthesize a second ticket.
            drop(self.objects.remove(id));
            if id == root
            {
                break;
            }
            id = parent;
        }
        Ok(())
    }

    pub(super) fn inherited_clip(&self, id: u64) -> Option<Rect>
    {
        let mut parent = self.objects.get(id)?.tree.parent;
        let mut visible = None;
        while let Some(o) = self.objects.get(parent)
        {
            visible = clip(visible, Some(o.state.spec.bounds()));
            visible = clip(visible, o.state.layout.as_ref().and_then(|l| l.visible));
            parent = o.tree.parent;
        }
        visible
    }

    pub(crate) fn visible_clip(&self, id: u64) -> Option<Rect>
    {
        let local = self.objects.get(id)?.state.layout.as_ref().and_then(|l| l.visible);
        clip(local, self.inherited_clip(id))
    }

    pub(crate) fn live(&self, id: u64) -> bool
    {
        self.objects.get(id).is_some()
    }

    pub(crate) fn in_tree(&self, id: u64) -> bool
    {
        self.objects.get(id).is_some_and(|o| o.tree.parent != 0 || o.tree.first != 0)
    }

    /// Retained command order is a cache derived from the same edges. Root
    /// creation order is stable; descendants use row/column preorder.
    pub(crate) fn paint_order(&self, a: u64, b: u64) -> core::cmp::Ordering
    {
        fn ancestry(manager: &Manager, mut id: u64) -> (u64, usize)
        {
            let mut depth = 0;
            while let Some(o) = manager.objects.get(id)
            {
                if o.tree.parent == 0
                {
                    return (id, depth);
                }
                id = o.tree.parent;
                depth += 1;
            }
            (0, depth)
        }
        let (aroot, mut ad) = ancestry(self, a);
        let (broot, mut bd) = ancestry(self, b);
        if aroot != broot
        {
            return self.objects
                       .get(aroot)
                       .map(|o| o.tree.order)
                       .cmp(&self.objects.get(broot).map(|o| o.tree.order));
        }
        let (mut a, mut b) = (a, b);
        while ad > bd
        {
            a = self.objects.get(a).map_or(0, |o| o.tree.parent);
            ad -= 1;
            if a == b
            {
                return core::cmp::Ordering::Greater;
            }
        }
        while bd > ad
        {
            b = self.objects.get(b).map_or(0, |o| o.tree.parent);
            bd -= 1;
            if a == b
            {
                return core::cmp::Ordering::Less;
            }
        }
        while a != b
        {
            let (Some(x), Some(y)) = (self.objects.get(a), self.objects.get(b))
            else
            {
                break;
            };
            if x.tree.parent == y.tree.parent
            {
                return x.tree.cell.cmp(&y.tree.cell);
            }
            a = x.tree.parent;
            b = y.tree.parent;
        }
        core::cmp::Ordering::Equal
    }

    pub(crate) fn hit_children(&self, root: u64, owner: u64, point: Point,
                               mut hit: impl FnMut(&ControlCall) -> Result<i16, Status>)
                               -> Result<(u64, i16), Status>
    {
        self.table(root, owner)?;
        let mut id = root;
        while let Some(o) = self.objects.get(id)
        {
            if o.tree.last == 0
            {
                break;
            }
            id = o.tree.last;
        }
        while id != root
        {
            let o = self.get(id, owner)?;
            if self.visible_clip(id).is_none_or(|clip| clip.contains(point))
            {
                let mut call = o.state.spec.draw();
                call.operation =
                    if o.state.spec.scrollbar().is_some() { HIT_SCROLLBAR } else { HIT_STANDARD };
                call.point = point;
                call.reflow = 1;
                let part = hit(&call)?;
                if part != 0 && part != 254
                {
                    return Ok((id, part));
                }
            }
            if o.tree.previous != 0
            {
                id = o.tree.previous;
                while let Some(o) = self.objects.get(id)
                {
                    if o.tree.last == 0
                    {
                        break;
                    }
                    id = o.tree.last;
                }
            }
            else
            {
                id = o.tree.parent;
            }
        }
        Ok((0, 0))
    }
}

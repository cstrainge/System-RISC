//! A Table owns a sparse grid of actual controls. Cells are stable row/column
//! addresses, not data-list rows. Membership does not assign new geometry;
//! automatic grid sizing and splitter controls are separate facilities.
use mac_abi::controls::{self as wire, reflow};

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell
{
    pub row:    u32,
    pub column: u32
}
impl From<Cell> for reflow::Cell
{
    fn from(c: Cell) -> Self
    {
        Self { row:    c.row,
               column: c.column }
    }
}
impl From<reflow::Cell> for Cell
{
    fn from(c: reflow::Cell) -> Self
    {
        Self { row:    c.row,
               column: c.column }
    }
}

pub struct Table<'a>
{
    control: Control<'a>
}
#[derive(Clone, Copy)]
pub struct TableRef<'a>
{
    control: ControlRef<'a>
}
pub struct AdoptionError<'a>
{
    pub cause: Error,
    pub child: Control<'a>
}
impl core::fmt::Display for AdoptionError<'_>
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        write!(f, "table child adoption failed: {}; ownership retained", self.cause)
    }
}
impl core::fmt::Debug for AdoptionError<'_>
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        f.debug_struct("AdoptionError")
         .field("cause", &self.cause)
         .field("child", &self.child.id())
         .finish()
    }
}
pub struct Child<'a>
{
    pub cell:    Cell,
    pub control: ControlRef<'a>
}
pub struct ChildHit<'a>
{
    pub control: ControlRef<'a>,
    pub part:    Part
}
pub struct Children<'a>
{
    table:    TableRef<'a>,
    relation: reflow::Relation,
    finished: bool
}
impl<'a> Controls<'a>
{
    pub fn create_table(&self, window: WindowId, bounds: Rect, reflow: Reflow)
                        -> Result<Table<'a>>
    {
        Ok(Table { control: self.create_standard(window,
                                                 StandardKind::Table,
                                                 StandardState::new(bounds, ""),
                                                 reflow)? })
    }
}
impl<'a> Table<'a>
{
    pub fn table(&self) -> TableRef<'a>
    {
        TableRef { control: self.control.control() }
    }

    pub fn control(&self) -> ControlRef<'a>
    {
        self.control.control()
    }

    /// Transfer this entire table subtree into another container.
    pub fn into_control(self) -> Control<'a>
    {
        self.control
    }
}
impl<'a> TableRef<'a>
{
    pub fn control(self) -> ControlRef<'a>
    {
        self.control
    }

    /// Success consumes the owner. Failure returns it intact. Occupied cells
    /// refuse with AlreadyExists; capture must be cancelled before transfer.
    pub fn set_child(self, cell: Cell, child: Control<'a>)
                     -> core::result::Result<ControlRef<'a>, AdoptionError<'a>>
    {
        let control = child.control();
        match self.control.layout(reflow::Payload { message: reflow::SET_CHILD_INDEXED,
                                                    child: child.id().0,
                                                    cell: cell.into(),
                                                    ..Default::default() })
        {
            Ok(_) =>
            {
                core::mem::forget(child);
                Ok(control)
            }
            Err(cause) =>
            {
                Err(AdoptionError { cause,
                                    child })
            }
        }
    }

    pub fn remove_child(self, child: ControlRef<'a>) -> Result<Control<'a>>
    {
        self.control.layout(reflow::Payload { message: reflow::REMOVE_CHILD,
                                               child: child.id().0,
                                               ..Default::default() })?;
        Ok(Control::from_ref(Controls::new(self.control.client()).control(child.id())))
    }

    /// Transfer lifetime directly; a refusal preserves the old parent/cell.
    pub fn reparent(self, child: ControlRef<'a>, destination: TableRef<'a>, cell: Cell)
                    -> Result<()>
    {
        self.control
            .layout(reflow::Payload { message: reflow::REPARENT,
                                      child: child.id().0,
                                      parent: destination.control.id().0,
                                      cell: cell.into(),
                                      ..Default::default() })
            .map(|_| ())
    }

    /// Row-major, depth-one enumeration. Changes to membership invalidate this
    /// cursor with ServiceBusy; stale parents/children return NotFound.
    pub fn children(self) -> Children<'a>
    {
        Children { table:    self,
                   relation: Default::default(),
                   finished: false }
    }

    /// Query the frontmost interactive descendant using the same tree order and
    /// ancestor clipping as retained drawing. Coordinates are window-local.
    pub fn hit_test(self, point: Point) -> Result<Option<ChildHit<'a>>>
    {
        let mut call = wire::ControlCall { id: self.control.id().0,
                                           operation: wire::HIT_CHILDREN_OBJECT,
                                           point,
                                           ..Default::default() };
        self.control.client().control(&mut call)?;
        if call.layout.child == 0
        {
            return Ok(None);
        }
        Ok(Some(ChildHit { control:
                               Controls::new(self.control.client()).control(ControlId(call.layout
                                                                                          .child)),
                           part:    Part::from_code(call.result as i16)? }))
    }
}
impl<'a> Iterator for Children<'a>
{
    type Item = Result<Child<'a>>;

    fn next(&mut self) -> Option<Self::Item>
    {
        if self.finished
        {
            return None;
        }
        let mut call = wire::ControlCall { id: self.table.control.id().0,
                                           operation: wire::NEXT_CHILD_OBJECT,
                                           relation: self.relation,
                                           ..Default::default() };
        if let Err(error) = self.table.control.client().control(&mut call)
        {
            self.finished = true;
            return Some(Err(error));
        }
        self.relation = call.relation;
        if call.relation.child == 0
        {
            self.finished = true;
            return None;
        }
        Some(Ok(Child { cell: call.relation.cell.into(),
            control: Controls::new(self.table.control.client()).control(ControlId(call.relation.child)) }))
    }
}

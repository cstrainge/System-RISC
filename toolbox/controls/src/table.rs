//! A Table owns a sparse grid of actual controls. Cells are stable row/column
//! addresses, not data-list rows. Configuration is deferred until assignment.
//! Grid layout supports explicit non-overlapping spans; Splitter is separate.
use mac_abi::controls::{self as wire, reflow};

use super::*;

/// Independent Table decoration switches. These do not affect layout or hit
/// testing. Defaults retain all lines; NONE hides all line decoration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridLines
{
    pub rows:    bool,
    pub columns: bool,
    pub border:  bool
}
impl GridLines
{
    pub const ALL: Self = Self { rows:    true,
                                 columns: true,
                                 border:  true };
    pub const NONE: Self = Self { rows:    false,
                                  columns: false,
                                  border:  false };
}
impl Default for GridLines
{
    fn default() -> Self
    {
        Self::ALL
    }
}
impl From<GridLines> for i16
{
    fn from(v: GridLines) -> Self
    {
        i16::from(!v.rows) * wire::TABLE_HIDE_ROWS
        | i16::from(!v.columns) * wire::TABLE_HIDE_COLUMNS
        | i16::from(!v.border) * wire::TABLE_HIDE_BORDER
    }
}
impl TryFrom<i16> for GridLines
{
    type Error = Error;

    fn try_from(bits: i16) -> Result<Self>
    {
        if bits & !wire::TABLE_LINE_FLAGS != 0
        {
            return Err(Error::BadArgument);
        }
        Ok(Self { rows:    bits & wire::TABLE_HIDE_ROWS == 0,
                  columns: bits & wire::TABLE_HIDE_COLUMNS == 0,
                  border:  bits & wire::TABLE_HIDE_BORDER == 0 })
    }
}

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
        match self.cause
        {
            Error::AlreadyExists =>
            {
                f.write_str("table child region overlaps an existing child; ownership retained")
            }
            Error::ServiceBusy =>
            {
                f.write_str("table child adoption requires capture cancellation or paint \
                             completion; ownership retained")
            }
            Error::BadArgument =>
            {
                f.write_str("invalid table cell, span or ownership relationship; ownership \
                             retained")
            }
            _ => write!(f, "table child adoption failed: {}; ownership retained", self.cause)
        }
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
    pub span:    Span,
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

    /// Atomically change row/column separators and the outer border. Redraws
    /// only this Table, without changing geometry, layout caches or capture.
    /// An admission failure preserves the previous decoration settings.
    pub fn set_grid_lines(self, lines: GridLines) -> Result<()>
    {
        let mut call = wire::ControlCall { id: self.control.id().0,
                                           operation: wire::SET_TABLE_LINES_OBJECT,
                                           ..Default::default() };
        call.standard.flags = lines.into();
        self.control.client().control(&mut call)
    }

    pub fn grid_lines(self) -> Result<GridLines>
    {
        let mut call = wire::ControlCall { id: self.control.id().0,
                                           operation: wire::INFO_OBJECT,
                                           ..Default::default() };
        self.control.client().control(&mut call)?;
        if call.procedure != wire::TABLE_PROCEDURE
        {
            return Err(Error::NotSupported);
        }
        call.standard.flags.try_into().map_err(|_| Error::BadAbi)
    }

    /// Success consumes the owner. Failure returns it intact. Occupied cells
    /// refuse with AlreadyExists; capture must be cancelled before transfer.
    pub fn set_child(self, cell: Cell, child: Control<'a>)
                     -> core::result::Result<ControlRef<'a>, AdoptionError<'a>>
    {
        self.set_child_spanning(cell, Span::ONE, child)
    }

    /// Adopt one control covering a rectangular region. Every covered cell must
    /// be free. A refused span leaves the owner and previous layout intact.
    pub fn set_child_spanning(self, cell: Cell, span: Span, child: Control<'a>)
                              -> core::result::Result<ControlRef<'a>, AdoptionError<'a>>
    {
        let control = child.control();
        match self.control.layout(reflow::Payload { message: reflow::SET_CHILD_INDEXED,
                                                    child: child.id().0,
                                                    cell: cell.into(),
                                                    span: span.into(),
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
        self.reparent_spanning(child, destination, cell, Span::ONE)
    }

    /// Also updates a span in place when destination is this Table. Overlap
    /// refusal preserves the previous region and every child's ownership.
    pub fn reparent_spanning(self, child: ControlRef<'a>, destination: TableRef<'a>, cell: Cell,
                             span: Span)
                             -> Result<()>
    {
        self.control
            .layout(reflow::Payload { message: reflow::REPARENT,
                                      child: child.id().0,
                                      parent: destination.control.id().0,
                                      cell: cell.into(),
                                      span: span.into(),
                                      ..Default::default() })
            .map(|_| ())
    }

    fn set_track(self, axis: u32, index: u32, size: TrackSize) -> Result<()>
    {
        let (mode, percent) = size.encode().map_err(|_| Error::BadArgument)?;
        self.control
            .layout(reflow::Payload { message: reflow::SET_GRID_TRACK,
                                      track: reflow::TrackSetting { axis,
                                                                    index,
                                                                    mode,
                                                                    percent },
                                      ..Default::default() })
            .map(|_| ())
    }

    fn track(self, axis: u32, index: u32) -> Result<TrackSize>
    {
        let t = self.control
                    .layout(reflow::Payload { message: reflow::GET_GRID_TRACK,
                                              track: reflow::TrackSetting { axis,
                                                                            index,
                                                                            ..Default::default() },
                                              ..Default::default() })?
                    .track;
        TrackSize::decode(t.mode, t.percent).map_err(|_| Error::BadAbi)
    }

    pub fn set_column(self, index: u32, size: TrackSize) -> Result<()>
    {
        self.set_track(0, index, size)
    }

    pub fn set_row(self, index: u32, size: TrackSize) -> Result<()>
    {
        self.set_track(1, index, size)
    }

    pub fn column(self, index: u32) -> Result<TrackSize>
    {
        self.track(0, index)
    }

    pub fn row(self, index: u32) -> Result<TrackSize>
    {
        self.track(1, index)
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
            span: match call.relation.span.try_into() { Ok(span) => span, Err(_) => { self.finished = true; return Some(Err(Error::BadAbi)); } },
            control: Controls::new(self.table.control.client()).control(ControlId(call.relation.child)) }))
    }
}

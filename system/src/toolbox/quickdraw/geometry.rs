#![forbid(unsafe_code)]
pub use mac_abi::geometry::{Point, Rect};

/// Source values are copied before writing the destination, allowing guest aliasing.
pub fn sect_rect(a: Rect, b: Rect, destination: &mut Rect) -> bool
{
    let result = Rect::new(a.left.max(b.left),
                           a.top.max(b.top),
                           a.right.min(b.right),
                           a.bottom.min(b.bottom));
    let intersects = !result.is_empty();
    *destination = if intersects { result } else { Rect::ZERO };
    intersects
}

pub fn union_rect(a: Rect, b: Rect) -> Rect
{
    Rect::new(a.left.min(b.left), a.top.min(b.top), a.right.max(b.right), a.bottom.max(b.bottom))
}

/// Small boot-time checks, also run in the RISC-V guest.
pub fn self_check() -> bool
{
    let a = Rect::new(0, 0, 20, 20);
    let mut out = Rect::ZERO;
    if !sect_rect(a, Rect::new(10, 5, 30, 15), &mut out) || out != Rect::new(10, 5, 20, 15)
    {
        return false;
    }
    if sect_rect(a, Rect::new(20, 0, 30, 20), &mut out) || out != Rect::ZERO
    {
        return false;
    }
    if !a.contains(Point { h: 0,
                           v: 0 })
       || a.contains(Point { h: 20,
                             v: 0 })
    {
        return false;
    }
    let mut moved = a;
    moved.offset(-10, 3);
    moved.inset(2, 2);
    if moved != Rect::new(-8, 5, 8, 21)
    {
        return false;
    }
    let mut empty = a;
    empty.inset(10, 1);
    empty == Rect::ZERO && Rect::from_be_bytes(moved.to_be_bytes()) == moved
}

/// Native rectangle service. No pointers or trap mechanics belong in this logic.
pub fn dispatch(call: &mut mac_abi::traps::GeometryCall) -> mac_abi::Status
{
    match call.operation
    {
        0 =>
        {
            call.result = u32::from(sect_rect(call.a, call.b, &mut call.output));
        }
        1 =>
        {
            call.output = union_rect(call.a, call.b);
            call.result = 1;
        }
        _ => return mac_abi::NOT_SUPPORTED
    }
    mac_abi::OK
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeometryError
{
    EmptySource,
    ZeroDenominator,
    CoordinateOverflow
}
pub fn scale_point(point: Point, numerator: Point, denominator: Point)
                   -> Result<Point, GeometryError>
{
    if denominator.h == 0 || denominator.v == 0
    {
        return Err(GeometryError::ZeroDenominator);
    }
    Ok(Point { h: i16::try_from(i64::from(point.h) * i64::from(numerator.h)
                                / i64::from(denominator.h)).map_err(|_| {
                                                               GeometryError::CoordinateOverflow
                                                           })?,
               v: i16::try_from(i64::from(point.v) * i64::from(numerator.v)
                                / i64::from(denominator.v)).map_err(|_| {
                                                               GeometryError::CoordinateOverflow
                                                           })? })
}
pub fn map_point(point: Point, source: Rect, destination: Rect) -> Result<Point, GeometryError>
{
    if source.is_empty()
    {
        return Err(GeometryError::EmptySource);
    }
    let map = |p: i16, a: i16, b: i16, c: i16, d: i16| -> Result<i16, GeometryError> {
        i16::try_from(i64::from(c)
                      + (i64::from(p) - i64::from(a)) * (i64::from(d) - i64::from(c))
                        / (i64::from(b) - i64::from(a))).map_err(|_| {
                                                            GeometryError::CoordinateOverflow
                                                        })
    };
    Ok(Point { h: map(point.h, source.left, source.right, destination.left, destination.right)?,
               v: map(point.v, source.top, source.bottom, destination.top, destination.bottom)? })
}
pub fn map_rect(rect: Rect, source: Rect, destination: Rect) -> Result<Rect, GeometryError>
{
    let first = map_point(Point { h: rect.left,
                                  v: rect.top },
                          source,
                          destination)?;
    let last = map_point(Point { h: rect.right,
                                 v: rect.bottom },
                         source,
                         destination)?;
    Ok(Rect::new(first.h, first.v, last.h, last.v))
}
pub fn map_polygon(points: &[Point], source: Rect, destination: Rect)
                   -> Result<alloc::vec::Vec<Point>, GeometryError>
{
    points.iter().map(|p| map_point(*p, source, destination)).collect()
}


pub fn outline_edges(rect: Rect) -> [Rect; 4]
{
    if rect.is_empty()
    {
        return [Rect::ZERO; 4];
    }
    [Rect::new(rect.left, rect.top, rect.right, rect.top + 1),
     Rect::new(rect.left, rect.bottom - 1, rect.right, rect.bottom),
     Rect::new(rect.left, rect.top + 1, rect.left + 1, rect.bottom - 1),
     Rect::new(rect.right - 1, rect.top + 1, rect.right, rect.bottom - 1)]
}

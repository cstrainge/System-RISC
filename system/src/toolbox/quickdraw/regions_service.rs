//! Region service operates on checked slices and returns classic big-endian data.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_abi::{quickdraw::*, *};

use super::{port::{Shape, shape_region},
            region::{BooleanOp, Region, RegionError}};
pub(crate) fn status(error: RegionError) -> Status
{
    match error
    {
        RegionError::Allocation => NO_MEMORY,
        RegionError::Truncated
        | RegionError::InvalidSize
        | RegionError::InvalidBounds
        | RegionError::InvalidData
        | RegionError::CoordinateOverflow
        | RegionError::TooLarge => BAD_ARGUMENT
    }
}
pub(crate) fn graphics_status(error: super::port::GraphicsError) -> Status
{
    match error
    {
        super::port::GraphicsError::Allocation => NO_MEMORY,
        super::port::GraphicsError::InvalidSize
        | super::port::GraphicsError::TooLarge
        | super::port::GraphicsError::CoordinateOverflow => BAD_ARGUMENT
    }
}
pub enum Encoded
{
    Compact([u8; 10]),
    Complex(Vec<u8>)
}
impl core::ops::Deref for Encoded
{
    type Target = [u8];

    fn deref(&self) -> &[u8]
    {
        match self
        {
            Self::Compact(bytes) => bytes,
            Self::Complex(bytes) => bytes
        }
    }
}
pub fn compute(call: &mut RegionCall, a: &[u8], b: &[u8], points: &[Point])
               -> Result<Option<Encoded>, Status>
{
    let decode = |bytes| Region::from_be_bytes(bytes).map_err(status);
    let region = match call.operation
    {
        REGION_RECT => Region::from_rect(call.bounds),
        REGION_OVAL => shape_region(Shape::Oval(call.bounds)).map_err(graphics_status)?,
        REGION_ROUND_RECT =>
        {
            shape_region(Shape::RoundRect { bounds:      call.bounds,
                                            oval_width:  call.point.h,
                                            oval_height: call.point.v }).map_err(graphics_status)?
        }
        REGION_POLYGON => shape_region(Shape::Polygon(points)).map_err(graphics_status)?,
        REGION_INTERSECT | REGION_UNION | REGION_DIFFERENCE | REGION_XOR =>
        {
            let op = match call.operation
            {
                REGION_INTERSECT => BooleanOp::Intersection,
                REGION_UNION => BooleanOp::Union,
                REGION_DIFFERENCE => BooleanOp::Difference,
                _ => BooleanOp::Xor
            };
            decode(a)?.combine(&decode(b)?, op).map_err(status)?
        }
        REGION_OFFSET => decode(a)?.offset(call.point.h, call.point.v).map_err(status)?,
        REGION_INSET => decode(a)?.inset(call.point.h, call.point.v).map_err(status)?,
        REGION_CONTAINS =>
        {
            call.result = u32::from(decode(a)?.contains(call.point));
            return Ok(None);
        }
        REGION_INTERSECTS =>
        {
            call.result = u32::from(decode(a)?.overlaps(&Region::from_rect(call.bounds)));
            return Ok(None);
        }
        _ => return Err(NOT_SUPPORTED)
    };
    let bytes = match region.compact_bytes()
    {
        Some(bytes) => Encoded::Compact(bytes),
        None => Encoded::Complex(region.to_be_bytes().map_err(status)?)
    };
    call.required = bytes.len();
    Ok(Some(bytes))
}

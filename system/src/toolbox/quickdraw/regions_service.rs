//! Region service operates on checked slices and returns classic big-endian data.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_abi::{quickdraw::*, *};

use super::{port::{Shape, shape_region},
            region::{BooleanOp, Region}};
pub fn compute(call: &mut RegionCall, a: &[u8], b: &[u8], points: &[Point])
               -> Result<Option<Vec<u8>>, Status>
{
    let decode = |bytes| Region::from_be_bytes(bytes).map_err(|_| BAD_ARGUMENT);
    let region = match call.operation
    {
        REGION_RECT => Region::from_rect(call.bounds),
        REGION_OVAL => shape_region(Shape::Oval(call.bounds)).map_err(|_| BAD_ARGUMENT)?,
        REGION_ROUND_RECT =>
        {
            shape_region(Shape::RoundRect { bounds:      call.bounds,
                                            oval_width:  call.point.h,
                                            oval_height: call.point.v }).map_err(|_| BAD_ARGUMENT)?
        }
        REGION_POLYGON => shape_region(Shape::Polygon(points)).map_err(|_| BAD_ARGUMENT)?,
        REGION_INTERSECT | REGION_UNION | REGION_DIFFERENCE | REGION_XOR =>
        {
            let op = match call.operation
            {
                REGION_INTERSECT => BooleanOp::Intersection,
                REGION_UNION => BooleanOp::Union,
                REGION_DIFFERENCE => BooleanOp::Difference,
                _ => BooleanOp::Xor
            };
            decode(a)?.combine(&decode(b)?, op)
        }
        REGION_OFFSET => decode(a)?.offset(call.point.h, call.point.v).map_err(|_| BAD_ARGUMENT)?,
        REGION_INSET => decode(a)?.inset(call.point.h, call.point.v).map_err(|_| BAD_ARGUMENT)?,
        REGION_CONTAINS =>
        {
            call.result = u32::from(decode(a)?.contains(call.point));
            return Ok(None);
        }
        REGION_INTERSECTS =>
        {
            call.result = u32::from(!decode(a)?.combine(&Region::from_rect(call.bounds),
                                                        BooleanOp::Intersection)
                                               .is_empty());
            return Ok(None);
        }
        _ => return Err(NOT_SUPPORTED)
    };
    let bytes = region.to_be_bytes().map_err(|_| BAD_ARGUMENT)?;
    call.required = bytes.len();
    Ok(Some(bytes))
}

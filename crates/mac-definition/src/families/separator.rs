//! Passive Separator state and the single wire-admission codec.
use mac_abi::{controls, definitions::ControlDefinitionState};

use super::{Family, title};
use crate::{Error, Rect, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeparatorOrientation
{
    Horizontal,
    Vertical
}
/// The initial Separator definition supports unlabelled theme rules only.
#[derive(Clone, Copy, Debug)]
pub struct Separator
{
    pub bounds:      Rect,
    pub orientation: SeparatorOrientation
}
pub(crate) struct SeparatorFamily;
impl Family for SeparatorFamily
{
    type View<'a> = Separator;
    type Wire = ControlDefinitionState;

    const ID: i16 = controls::CDEF_SEPARATOR;

    fn variation(value: i16) -> Result<()>
    {
        if value == 0 { Ok(()) } else { Err(Error::NotSupported) }
    }

    fn decode<'a>(wire: &'a Self::Wire, _: i16) -> Result<Self::View<'a>>
    {
        if wire.flags & !controls::SEPARATOR_VERTICAL != 0
           || wire.highlighted != 0
           || wire.value != 0
           || wire.minimum != 0
           || !matches!(wire.maximum, 0 | 1)
           || wire.bounds.right < wire.bounds.left
           || wire.bounds.bottom < wire.bounds.top
        {
            return Err(Error::BadArgument);
        }
        // Validate the UTF-8/length first. Labels (including rotated labels)
        // are unimplemented, not silently discarded after publication.
        if !title(wire)?.is_empty()
        {
            return Err(Error::NotSupported);
        }
        Ok(Separator { bounds:      wire.bounds,
                       orientation: if wire.flags & controls::SEPARATOR_VERTICAL == 0
                       {
                           SeparatorOrientation::Horizontal
                       }
                       else
                       {
                           SeparatorOrientation::Vertical
                       } })
    }

    fn bounds(state: &Self::View<'_>) -> Rect
    {
        state.bounds
    }
}

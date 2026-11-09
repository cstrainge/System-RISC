//! Typed future control notifications in the ordinary Event model. Stubs emit
//! none. Variable text transactions require owned queue admission before a real
//! editor mutates text; the fixed legacy queue must never truncate/coalesce them.
use mac_abi::{WindowId, controls::suite::messages::TextDelta};
use toolbox_controls::ControlId;

use crate::Input;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlChange<'a>
{
    TextChanged
    {
        transaction: u64,
        deltas:      &'a [TextDelta<'a>]
    },
    SelectionChanged
    {
        anchor: u64,
        caret:  u64
    },
    ValueChanged,
    ChoiceChanged
    {
        key: u64
    },
    Activated
    {
        key: u64
    },
    DisclosureChanged
    {
        open: bool
    },
    Navigate
    {
        key: u64
    },
    ColorChanged
    {
        rgba: u32
    },
    ReadoutChanged
    {
        series: u64,
        index:  u64
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlNotification<'a>
{
    pub control: ControlId,
    pub window:  WindowId,
    pub change:  ControlChange<'a>,
    pub input:   Input
}

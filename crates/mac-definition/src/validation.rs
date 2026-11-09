//! Pure wire admission shared with the resident cold-load boundary. These use
//! the entry shim's same family codecs; no author callback, port or allocation.
#![forbid(unsafe_code)]

use mac_abi::{controls, definitions::ControlDefinitionState};

use crate::{Error, Result,
            families::{ButtonFamily, Family, ProgressFamily, ScrollbarFamily, SplitterFamily,
                       StaticFamily, TableFamily}};

pub fn standard(id: i16, variation: i16, state: &ControlDefinitionState) -> Result<()>
{
    match id
    {
        0 =>
        {
            ButtonFamily::variation(variation)?;
            ButtonFamily::decode(state, variation).map(|_| ())
        }
        controls::CDEF_STATIC_TEXT =>
        {
            StaticFamily::variation(variation)?;
            StaticFamily::decode(state, variation).map(|_| ())
        }
        controls::CDEF_PROGRESS_BAR =>
        {
            ProgressFamily::variation(variation)?;
            ProgressFamily::decode(state, variation).map(|_| ())
        }
        controls::CDEF_TABLE =>
        {
            TableFamily::variation(variation)?;
            TableFamily::decode(state, variation).map(|_| ())
        }
        controls::CDEF_SPLITTER =>
        {
            SplitterFamily::variation(variation)?;
            SplitterFamily::decode(state, variation).map(|_| ())
        }
        29 =>
        {
            crate::families::SeparatorFamily::variation(variation)?;
            crate::families::SeparatorFamily::decode(state, variation).map(|_| ())
        }
        30 =>
        {
            crate::families::GroupBoxFamily::variation(variation)?;
            crate::families::GroupBoxFamily::decode(state, variation).map(|_| ())
        }
        31 =>
        {
            crate::families::PictureFamily::variation(variation)?;
            crate::families::PictureFamily::decode(state, variation).map(|_| ())
        }
        32 =>
        {
            crate::families::ActivityIndicatorFamily::variation(variation)?;
            crate::families::ActivityIndicatorFamily::decode(state, variation).map(|_| ())
        }
        25 =>
        {
            crate::families::DropMenuFamily::variation(variation)?;
            crate::families::DropMenuFamily::decode(state, variation).map(|_| ())
        }
        33 =>
        {
            crate::families::ListBoxFamily::variation(variation)?;
            crate::families::ListBoxFamily::decode(state, variation).map(|_| ())
        }
        34 =>
        {
            crate::families::SearchFieldFamily::variation(variation)?;
            crate::families::SearchFieldFamily::decode(state, variation).map(|_| ())
        }
        35 =>
        {
            crate::families::OverflowArrowsFamily::variation(variation)?;
            crate::families::OverflowArrowsFamily::decode(state, variation).map(|_| ())
        }
        36 =>
        {
            crate::families::SliderFamily::variation(variation)?;
            crate::families::SliderFamily::decode(state, variation).map(|_| ())
        }
        37 =>
        {
            crate::families::StepperFamily::variation(variation)?;
            crate::families::StepperFamily::decode(state, variation).map(|_| ())
        }
        38 =>
        {
            crate::families::DisclosureTriangleFamily::variation(variation)?;
            crate::families::DisclosureTriangleFamily::decode(state, variation).map(|_| ())
        }
        39 =>
        {
            crate::families::MeterFamily::variation(variation)?;
            crate::families::MeterFamily::decode(state, variation).map(|_| ())
        }
        40 =>
        {
            crate::families::SegmentedFamily::variation(variation)?;
            crate::families::SegmentedFamily::decode(state, variation).map(|_| ())
        }
        41 =>
        {
            crate::families::DateTimeFamily::variation(variation)?;
            crate::families::DateTimeFamily::decode(state, variation).map(|_| ())
        }
        42 =>
        {
            crate::families::ListEditFamily::variation(variation)?;
            crate::families::ListEditFamily::decode(state, variation).map(|_| ())
        }
        43 =>
        {
            crate::families::TabFamily::variation(variation)?;
            crate::families::TabFamily::decode(state, variation).map(|_| ())
        }
        44 =>
        {
            crate::families::BreadcrumbFamily::variation(variation)?;
            crate::families::BreadcrumbFamily::decode(state, variation).map(|_| ())
        }
        45 =>
        {
            crate::families::ColorWellFamily::variation(variation)?;
            crate::families::ColorWellFamily::decode(state, variation).map(|_| ())
        }
        46 =>
        {
            crate::families::TreeFamily::variation(variation)?;
            crate::families::TreeFamily::decode(state, variation).map(|_| ())
        }
        47 =>
        {
            crate::families::IconViewFamily::variation(variation)?;
            crate::families::IconViewFamily::decode(state, variation).map(|_| ())
        }
        48 =>
        {
            crate::families::GraphFamily::variation(variation)?;
            crate::families::GraphFamily::decode(state, variation).map(|_| ())
        }
        49 =>
        {
            crate::families::TooltipFamily::variation(variation)?;
            crate::families::TooltipFamily::decode(state, variation).map(|_| ())
        }
        17 =>
        {
            crate::families::TextEditFamily::variation(variation)?;
            crate::families::TextEditFamily::decode(state, variation).map(|_| ())
        }
        _ => Err(Error::NotSupported)
    }
}

pub fn scrollbar(state: &controls::ScrollbarState) -> Result<()>
{
    ScrollbarFamily::decode(state, 0).map(|_| ())
}

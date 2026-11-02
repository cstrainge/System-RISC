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
        _ => Err(Error::NotSupported)
    }
}

pub fn scrollbar(state: &controls::ScrollbarState) -> Result<()>
{
    ScrollbarFamily::decode(state, 0).map(|_| ())
}

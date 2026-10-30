//! Immutable title backing admitted before managed-control publication. Painting
//! snapshots only small appearance fields and shares the already owned bytes.
#![forbid(unsafe_code)]
use alloc::sync::Arc;

use mac_abi::{BAD_ABI, BAD_ARGUMENT, NO_MEMORY, Rect, Status, controls::ProgressRange,
              definitions::ControlDefinitionState, quickdraw::FontSelection};

pub(super) struct Title
{
    bytes:  [u8; mac_abi::INLINE_STRING_BYTES],
    length: u32
}
impl Title
{
    pub(super) fn matches(&self, state: &ControlDefinitionState) -> bool
    {
        self.length == state.title_length && self.bytes == state.title
    }

    pub(super) fn admit(state: &ControlDefinitionState, previous: Option<&Arc<Self>>)
                        -> Result<Arc<Self>, Status>
    {
        let length = state.title_length as usize;
        if length > state.title.len() || core::str::from_utf8(&state.title[..length]).is_err()
        {
            return Err(BAD_ARGUMENT);
        }
        if let Some(previous) = previous.filter(|title| title.matches(state))
        {
            return Ok(previous.clone());
        }
        Arc::try_new(Self { bytes:  state.title,
                            length: state.title_length }).map_err(|_| NO_MEMORY)
    }
}
pub(super) struct Standard
{
    title:             Arc<Title>,
    pub(super) bounds: Rect,
    enabled:           u32,
    highlighted:       u32,
    value:             i16,
    minimum:           i16,
    maximum:           i16,
    flags:             i16,
    font:              FontSelection,
    progress:          ProgressRange
}
impl Standard
{
    pub(super) fn new(state: &ControlDefinitionState, title: Option<&Arc<Title>>)
                      -> Result<Self, Status>
    {
        let title = match title
        {
            Some(title) if title.matches(state) => title.clone(),
            Some(_) => return Err(BAD_ABI), // Definition validation changed the admitted title.
            None => Title::admit(state, None)? // Stateless legacy/raw drawing, before publication.
        };
        Ok(Self { title,
                  bounds: state.bounds,
                  enabled: state.enabled,
                  highlighted: state.highlighted,
                  value: state.value,
                  minimum: state.minimum,
                  maximum: state.maximum,
                  flags: state.flags,
                  font: state.font,
                  progress: state.progress })
    }

    pub(super) fn clear_highlight(&mut self) -> bool
    {
        core::mem::replace(&mut self.highlighted, 0) != 0
    }

    pub(super) fn wire(&self) -> ControlDefinitionState
    {
        ControlDefinitionState { title:        self.title.bytes,
                                 title_length: self.title.length,
                                 bounds:       self.bounds,
                                 enabled:      self.enabled,
                                 highlighted:  self.highlighted,
                                 value:        self.value,
                                 minimum:      self.minimum,
                                 maximum:      self.maximum,
                                 flags:        self.flags,
                                 font:         self.font,
                                 progress:     self.progress }
    }

    pub(super) fn storage(&self) -> usize
    {
        core::mem::size_of::<Title>()
    }
}

/// Immutable occupied-cell outlines, including complete merged regions. No
/// interior divider is drawn through a span. Empty cells need no stored object.
pub(super) struct Grid
{
    pub cells: alloc::vec::Vec<Rect>
}

//! Immutable title backing admitted before managed-control publication. Painting
//! snapshots only small appearance fields and shares the already owned bytes.
#![forbid(unsafe_code)]
use alloc::sync::Arc;

use mac_abi::{BAD_ABI, BAD_ARGUMENT, NO_MEMORY, Status, controls::ProgressRange,
              definitions::ControlDefinitionState, geometry::Rect, quickdraw::FontSelection};

use crate::toolbox::resources::definitions::BoundDefinition;

pub(super) struct Title
{
    bytes:                 [u8; mac_abi::INLINE_STRING_BYTES],
    length:                u32,
    pub(super) properties: Option<Arc<mac_control_state::State>>,
    pub(super) revision:   u64
}
impl Title
{
    pub(super) fn with_properties(previous: &Self, properties: Arc<mac_control_state::State>,
                                  revision: u64)
                                  -> Self
    {
        Self { bytes: previous.bytes,
               length: previous.length,
               properties: Some(properties),
               revision }
    }

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
        Arc::try_new(Self { bytes:      state.title,
                            length:     state.title_length,
                            properties: previous.and_then(|old| old.properties.clone()),
                            revision:   match previous
                            {
                                Some(old) => old.revision.checked_add(1).ok_or(NO_MEMORY)?,
                                None => 1
                            } }).map_err(|_| NO_MEMORY)
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
    progress:          ProgressRange,
    splitter_parent:   Rect
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
                  progress: state.progress,
                  splitter_parent: state.splitter_parent })
    }

    pub(super) fn clear_highlight(&mut self) -> bool
    {
        core::mem::replace(&mut self.highlighted, 0) != 0
    }

    pub(super) fn wire(&self) -> ControlDefinitionState
    {
        ControlDefinitionState { title:           self.title.bytes,
                                 title_length:    self.title.length,
                                 bounds:          self.bounds,
                                 enabled:         self.enabled,
                                 highlighted:     self.highlighted,
                                 value:           self.value,
                                 minimum:         self.minimum,
                                 maximum:         self.maximum,
                                 flags:           self.flags,
                                 font:            self.font,
                                 progress:        self.progress,
                                 splitter_parent: self.splitter_parent }
    }

    pub(super) fn properties(&self) -> Option<Arc<mac_control_state::State>>
    {
        self.title.properties.clone()
    }

    pub(super) fn storage(&self) -> usize
    {
        core::mem::size_of::<Title>()
        + self.title
              .properties
              .as_ref()
              .map_or(0, |p| core::mem::size_of::<mac_control_state::State>() + p.as_bytes().len())
    }
}

/// Immutable occupied-cell outlines, including complete merged regions. No
/// interior divider is drawn through a span. Empty cells need no stored object.
pub(super) struct Grid
{
    pub cells:    alloc::vec::Vec<Rect>,
    pub logical:  alloc::vec::Vec<mac_abi::controls::reflow::LogicalRect>,
    pub dividers: Option<Arc<Dividers>>
}

pub(super) struct Bars
{
    pub bars:       [Option<mac_abi::controls::ScrollbarState>; 2],
    pub definition: Arc<BoundDefinition>,
    pub viewport:   Rect
}

/// Immutable Splitter instances owned by one Table's boundary configuration.
/// Row/column indexes are geometry settings; the Table retains the CTRL identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Divider
{
    pub axis:     u32,
    pub index:    u32,
    pub bounds:   Rect,
    pub minimum:  i16,
    pub maximum:  i16,
    pub position: i16
}
impl Divider
{
    pub fn state(self, parent: Rect, enabled: bool) -> ControlDefinitionState
    {
        use mac_abi::controls::*;
        ControlDefinitionState { bounds: self.bounds,
                                 value: self.position,
                                 minimum: self.minimum,
                                 maximum: self.maximum,
                                 enabled: u32::from(enabled),
                                 flags: SPLITTER_PARENT
                                        | SPLITTER_RANGE
                                        | if self.axis == 1 { SPLITTER_HORIZONTAL } else { 0 },
                                 splitter_parent: parent,
                                 ..Default::default() }
    }
}
pub(super) struct Dividers
{
    pub definition: Arc<BoundDefinition>,
    pub viewport:   Rect,
    pub segments:   alloc::vec::Vec<Divider>,
    pub tracks:     [Arc<alloc::vec::Vec<mac_abi::controls::reflow::GridTrack>>; 2]
}
impl Dividers
{
    pub fn storage(&self) -> usize
    {
        core::mem::size_of::<Self>()
        + self.segments.len() * core::mem::size_of::<Divider>()
        + self.tracks
              .iter()
              .map(|t| {
                  core::mem::size_of_val(t.as_ref())
                  + t.len() * core::mem::size_of::<mac_abi::controls::reflow::GridTrack>()
              })
              .sum::<usize>()
    }
}

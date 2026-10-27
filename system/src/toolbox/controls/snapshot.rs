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

#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn appearance_snapshots_share_title_and_retain_old_pixels_without_mutation()
    {
        let mut state = ControlDefinitionState::new(Rect::new(2, 3, 90, 28), "Before");
        state.enabled = 0;
        state.highlighted = 1;
        state.value = 2;
        state.minimum = -3;
        state.maximum = 7;
        state.flags = 5;
        state.font = FontSelection { family: 3,
                                     size:   12,
                                     face:   1 };
        let title = Title::admit(&state, None).unwrap();
        let before = Standard::new(&state, Some(&title)).unwrap();
        assert!(before.wire() == state, "every appearance field must survive the binding snapshot");
        let mut changed = state;
        changed.bounds = Rect::new(10, 10, 100, 40);
        changed.enabled = 1;
        changed.highlighted = 0;
        changed.value = 4;
        changed.font.size = 9;
        let after = Standard::new(&changed, Some(&title)).unwrap();
        assert!(Arc::ptr_eq(&before.title, &after.title),
                "appearance-only changes allocate no title copy");
        assert!(before.wire() == state && after.wire() == changed,
                "retained drawings are immutable snapshots");
        let renamed = ControlDefinitionState::new(changed.bounds, "After");
        let next = Title::admit(&renamed, Some(&title)).unwrap();
        assert!(!Arc::ptr_eq(&title, &next));
        assert!(before.wire() == state,
                "renaming a control must not mutate a retained older drawing");
        assert!(core::mem::size_of::<super::super::Drawing>()
                < core::mem::size_of::<ControlDefinitionState>() / 2,
                "wire title bytes must not inflate every retained command");
    }
    #[test]
    fn malformed_or_mismatched_title_refuses_without_replacing_admitted_backing()
    {
        let state = ControlDefinitionState::new(Rect::new(0, 0, 80, 20), "Valid");
        let title = Title::admit(&state, None).unwrap();
        let mut invalid = state;
        invalid.title_length = u32::MAX;
        assert!(matches!(Title::admit(&invalid, Some(&title)), Err(BAD_ARGUMENT)));
        invalid = state;
        invalid.title[0] = 0xff;
        assert!(matches!(Title::admit(&invalid, Some(&title)), Err(BAD_ARGUMENT)));
        let different = ControlDefinitionState::new(state.bounds, "Different");
        assert!(matches!(Standard::new(&different, Some(&title)), Err(BAD_ABI)));
        assert!(Arc::ptr_eq(&title, &Title::admit(&state, Some(&title)).unwrap()));
    }
}

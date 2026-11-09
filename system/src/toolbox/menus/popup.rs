//! Standalone popups use the same canonical MENU objects and MDEF
//! measure/draw/hit/selection path as the menu bar. No second menu renderer.
use super::*;
impl Manager
{
    /// Validate and measure everything before publishing the popup or capture.
    /// The caller retains the original row-to-destination snapshot until close.
    pub(crate) fn open_popup(&mut self, owner: &OwnerRef, labels: &[&str], current: usize,
                             anchor: Rect, usable: Rect)
                             -> Result<u64, Status>
    {
        if labels.is_empty()
           || labels.len() > MAX_ITEMS
           || current >= labels.len()
           || anchor.is_empty()
           || usable.is_empty()
           || labels.iter().any(|text| text.len() > INLINE_STRING_BYTES)
        {
            return Err(BAD_ARGUMENT);
        }
        let mut state = new_state()?;
        state.minimum_width = anchor.right.checked_sub(anchor.left).ok_or(BAD_ARGUMENT)?;
        state.item_count = labels.len() as u32;
        for (index, text) in labels.iter().enumerate()
        {
            let row = &mut state.items[index];
            row.text[..text.len()].copy_from_slice(text.as_bytes());
            row.text_length = text.len() as u32;
            row.checked = u32::from(index == current);
        }
        Self::measure_with(self.entry, self.gate, &mut state)?;
        let bounds = state.popup_bounds;
        let width = i32::from(bounds.right) - i32::from(bounds.left);
        let height = i32::from(bounds.bottom) - i32::from(bounds.top);
        let available_width = i32::from(usable.right) - i32::from(usable.left);
        let available_height = i32::from(usable.bottom) - i32::from(usable.top);
        // Scrolling popup menus are not implemented. Refuse honestly instead
        // of painting or selecting rows beyond the actual usable screen.
        if width <= 0 || height <= 0 || width > available_width || height > available_height
        {
            return Err(NOT_SUPPORTED);
        }
        let row = state.items[current].bounds;
        let left =
            i32::from(anchor.left).clamp(i32::from(usable.left), i32::from(usable.right) - width);
        let top = (i32::from(anchor.top) - (i32::from(row.top) - i32::from(bounds.top)))
            .clamp(i32::from(usable.top), i32::from(usable.bottom) - height);
        let dx = left - i32::from(bounds.left);
        let dy = top - i32::from(bounds.top);
        let translate = |r: Rect| -> Result<Rect, Status> {
            let x = |v: i16| i16::try_from(i32::from(v) + dx).map_err(|_| BAD_ARGUMENT);
            let y = |v: i16| i16::try_from(i32::from(v) + dy).map_err(|_| BAD_ARGUMENT);
            Ok(Rect::new(x(r.left)?, y(r.top)?, x(r.right)?, y(r.bottom)?))
        };
        state.popup_bounds = translate(bounds)?;
        for row in &mut state.items[..labels.len()]
        {
            row.bounds = translate(row.bounds)?;
        }
        let id = self.objects
                     .insert_with(owner, |id| {
                         Menu { id,
                                state,
                                revision: 1,
                                popup: true }
                     })
                     .map_err(pool_status)?;
        self.track_popup(id, current as i16 + 1);
        Ok(id)
    }

    pub(crate) fn close_popup(&mut self, owner: &OwnerRef, id: u64) -> Result<(), Status>
    {
        let menu = self.objects.get(id).ok_or(NOT_FOUND)?;
        if self.objects.owner(id) != Some(owner.owner())
        {
            return Err(ACCESS_DENIED);
        }
        if !menu.popup
        {
            return Err(BAD_ARGUMENT);
        }
        self.cancel_popup(id);
        drop(self.objects.remove(id).ok_or(NOT_FOUND)?);
        Ok(())
    }
}

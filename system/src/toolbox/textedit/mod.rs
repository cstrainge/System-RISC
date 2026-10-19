//! Resident, per-application monochrome TextEdit state. Native UTF-8 byte offsets
//! are distinct from an original 68k TEHandle's MacRoman offsets.
#![deny(unsafe_code)]
use alloc::vec::Vec;

use mac_abi::{textedit::*, *};
mod layout;
mod model;
pub mod clipboard;
pub use model::Editor;
#[allow(unsafe_code)]
pub mod native;
struct Slot
{
    generation: u16,
    editor:     Option<Editor>
}
#[derive(Default)]
pub struct Manager
{
    slots: Vec<Slot>
}
pub struct PreparedEdit
{
    id:     EditorId,
    editor: Editor
}
impl Manager
{
    pub fn selected_text(&self, id: EditorId) -> Result<&str, Status>
    {
        Ok(self.inspect(id).ok_or(NOT_FOUND)?.selection())
    }

    pub fn prepare_delete(&self, id: EditorId) -> Result<PreparedEdit, Status>
    {
        Ok(PreparedEdit { id,
                          editor: self.inspect(id).ok_or(NOT_FOUND)?.prepared_replace("")? })
    }

    pub fn prepare_replace_selection(&self, id: EditorId, input: &str)
                                     -> Result<PreparedEdit, Status>
    {
        Ok(PreparedEdit { id,
                          editor: self.inspect(id).ok_or(NOT_FOUND)?.prepared_replace(input)? })
    }

    pub fn apply(&mut self, edit: PreparedEdit) -> Result<(), Status>
    {
        *self.inspect_mut(edit.id).ok_or(NOT_FOUND)? = edit.editor;
        Ok(())
    }

    pub fn inspect(&self, id: EditorId) -> Option<&Editor>
    {
        let slot = self.slots.get((id.0 & 0xffff).checked_sub(1)? as usize)?;
        if u32::from(slot.generation) == id.0 >> 16 { slot.editor.as_ref() } else { None }
    }

    pub fn inspect_mut(&mut self, id: EditorId) -> Option<&mut Editor>
    {
        let slot = self.slots.get_mut((id.0 & 0xffff).checked_sub(1)? as usize)?;
        if u32::from(slot.generation) == id.0 >> 16 { slot.editor.as_mut() } else { None }
    }

    pub fn request(&mut self, call: &mut TextEditCall, input: &str, output: &mut [u8]) -> Status
    {
        call.transferred = 0;
        self.perform(call, input, output).map_or_else(|status| status, |()| OK)
    }

    fn perform(&mut self, call: &mut TextEditCall, input: &str, output: &mut [u8])
               -> Result<(), Status>
    {
        if call.operation == CREATE
        {
            let editor = Editor::new(call.info.dest, call.info.view)?;
            let index = if let Some(index) =
                self.slots
                    .iter()
                    .position(|slot| slot.editor.is_none() && slot.generation != u16::MAX)
            {
                index
            }
            else
            {
                if self.slots.len() >= MAX_EDITORS
                {
                    return Err(NO_MEMORY);
                }
                self.slots.try_reserve(1).map_err(|_| NO_MEMORY)?;
                self.slots.push(Slot { generation: 0,
                                       editor:     None });
                self.slots.len() - 1
            };
            let slot = &mut self.slots[index];
            slot.generation += 1;
            call.id = EditorId((u32::from(slot.generation) << 16) | (index + 1) as u32);
            slot.editor = Some(editor);
        }
        if call.operation == DISPOSE
        {
            if self.inspect(call.id).is_none()
            {
                return Err(NOT_FOUND);
            }
            self.slots[(call.id.0 & 0xffff) as usize - 1].editor = None;
            return Ok(());
        }
        let editor = self.inspect_mut(call.id).ok_or(NOT_FOUND)?;
        match call.operation
        {
            CREATE | INFO => (),
            TEXT | SELECTION_TEXT =>
            {
                let text = if call.operation == TEXT { editor.text() } else { editor.selection() };
                if output.len() < text.len()
                {
                    return Err(NO_MEMORY);
                }
                output[..text.len()].copy_from_slice(text.as_bytes());
                call.transferred = text.len();
            }
            SET_TEXT => editor.set_text(input)?,
            SET_RECTS => editor.set_rects(call.info.dest, call.info.view)?,
            INSERT => editor.insert(input)?,
            DELETE => editor.delete()?,
            KEY => editor.key(char::from_u32(call.key).ok_or(BAD_ARGUMENT)?)?,
            SELECT => editor.select(call.info.selection_start, call.info.selection_end)?,
            LINE => call.line = *editor.lines().get(call.index as usize).ok_or(NOT_FOUND)?,
            OFFSET => call.index = editor.offset(call.point),
            POINT => call.point = editor.point(call.index)?,
            SCROLL => editor.scroll(call.delta)?,
            ACTIVATE => editor.activate(call.index != 0),
            IDLE => editor.idle(call.ticks),
            CLICK => editor.click(call.point, call.index != 0)?,
            _ => return Err(NOT_SUPPORTED)
        }
        call.info = editor.info();
        Ok(())
    }
}

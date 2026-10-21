//! Resident, per-application monochrome TextEdit state. Native UTF-8 byte offsets
//! are distinct from an original 68k TEHandle's MacRoman offsets.
#![deny(unsafe_code)]
use mac_abi::{textedit::*, *};

use crate::toolbox::{handles::{Kind, Table},
                     quickdraw::service::Port};
mod layout;
mod model;
pub mod clipboard;
pub use model::Editor;
#[allow(unsafe_code)]
pub mod native;
struct Slot
{
    editor:         Editor,
    cursor_enabled: bool,
    cursor:         Option<CursorBinding>,
    pending_cursor: Option<CursorBinding>
}
struct CursorBinding
{
    window: WindowId,
    view:   Rect,
    port:   Port
}
pub struct Manager
{
    slots: Table<Slot>
}
impl Default for Manager
{
    fn default() -> Self
    {
        Self { slots: Table::new(Kind::TextEdit) }
    }
}
pub struct PreparedEdit
{
    id:     EditorId,
    editor: Editor
}
impl Manager
{
    /// Register a successful draw's geometry and clip. Staged registrations stay
    /// private alongside retained drawing until their window's EndUpdate.
    pub fn bind_cursor(&mut self, id: EditorId, window: WindowId, port: Port, staged: bool)
                       -> Result<(), Status>
    {
        let view = self.inspect(id).ok_or(NOT_FOUND)?.info().view;
        let slot = self.slots.get_mut(id.0).ok_or(NOT_FOUND)?;
        let binding = Some(CursorBinding { window,
                                           view,
                                           port });
        if staged
        {
            slot.pending_cursor = binding;
        }
        else
        {
            slot.cursor = binding;
        }
        Ok(())
    }

    /// Begin a replacement scene without disturbing its committed hover regions.
    pub fn begin_cursor_update(&mut self, window: WindowId)
    {
        for slot in self.slots.values_mut()
        {
            if slot.pending_cursor.as_ref().is_some_and(|binding| binding.window == window)
            {
                slot.pending_cursor = None;
            }
        }
    }

    /// Publish exactly the editors drawn in a successfully committed scene.
    pub fn commit_cursor_update(&mut self, window: WindowId)
    {
        for slot in self.slots.values_mut()
        {
            if slot.cursor.as_ref().is_some_and(|binding| binding.window == window)
            {
                slot.cursor = None;
            }
            if slot.pending_cursor.as_ref().is_some_and(|binding| binding.window == window)
            {
                slot.cursor = slot.pending_cursor.take();
            }
        }
    }

    /// Window disposal discards both visible registrations and abandoned draws.
    pub fn forget_cursor_window(&mut self, window: WindowId)
    {
        self.begin_cursor_update(window);
        for slot in self.slots.values_mut()
        {
            if slot.cursor.as_ref().is_some_and(|binding| binding.window == window)
            {
                slot.cursor = None;
            }
        }
    }

    /// The caller first resolves topmost visible content and its owning app.
    /// `local` is in that window's coordinate system, before its port origin.
    pub fn cursor_hit(&self, window: WindowId, local: Point) -> bool
    {
        self.slots.iter().any(|(_, slot)| {
                             if !slot.cursor_enabled
                             {
                                 return false;
                             }
                             let Some(binding) = &slot.cursor
                             else
                             {
                                 return false;
                             };
                             if binding.window != window
                             {
                                 return false;
                             }
                             let Some(h) = local.h.checked_sub(binding.port.origin.h)
                             else
                             {
                                 return false;
                             };
                             let Some(v) = local.v.checked_sub(binding.port.origin.v)
                             else
                             {
                                 return false;
                             };
                             let point = Point { h,
                                                 v };
                             binding.view.contains(point)
                             && binding.port
                                       .state
                                       .clip
                                       .as_ref()
                                       .is_none_or(|clip| clip.contains(point))
                         })
    }

    pub fn font_request(&mut self, call: &mut TextEditFontCall,
                        fonts: &crate::toolbox::quickdraw::fonts::FontManager)
                        -> Status
    {
        let result = (|| {
            if call.operation != SET_FONT
            {
                return Err(BAD_ARGUMENT);
            }
            let editor = self.inspect_mut(call.id).ok_or(NOT_FOUND)?;
            editor.set_font(call.font, fonts)?;
            call.info = editor.info();
            Ok(())
        })();
        result.map_or_else(|status| status, |()| OK)
    }

    pub fn request_with_fonts(&mut self, call: &mut TextEditCall, input: &str, output: &mut [u8],
                              fonts: &crate::toolbox::quickdraw::fonts::FontManager)
                              -> Status
    {
        let prepared = (|| {
            match call.operation
            {
                SET_TEXT | INSERT =>
                {
                    self.inspect_mut(call.id).ok_or(NOT_FOUND)?.prepare_font_input(input, fonts)
                }
                KEY =>
                {
                    let character = char::from_u32(call.key).ok_or(BAD_ARGUMENT)?;
                    self.inspect_mut(call.id)
                        .ok_or(NOT_FOUND)?
                        .prepare_font_input(character.encode_utf8(&mut [0; 4]), fonts)
                }
                _ => Ok(())
            }
        })();
        if let Err(status) = prepared
        {
            return status;
        }
        self.request(call, input, output)
    }

    pub fn prepare_replace_with_fonts(&self, id: EditorId, input: &str,
                                      fonts: &crate::toolbox::quickdraw::fonts::FontManager)
                                      -> Result<PreparedEdit, Status>
    {
        Ok(PreparedEdit { id,
                          editor: self.inspect(id)
                                      .ok_or(NOT_FOUND)?
                                      .prepared_replace_with_fonts(input, Some(fonts))? })
    }

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
        self.slots.get(id.0).map(|slot| &slot.editor)
    }

    pub fn inspect_mut(&mut self, id: EditorId) -> Option<&mut Editor>
    {
        self.slots.get_mut(id.0).map(|slot| &mut slot.editor)
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
            call.id = EditorId(self.slots
                                   .insert(Slot { editor,
                                                  cursor_enabled: true,
                                                  cursor: None,
                                                  pending_cursor: None })
                                   .map_err(|_| NO_MEMORY)?);
        }
        if call.operation == DISPOSE
        {
            self.slots.remove(call.id.0).ok_or(NOT_FOUND)?;
            return Ok(());
        }
        if call.operation == SET_CURSOR_ENABLED
        {
            let info = self.inspect(call.id).ok_or(NOT_FOUND)?.info();
            if call.index > 1
            {
                return Err(BAD_ARGUMENT);
            }
            self.slots.get_mut(call.id.0).ok_or(NOT_FOUND)?.cursor_enabled = call.index != 0;
            call.info = info;
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
            NAVIGATE =>
            {
                if call.index > 1
                {
                    return Err(BAD_ARGUMENT);
                }
                editor.navigate(CursorMovement::from_raw(call.key).ok_or(BAD_ARGUMENT)?,
                                call.index != 0)?;
            }
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

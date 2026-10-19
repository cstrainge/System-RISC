//! Safe Font Manager service implementation; raw pointers are handled by the trap adapter.
#![forbid(unsafe_code)]
use mac_abi::{quickdraw::*, *};

use super::{Canvas,
            fonts::{self, FontManager}};

pub fn selection(value: FontSelection) -> fonts::FontSelection
{
    fonts::FontSelection { family: value.family,
                           size:   value.size,
                           face:   value.face }
}
pub fn query(manager: &FontManager, call: &mut FontCall, text: &str) -> Status
{
    let name = match call.operation
    {
        FONT_COUNT =>
        {
            call.index = manager.catalog().len() as u32;
            return OK;
        }
        FONT_AT =>
        {
            let Some(family) = manager.catalog().get(call.index as usize)
            else
            {
                return NOT_FOUND;
            };
            call.selection.family = family.id;
            &family.name
        }
        FONT_NAME =>
        {
            match manager.family_name(call.selection.family)
            {
                Some(n) => n,
                None => return NOT_FOUND
            }
        }
        FONT_ID =>
        {
            match manager.family_id(text)
            {
                Some(id) =>
                {
                    call.selection.family = id;
                    return OK;
                }
                None => return NOT_FOUND
            }
        }
        FONT_METRICS =>
        {
            return match manager.metrics(selection(call.selection))
            {
                Ok(m) =>
                {
                    call.metrics = FontMetrics { ascent:    m.ascent,
                                                 descent:   m.descent,
                                                 leading:   m.leading,
                                                 max_width: m.max_width };
                    OK
                }
                Err(s) => s
            };
        }
        FONT_WIDTH =>
        {
            return match manager.text_width(selection(call.selection), text)
            {
                Ok(w) =>
                {
                    call.width = w;
                    OK
                }
                Err(s) => s
            };
        }
        _ => return NOT_SUPPORTED
    };
    let mut len = name.len().min(call.name.len());
    while !name.is_char_boundary(len)
    {
        len -= 1;
    }
    call.name.fill(0);
    call.name[..len].copy_from_slice(&name.as_bytes()[..len]);
    call.name_length = len as u32;
    OK
}
pub fn draw(manager: &FontManager, call: &mut FontCall, text: &str, canvas: &mut Canvas<'_>)
            -> Status
{
    match manager.draw(selection(call.selection), canvas, call.baseline, text, call.color)
    {
        Ok(w) =>
        {
            call.width = w;
            OK
        }
        Err(s) => s
    }
}

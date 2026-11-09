//! Resource-based Alert/DITL interpreter. WDEF/CDEF resources own their pixels.
#![forbid(unsafe_code)]
use alloc::{string::String, vec::Vec};

use mac_abi::{dialogs::*, geometry::Rect, *};

use super::resources::manager::Manager;
#[cfg(systemrisc)]
#[path = "core.rs"]
pub(crate) mod core_alerts;
#[derive(Clone)]
pub struct Item
{
    pub bounds:  Rect,
    pub kind:    u8,
    pub enabled: bool,
    pub text:    String,
    pub icon:    i16
}
#[derive(Clone)]
pub struct Alert
{
    pub bounds:   Rect,
    pub position: u16,
    pub items:    Vec<Item>
}
fn word(data: &[u8], at: usize) -> Result<i16, Status>
{
    Ok(i16::from_be_bytes(data.get(at..at + 2)
                              .ok_or(BAD_ARGUMENT)?
                              .try_into()
                              .map_err(|_| BAD_ARGUMENT)?))
}
fn rect(data: &[u8], at: usize) -> Result<Rect, Status>
{
    Ok(Rect::new(word(data, at + 2)?, word(data, at)?, word(data, at + 6)?, word(data, at + 4)?))
}
/// Decode the original MacRoman item and substitute without allocating an
/// intermediate decoded string. Validation precedes backing admission.
fn resource_text(payload: &[u8], substitution: &str) -> Result<String, Status>
{
    fn visit(payload: &[u8], substitution: &str,
             mut append: impl FnMut(&str) -> Result<(), Status>)
             -> Result<(), Status>
    {
        let mut offset = 0;
        while offset < payload.len()
        {
            if payload[offset..].starts_with(b"^0")
            {
                append(substitution)?;
                offset += 2;
            }
            else
            {
                let mut encoded = [0; 4];
                append(mac_formats::text::character(payload[offset]).encode_utf8(&mut encoded))?;
                offset += 1;
            }
        }
        Ok(())
    }
    let mut length = 0usize;
    visit(payload, substitution, |text| {
        length = length.checked_add(text.len()).ok_or(BAD_ARGUMENT)?;
        if length > INLINE_STRING_BYTES
        {
            return Err(BAD_ARGUMENT);
        }
        Ok(())
    })?;
    let mut text = String::new();
    text.try_reserve_exact(length).map_err(|_| NO_MEMORY)?;
    visit(payload, substitution, |part| {
        text.push_str(part);
        Ok(())
    })?;
    Ok(text)
}

/// Borrow each wrapped line from its paragraph. The candidate is the contiguous
/// original span, including repeated/trailing spaces; no temporary String or
/// second text buffer is needed. A true flag denotes a width-triggered wrap.
#[cfg(systemrisc)]
pub(super) fn wrapped_lines(text: &str, width: i16,
                            measure: impl FnMut(&str) -> Result<i16, Status>,
                            mut emit: impl FnMut(&str, bool) -> Result<(), Status>)
                            -> Result<(), Status>
{
    wrapped_line_ranges(text, width, measure, |range, wrapped| {
        emit(text.get(range).ok_or(BAD_ARGUMENT)?, wrapped)
    })
}

/// Checked byte ranges belong to this exact source, never inferred from a
/// borrowed slice's pointer or from equal text in another owner.
#[cfg(systemrisc)]
pub(super) fn wrapped_line_ranges(text: &str, width: i16,
                                  mut measure: impl FnMut(&str) -> Result<i16, Status>,
                                  mut emit: impl FnMut(core::ops::Range<usize>,
                                        bool)
                                        -> Result<(), Status>)
                                  -> Result<(), Status>
{
    let mut paragraph_start = 0usize;
    for paragraph in text.split(['\r', '\n'])
    {
        let mut line = "";
        let mut line_start = 0;
        let mut word_start = 0;
        for word in paragraph.split(' ')
        {
            let word_end = word_start + word.len();
            let candidate = if line.is_empty() { word } else { &paragraph[line_start..word_end] };
            if measure(candidate)? > width && !line.is_empty()
            {
                let start = paragraph_start.checked_add(line_start).ok_or(BAD_ARGUMENT)?;
                let end = start.checked_add(line.len()).ok_or(BAD_ARGUMENT)?;
                text.get(start..end).ok_or(BAD_ARGUMENT)?;
                emit(start..end, true)?;
                line_start = word_start;
                line = word;
            }
            else
            {
                if line.is_empty()
                {
                    line_start = word_start;
                }
                line = candidate;
            }
            word_start = word_end + 1;
        }
        let start = paragraph_start.checked_add(line_start).ok_or(BAD_ARGUMENT)?;
        let end = start.checked_add(line.len()).ok_or(BAD_ARGUMENT)?;
        text.get(start..end).ok_or(BAD_ARGUMENT)?;
        emit(start..end, false)?;
        paragraph_start = paragraph_start.checked_add(paragraph.len())
                                         .and_then(|n| n.checked_add(1))
                                         .ok_or(BAD_ARGUMENT)?;
    }
    Ok(())
}

/// Supported resource profile: ALRT, push buttons, static text and ICON items.
/// Unsupported item kinds fail before opening a partially rendered dialog.
pub fn load(resources: Manager<'_>, call: &DialogCall) -> Result<Alert, Status>
{
    if call.reserved != 0
    {
        return Err(BAD_ARGUMENT);
    }
    let text = core::str::from_utf8(call.text
                                        .get(..call.text_length as usize)
                                        .ok_or(BAD_ARGUMENT)?).map_err(|_| BAD_ARGUMENT)?;
    let (origin, resource) = resources.get(call.scope,
                                           ResourceKey { kind: *b"ALRT",
                                                         id:   call.resource })?;
    let data = &resource.data;
    if ![12, 14].contains(&data.len())
    {
        return Err(BAD_ARGUMENT);
    }
    let bounds = rect(data, 0)?;
    let width = bounds.right.checked_sub(bounds.left).ok_or(BAD_ARGUMENT)?;
    let height = bounds.bottom.checked_sub(bounds.top).ok_or(BAD_ARGUMENT)?;
    if width <= 0 || height <= 0 || width > 624 || height > 440
    {
        return Err(BAD_ARGUMENT);
    }
    if word(data, 10)? as u16 != 0x4444
    {
        return Err(NOT_SUPPORTED);
    }
    let position = if data.len() == 14 { word(data, 12)? as u16 } else { 0 };
    if ![0, 0x300a].contains(&position)
    {
        return Err(NOT_SUPPORTED);
    }
    let (_, resource) = resources.get(origin,
                                      ResourceKey { kind: *b"DITL",
                                                    id:   word(data, 8)? })?;
    let data = &resource.data;
    let count = i32::from(word(data, 0)?) + 1;
    if !(1..=32).contains(&count)
    {
        return Err(BAD_ARGUMENT);
    }
    let mut items = Vec::new();
    items.try_reserve_exact(count as usize).map_err(|_| NO_MEMORY)?;
    let mut offset = 2;
    for index in 0..count
    {
        let bounds = rect(data, offset + 4)?;
        if bounds.right.checked_sub(bounds.left).is_none_or(|w| !(0..=1024).contains(&w))
           || bounds.bottom.checked_sub(bounds.top).is_none_or(|h| !(0..=1024).contains(&h))
        {
            return Err(BAD_ARGUMENT);
        }
        let kind = *data.get(offset + 12).ok_or(BAD_ARGUMENT)?;
        let length = usize::from(*data.get(offset + 13).ok_or(BAD_ARGUMENT)?);
        let payload = data.get(offset + 14..offset + 14 + length).ok_or(BAD_ARGUMENT)?;
        let base = kind & 0x7f;
        if ![4, 8, 32].contains(&base)
        {
            return Err(NOT_SUPPORTED);
        }
        // The default CDEF includes its four-pixel ring in its own bounds.
        // Validate expansion before opening any window or doing i16 arithmetic.
        if index == 0
           && base == 4
           && kind & 0x80 == 0
           && (bounds.left.checked_sub(4).is_none()
               || bounds.top.checked_sub(4).is_none()
               || bounds.right.checked_add(4).is_none()
               || bounds.bottom.checked_add(4).is_none())
        {
            return Err(BAD_ARGUMENT);
        }
        let label = if base == 32 { String::new() } else { resource_text(payload, text)? };
        if label.len() > INLINE_STRING_BYTES
        {
            return Err(BAD_ARGUMENT);
        }
        let icon = if base == 32
        {
            if length != 2
            {
                return Err(BAD_ARGUMENT);
            }
            word(payload, 0)?
        }
        else
        {
            0
        };
        items.push(Item { bounds,
                          kind: base,
                          enabled: kind & 0x80 == 0,
                          text: label,
                          icon });
        offset = (offset + 14 + length + 1) & !1;
    }
    if offset != data.len()
    {
        return Err(BAD_ARGUMENT);
    }
    Ok(Alert { bounds,
               position,
               items })
}
#[cfg(systemrisc)]
mod modal;
#[cfg(systemrisc)]
pub(crate) use modal::Manager as Alerts;
#[cfg(systemrisc)]
pub(crate) use modal::Released as ReleasedAlert;
#[cfg(systemrisc)]
pub(crate) use modal::{ReleaseAlerts, ReleaseStorage, specification};


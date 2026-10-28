//! Resource-based Alert/DITL interpreter. WDEF/CDEF resources own their pixels.
#![forbid(unsafe_code)]
use alloc::{string::String, vec::Vec};

use mac_abi::{dialogs::*, *};

use super::resources::manager::Manager;
#[cfg(target_arch = "riscv64")]
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
#[cfg(any(target_arch = "riscv64", test))]
pub(super) fn wrapped_lines(text: &str, width: i16,
                            mut measure: impl FnMut(&str) -> Result<i16, Status>,
                            mut emit: impl FnMut(&str, bool) -> Result<(), Status>)
                            -> Result<(), Status>
{
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
                emit(line, true)?;
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
        emit(line, false)?;
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
#[cfg(target_arch = "riscv64")]
mod modal;
#[cfg(target_arch = "riscv64")]
pub(crate) use modal::Manager as Alerts;
#[cfg(target_arch = "riscv64")]
pub(crate) use modal::{ReleaseAlerts, ReleaseStorage, specification};

#[cfg(all(test, feature = "test-support"))]
mod tests
{
    use alloc::{format, string::ToString, vec::Vec};

    use super::*;
    use crate::testing::allocations::Observe;

    #[test]
    fn resource_substitution_keeps_canonical_scalars_and_checks_before_allocating()
    {
        let all: Vec<_> = (0..=255u8).collect();
        for (payload, replacement) in [(all.as_slice(), "é🦀^0"),
                                       (b"^^0 and ^0^0".as_slice(), "é🦀^0"),
                                       (b"nothing to replace".as_slice(), ""),
                                       (b"^0".as_slice(), "")]
        {
            let expected = mac_formats::text::decode(payload).replace("^0", replacement);
            assert_eq!(resource_text(payload, replacement).unwrap(), expected);
        }
        let too_long = "é".repeat(INLINE_STRING_BYTES / 2);
        let observe = Observe::begin(true, None);
        let invalid = resource_text(b"^0!", &too_long);
        let empty = resource_text(b"^0", "");
        let count = observe.finish();
        assert_eq!(invalid, Err(BAD_ARGUMENT));
        assert_eq!(empty, Ok(String::new()));
        assert_eq!(count, 0, "validate length and empty output without backing");
        let observe = Observe::begin(true, None);
        let refused = resource_text(b"Caf\x8e ^0", "🦀");
        let count = observe.finish();
        assert_eq!(refused, Err(NO_MEMORY));
        assert_eq!(count, 1, "admit only the final exact UTF-8 output");
        assert_eq!(resource_text(b"Caf\x8e ^0", "🦀").unwrap(), "Café 🦀");
    }

    fn measured(text: &str) -> i16
    {
        text.chars()
            .map(|c| {
                if c == 'W'
                {
                    3
                }
                else if !c.is_ascii()
                {
                    2
                }
                else
                {
                    1
                }
            })
            .sum()
    }

    fn original_lines(text: &str, width: i16) -> (Vec<String>, Vec<(String, bool)>)
    {
        let mut candidates = Vec::new();
        let mut emitted = Vec::new();
        let mut line = String::new();
        for paragraph in text.split(['\r', '\n'])
        {
            for word in paragraph.split(' ')
            {
                let candidate =
                    if line.is_empty() { String::from(word) } else { format!("{} {}", line, word) };
                candidates.push(candidate.clone());
                if measured(&candidate) > width && !line.is_empty()
                {
                    emitted.push((line.clone(), true));
                    line.clear();
                }
                if !line.is_empty()
                {
                    line.push(' ');
                }
                line.push_str(word);
            }
            emitted.push((line.clone(), false));
            line.clear();
        }
        (candidates, emitted)
    }

    #[test]
    fn borrowed_wraps_match_original_paragraph_spaces_and_clipped_line_order()
    {
        for text in ["", " ", "   leading", "trailing   ", "one  two   three", "\r\n",
                     "a\r\nb\n\nc", "long_unbroken_Word", "é🦀  W i  café",
                     "  é \n 🦀\tW \r end   "]
        {
            for width in [-1, 0, 1, 7, 20]
            {
                let (expected_candidates, expected_lines) = original_lines(text, width);
                let mut candidates = Vec::new();
                let mut lines = Vec::new();
                wrapped_lines(text,
                              width,
                              |candidate| {
                                  candidates.push(candidate.to_string());
                                  Ok(measured(candidate))
                              },
                              |line, wrapped| {
                                  lines.push((line.to_string(), wrapped));
                                  Ok(())
                              }).unwrap();
                assert_eq!(candidates, expected_candidates, "width={width}, text={text:?}");
                assert_eq!(lines, expected_lines, "width={width}, text={text:?}");
                // Width-triggered lines retain the original unconditional draw;
                // only each paragraph's final line obeys the vertical cutoff.
                let clipped = |rows: &[(String, bool)]| {
                    rows.iter()
                        .enumerate()
                        .filter(|(index, (_, wrapped))| *wrapped || 8 + *index as i16 * 7 <= 16)
                        .map(|(_, row)| row.clone())
                        .collect::<Vec<_>>()
                };
                assert_eq!(clipped(&lines), clipped(&expected_lines));
            }
        }
    }

    #[test]
    fn wrapping_needs_no_backing_and_preserves_measure_or_emit_refusals()
    {
        let mut lines = 0;
        let observe = Observe::begin(true, None);
        let drawn = wrapped_lines("  é🦀  W\r\nlongword   end",
                                  4,
                                  |text| Ok(measured(text)),
                                  |_, _| {
                                      lines += 1;
                                      Ok(())
                                  });
        let count = observe.finish();
        assert_eq!(drawn, Ok(()));
        assert!(lines > 4);
        assert_eq!(count, 0);
        let mut emitted = 0;
        assert_eq!(wrapped_lines("one two",
                                 3,
                                 |_| Err(NO_MEMORY),
                                 |_, _| {
                                     emitted += 1;
                                     Ok(())
                                 }),
                   Err(NO_MEMORY));
        assert_eq!(emitted, 0);
        assert_eq!(wrapped_lines("one two", 3, |text| Ok(measured(text)), |_, _| Err(NOT_FOUND)),
                   Err(NOT_FOUND));
    }
}

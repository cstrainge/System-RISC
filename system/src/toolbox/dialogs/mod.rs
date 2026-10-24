//! Resource-based Alert/DITL interpreter. WDEF/CDEF resources own their pixels.
#![forbid(unsafe_code)]
#![cfg_attr(not(target_arch = "riscv64"), allow(dead_code))]
use alloc::{string::String, vec::Vec};

use mac_abi::{dialogs::*, *};

use super::resources::manager::Manager;
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
/// Supported resource profile: ALRT, push buttons, static text and ICON items.
/// Unsupported item kinds fail before opening a partially rendered dialog.
pub fn load(resources: Manager<'_>, call: &DialogCall) -> Result<Alert, Status>
{
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
    let mut offset = 2;
    for _ in 0..count
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
        let label = if base == 32
        {
            String::new()
        }
        else
        {
            mac_formats::text::decode(payload).replace("^0", text)
        };
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
#[derive(Default)]
pub(crate) struct DialogState
{
    pub previous:    Option<mac_abi::WindowId>,
    pub window:      u64,
    pub alert:       Option<Alert>,
    pub pressed:     Option<usize>,
    pub highlighted: bool
}
impl DialogState
{
    pub fn active(&self) -> bool
    {
        self.alert.is_some()
    }

    pub fn event(&mut self, event: Event) -> (u16, bool)
    {
        let Some(alert) = &self.alert
        else
        {
            return (0, false);
        };
        let point = Point { h: event.where_.h.saturating_sub(alert.bounds.left),
                            v: event.where_.v.saturating_sub(alert.bounds.top) };
        let button =
            |index: usize| alert.items.get(index).is_some_and(|i| i.kind == 4 && i.enabled);
        let hit =
            alert.items.iter().position(|i| i.kind == 4 && i.enabled && i.bounds.contains(point));
        let old = self.highlighted;
        match event.kind
        {
            EventKind::MouseDown if event.window == self.window =>
            {
                self.pressed = hit;
                self.highlighted = hit.is_some();
            }
            EventKind::MouseUp =>
            {
                let item =
                    self.pressed.filter(|index| Some(*index) == hit).map_or(0, |i| i as u16 + 1);
                self.pressed = None;
                self.highlighted = false;
                return (item, old);
            }
            EventKind::Null =>
            {
                self.highlighted = self.pressed.is_some() && self.pressed == hit;
            }
            EventKind::KeyDown | EventKind::AutoKey =>
            {
                match event.character()
                {
                    b'\r' | 3 if button(0) => return (1, false),
                    27 if button(1) => return (2, false),
                    _ =>
                    {}
                }
            }
            _ =>
            {}
        }
        (0, old != self.highlighted)
    }
}

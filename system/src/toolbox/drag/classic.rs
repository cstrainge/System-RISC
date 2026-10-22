//! Classic flavor access shares the native store, but preserves repeatable,
//! offset-based reads. Native take-once is deliberately a different operation.
use super::*;
// Resident storage is UTF-8. Original Drag Manager TEXT uses counted MacRoman
// bytes (not Pascal strings), including its sizes and partial-read/set offsets.
fn utf8(bytes: &[u8]) -> Result<Vec<u8>, Status>
{
    let length: usize = bytes.iter().map(|&b| mac_formats::text::character(b).len_utf8()).sum();
    if length > MAX_BYTES
    {
        return Err(NO_MEMORY);
    }
    let mut result = Vec::new();
    result.try_reserve_exact(length).map_err(|_| NO_MEMORY)?;
    for &byte in bytes
    {
        let mut scalar = [0; 4];
        result.extend_from_slice(mac_formats::text::character(byte).encode_utf8(&mut scalar)
                                                                   .as_bytes());
    }
    Ok(result)
}
fn roman(bytes: &[u8]) -> Result<Vec<u8>, Status>
{
    let text = core::str::from_utf8(bytes).map_err(|_| BAD_ABI)?;
    let mut result = Vec::new();
    result.try_reserve_exact(text.chars().count()).map_err(|_| NO_MEMORY)?;
    for ch in text.chars()
    {
        let byte = if ch.is_ascii()
        {
            ch as u8
        }
        else
        {
            (128..=255).find(|&b| mac_formats::text::character(b) == ch).ok_or(-1854)?
        };
        result.push(byte);
    }
    Ok(result)
}
fn bytes(kind: [u8; 4], input: &[u8]) -> Result<Vec<u8>, Status>
{
    if kind == *b"TEXT" { utf8(input) } else { copied(input) }
}
impl Manager
{
    pub(super) fn classic_data(&mut self, owner: u64, call: &mut Call, input: &[u8],
                               output: &mut [u8])
                               -> Result<(), Status>
    {
        let session =
            self.readable(call.id, owner).map_err(|s| if s == NOT_FOUND { -1850 } else { s })?;
        let sender = session.owner == owner;
        if matches!(call.operation, CLASSIC_ADD | CLASSIC_SET) && !sender
        {
            return Err(ACCESS_DENIED);
        }
        if call.operation == CLASSIC_ADD
        {
            if session.phase != DRAFT
            {
                return Err(ACCESS_DENIED);
            }
            if call.flags & !0x107 != 0
            {
                return Err(BAD_ARGUMENT);
            }
            if input.len() > MAX_BYTES || call.index as usize > MAX_BYTES
            {
                return Err(NO_MEMORY);
            }
            let bytes = bytes(call.kind, input)?;
            if session.bytes.checked_add(bytes.len()).is_none_or(|n| n > MAX_BYTES)
            {
                return Err(NO_MEMORY);
            }
            let length = bytes.len();
            let session = self.sessions.get_mut(call.id).unwrap();
            let index = if let Some(i) = session.items.iter().position(|i| i.id == call.item)
            {
                i
            }
            else
            {
                if session.items.len() == MAX_ITEMS
                {
                    return Err(NO_MEMORY);
                }
                session.items.try_reserve(1).map_err(|_| NO_MEMORY)?;
                session.items.push(Item { id:      call.item,
                                          flavors: Vec::new(),
                                          bounds:  Rect::default() });
                session.items.len() - 1
            };
            let item = &mut session.items[index];
            if item.flavors.iter().any(|f| f.kind == call.kind)
            {
                return Err(-1853);
            }
            if item.flavors.len() == MAX_FLAVORS
            {
                return Err(NO_MEMORY);
            }
            item.flavors.try_reserve(1).map_err(|_| NO_MEMORY)?;
            item.flavors.push(Flavor { kind: call.kind,
                                       flags: call.flags,
                                       declared: call.index as usize,
                                       state: u32::from(call.error != 0),
                                       claimed: false,
                                       taken: false,
                                       native_claimed: false,
                                       error: OK,
                                       bytes });
            session.bytes += length;
            return Ok(());
        }
        let session = self.sessions.get_mut(call.id).unwrap();
        let item = session.items.iter_mut().find(|i| i.id == call.item).ok_or(-1851)?;
        if call.operation == CLASSIC_COUNT
        {
            call.flags = item.flavors.iter().filter(|f| sender || f.flags & 1 == 0).count() as u32;
            return Ok(());
        }
        if call.operation == ITEM_BOUNDS
        {
            if call.flags != 0
            {
                if !sender
                {
                    return Err(ACCESS_DENIED);
                }
                item.bounds = call.bounds;
            }
            else
            {
                call.bounds = item.bounds;
            }
            return Ok(());
        }
        let flavor = item.flavors
                         .iter_mut()
                         .find(|f| f.kind == call.kind && (sender || f.flags & 1 == 0))
                         .ok_or(-1852)?;
        match call.operation
        {
            CLASSIC_FLAVOR =>
            {
                call.flags = flavor.flags;
                call.transferred = if flavor.state != 0
                {
                    flavor.declared
                }
                else if flavor.kind == *b"TEXT"
                {
                    roman(&flavor.bytes)?.len()
                }
                else
                {
                    flavor.bytes.len()
                };
            }
            CLASSIC_SET =>
            {
                let start = call.index as usize;
                let end = start.checked_add(input.len()).ok_or(BAD_ARGUMENT)?;
                if end > MAX_BYTES
                {
                    return Err(NO_MEMORY);
                }
                let mut projected = if flavor.kind == *b"TEXT"
                {
                    roman(&flavor.bytes)?
                }
                else
                {
                    copied(&flavor.bytes)?
                };
                projected.try_reserve(end.saturating_sub(projected.len()))
                         .map_err(|_| NO_MEMORY)?;
                if end > projected.len()
                {
                    projected.resize(end, 0);
                }
                projected[start..end].copy_from_slice(input);
                let declared = projected.len();
                let replacement = bytes(flavor.kind, &projected)?;
                let total = session.bytes - flavor.bytes.len() + replacement.len();
                if total > MAX_BYTES
                {
                    return Err(NO_MEMORY);
                }
                flavor.bytes = replacement;
                flavor.declared = declared;
                flavor.state = 0;
                flavor.error = OK;
                session.bytes = total;
            }
            CLASSIC_READ =>
            {
                if flavor.state != 0
                {
                    return Err(-1854);
                }
                let projected;
                let bytes = if flavor.kind == *b"TEXT"
                {
                    projected = roman(&flavor.bytes)?;
                    projected.as_slice()
                }
                else
                {
                    &flavor.bytes
                };
                let start = call.index as usize;
                if start > bytes.len()
                {
                    return Err(BAD_ARGUMENT);
                }
                let n = output.len().min(bytes.len() - start);
                output[..n].copy_from_slice(&bytes[start..start + n]);
                call.transferred = n;
            }
            _ => return Err(NOT_SUPPORTED)
        }
        Ok(())
    }
}

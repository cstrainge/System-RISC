//! Checked 68k A-line argument marshalling. No host pointer dereferences occur here.
//! Entry is at the inline trap instruction: A7 points to reversed Pascal arguments;
//! a result slot, if present, follows them. Caller cleanup of that slot is unchanged.
#![forbid(unsafe_code)]
use mac_abi::{events::{Event, EventKind},
              geometry::{Point, Rect},
              traps::{ClassicRegisters, classic::*},
              *};
mod drag;
type Result<T> = core::result::Result<T, Status>;

pub trait Services
{
    fn drag(&mut self, _call: &mut mac_abi::drag::Call, _input: &[u8], _output: &mut [u8])
            -> Result<()>
    {
        Err(NOT_SUPPORTED)
    }
    fn event(&mut self, _mask: u16, _remove: bool) -> Result<Event>
    {
        Err(NOT_SUPPORTED)
    }
    fn font_selection(&mut self, _set: Option<mac_abi::quickdraw::FontSelection>)
                      -> Result<mac_abi::quickdraw::FontSelection>
    {
        Err(NOT_SUPPORTED)
    }
    fn font(&mut self, _operation: u32, _selection: mac_abi::quickdraw::FontSelection, _text: &str)
            -> Result<mac_abi::quickdraw::FontCall>
    {
        Err(NOT_SUPPORTED)
    }
    fn text(&mut self, _text: &str) -> Result<()>
    {
        Err(NOT_SUPPORTED)
    }
    fn cursor(&mut self, _call: &mut mac_abi::quickdraw::CursorCall) -> Result<()>
    {
        Err(NOT_SUPPORTED)
    }
    fn ticks(&self) -> u32
    {
        0
    }
}
pub struct NoServices;
impl Services for NoServices
{
}

struct Memory<'a>
{
    bytes: &'a mut [u8],
    base:  u32
}
impl Memory<'_>
{
    fn range(&self, address: u32, length: usize) -> Result<core::ops::Range<usize>>
    {
        let start = address.checked_sub(self.base).ok_or(BAD_ARGUMENT)? as usize;
        let end = start.checked_add(length).ok_or(BAD_ARGUMENT)?;
        if end > self.bytes.len()
        {
            return Err(BAD_ARGUMENT);
        }
        Ok(start..end)
    }

    fn read<const N: usize>(&self, address: u32) -> Result<[u8; N]>
    {
        Ok(self.bytes[self.range(address, N)?].try_into().unwrap())
    }

    fn word(&self, address: u32) -> Result<u16>
    {
        Ok(u16::from_be_bytes(self.read(address)?))
    }

    fn long(&self, address: u32) -> Result<u32>
    {
        Ok(u32::from_be_bytes(self.read(address)?))
    }

    fn point(&self, address: u32) -> Result<Point>
    {
        let bytes = self.read::<4>(address)?;
        Ok(Point { v: i16::from_be_bytes([bytes[0], bytes[1]]),
                   h: i16::from_be_bytes([bytes[2], bytes[3]]) })
    }

    fn write_point(&mut self, address: u32, p: Point) -> Result<()>
    {
        let [v0, v1] = p.v.to_be_bytes();
        let [h0, h1] = p.h.to_be_bytes();
        self.write(address, &[v0, v1, h0, h1])
    }

    fn rect(&self, address: u32) -> Result<Rect>
    {
        Ok(Rect::from_be_bytes(self.read(address)?))
    }

    fn write(&mut self, address: u32, bytes: &[u8]) -> Result<()>
    {
        let range = self.range(address, bytes.len())?;
        self.bytes[range].copy_from_slice(bytes);
        Ok(())
    }
}

/// Supported classic traps call the same safe rectangle/event implementations
/// used by native clients. Unsupported traps preserve registers and the stack.
pub fn dispatch(instruction: u16, registers: &mut ClassicRegisters, guest_base: u32,
                bytes: &mut [u8], services: &mut impl Services)
                -> Status
{
    let mut memory = Memory { bytes,
                              base: guest_base };
    match execute(instruction, registers, &mut memory, services)
    {
        Ok(()) => OK,
        Err(status) => status
    }
}
fn execute(instruction: u16, registers: &mut ClassicRegisters, memory: &mut Memory<'_>,
           services: &mut impl Services)
           -> Result<()>
{
    let sp = registers.a[7];
    let pc = registers.pc.checked_add(2).ok_or(BAD_ARGUMENT)?;
    let at = |offset: u32| sp.checked_add(offset).ok_or(BAD_ARGUMENT);
    let arguments = match instruction
    {
        DRAG_DISPATCH => drag::execute(registers, memory, services)?,
        SET_RECT =>
        {
            let destination = memory.long(at(8)?)?;
            let rect = Rect::new(memory.word(at(6)?)? as i16,
                                 memory.word(at(4)?)? as i16,
                                 memory.word(at(2)?)? as i16,
                                 memory.word(sp)? as i16);
            memory.write(destination, &rect.to_be_bytes())?;
            12
        }
        OFFSET_RECT | INSET_RECT =>
        {
            let destination = memory.long(at(4)?)?;
            let mut rect = memory.rect(destination)?;
            let dh = memory.word(at(2)?)? as i16;
            let dv = memory.word(sp)? as i16;
            if instruction == OFFSET_RECT
            {
                rect.offset(dh, dv)
            }
            else
            {
                rect.inset(dh, dv)
            }
            memory.write(destination, &rect.to_be_bytes())?;
            8
        }
        SECT_RECT | UNION_RECT =>
        {
            let destination = memory.long(sp)?;
            let b = memory.rect(memory.long(at(4)?)?)?;
            let a = memory.rect(memory.long(at(8)?)?)?;
            let mut call = mac_abi::traps::GeometryCall { a,
                                                          b,
                                                          ..Default::default() };
            call.operation = u32::from(instruction == UNION_RECT);
            if instruction == SECT_RECT
            {
                memory.range(at(12)?, 2)?;
            }
            super::quickdraw::geometry::dispatch(&mut call);
            memory.write(destination, &call.output.to_be_bytes())?;
            if instruction == SECT_RECT
            {
                memory.write(at(12)?, &[call.result as u8])?;
            }
            12
        }
        EMPTY_RECT =>
        {
            let value = memory.rect(memory.long(sp)?)?.is_empty();
            memory.range(at(4)?, 2)?;
            memory.write(at(4)?, &[u8::from(value)])?;
            4
        }
        EQUAL_RECT =>
        {
            let b = memory.rect(memory.long(sp)?)?;
            let a = memory.rect(memory.long(at(4)?)?)?;
            memory.range(at(8)?, 2)?;
            memory.write(at(8)?, &[u8::from(a == b)])?;
            8
        }
        PT_IN_RECT =>
        {
            let rect = memory.rect(memory.long(sp)?)?;
            let point = Point { v: memory.word(at(4)?)? as i16,
                                h: memory.word(at(6)?)? as i16 };
            memory.range(at(8)?, 2)?;
            memory.write(at(8)?, &[u8::from(rect.contains(point))])?;
            8
        }
        SET_PT =>
        {
            let destination = memory.long(at(4)?)?;
            memory.write_point(destination,
                               Point { h: memory.word(at(2)?)? as i16,
                                       v: memory.word(sp)? as i16 })?;
            8
        }
        ADD_PT | SUB_PT =>
        {
            let destination = memory.long(sp)?;
            let mut p = memory.point(destination)?;
            let delta = memory.point(at(4)?)?;
            if instruction == ADD_PT
            {
                p.h = p.h.wrapping_add(delta.h);
                p.v = p.v.wrapping_add(delta.v);
            }
            else
            {
                p.h = p.h.wrapping_sub(delta.h);
                p.v = p.v.wrapping_sub(delta.v);
            }
            memory.write_point(destination, p)?;
            8
        }
        EQUAL_PT =>
        {
            let a = memory.point(sp)?;
            let b = memory.point(at(4)?)?;
            memory.range(at(8)?, 2)?;
            memory.write(at(8)?, &[u8::from(a == b)])?;
            8
        }
        PT2_RECT =>
        {
            let destination = memory.long(sp)?;
            let a = memory.point(at(4)?)?;
            let b = memory.point(at(8)?)?;
            memory.write(destination,&Rect::new(a.h.min(b.h),a.v.min(b.v),a.h.max(b.h),a.v.max(b.v)).to_be_bytes())?;
            12
        }
        MAP_PT | SCALE_PT | MAP_RECT =>
        {
            let destination = memory.rect(memory.long(sp)?)?;
            let source = memory.rect(memory.long(at(4)?)?)?;
            let pointer = memory.long(at(8)?)?;
            if instruction == MAP_RECT
            {
                let rect =
                    super::quickdraw::geometry::map_rect(memory.rect(pointer)?,
                                                         source,
                                                         destination).map_err(|_| BAD_ARGUMENT)?;
                memory.write(pointer, &rect.to_be_bytes())?;
            }
            else
            {
                let p = memory.point(pointer)?;
                let p = if instruction == MAP_PT
                {
                    super::quickdraw::geometry::map_point(p, source, destination).map_err(|_| {
                                                                                     BAD_ARGUMENT
                                                                                 })?
                }
                else
                {
                    let sw = i64::from(source.right) - i64::from(source.left);
                    let sh = i64::from(source.bottom) - i64::from(source.top);
                    if sw == 0 || sh == 0
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    Point { h: i16::try_from(i64::from(p.h)
                                             * (i64::from(destination.right)
                                                - i64::from(destination.left))
                                             / sw).map_err(|_| BAD_ARGUMENT)?,
                            v: i16::try_from(i64::from(p.v)
                                             * (i64::from(destination.bottom)
                                                - i64::from(destination.top))
                                             / sh).map_err(|_| BAD_ARGUMENT)? }
                };
                memory.write_point(pointer, p)?;
            }
            12
        }
        TEXT_FONT | TEXT_FACE | TEXT_SIZE =>
        {
            let value = memory.word(sp)?;
            let mut selection = services.font_selection(None)?;
            match instruction
            {
                TEXT_FONT => selection.family = value as i16,
                TEXT_FACE => selection.face = value,
                _ => selection.size = value as i16
            }
            services.font_selection(Some(selection))?;
            2
        }
        GET_FONT_INFO =>
        {
            let pointer = memory.long(sp)?;
            memory.range(pointer, 8)?;
            let selection = services.font_selection(None)?;
            let reply = services.font(mac_abi::quickdraw::FONT_METRICS, selection, "")?;
            let m = reply.metrics;
            let mut out = [0; 8];
            for (i, v) in [m.ascent, m.descent, m.max_width, m.leading].iter().enumerate()
            {
                out[i * 2..i * 2 + 2].copy_from_slice(&v.to_be_bytes());
            }
            memory.write(pointer, &out)?;
            4
        }
        STRING_WIDTH
        | CHAR_WIDTH
        | TEXT_WIDTH
        | DRAW_STRING
        | mac_abi::traps::classic::DRAW_TEXT =>
        {
            let (bytes, args) = match instruction
            {
                CHAR_WIDTH =>
                {
                    let code = memory.word(sp)? as u8;
                    (alloc::vec![code], 2)
                }
                STRING_WIDTH | DRAW_STRING =>
                {
                    let pointer = memory.long(sp)?;
                    let length = memory.read::<1>(pointer)?[0] as usize;
                    let start = pointer.checked_add(1).ok_or(BAD_ARGUMENT)?;
                    (memory.bytes[memory.range(start, length)?].to_vec(), 4)
                }
                _ =>
                {
                    let count = memory.word(sp)? as i16;
                    let first = memory.word(at(2)?)? as i16;
                    if count < 0 || first < 0
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    let pointer =
                        memory.long(at(4)?)?.checked_add(first as u32).ok_or(BAD_ARGUMENT)?;
                    (memory.bytes[memory.range(pointer, count as usize)?].to_vec(), 8)
                }
            };
            let text = mac_formats::text::decode(&bytes);
            if instruction == DRAW_STRING || instruction == mac_abi::traps::classic::DRAW_TEXT
            {
                services.text(&text)?;
            }
            else
            {
                memory.range(at(args)?, 2)?;
                let selection = services.font_selection(None)?;
                let reply = services.font(mac_abi::quickdraw::FONT_WIDTH, selection, &text)?;
                memory.write(at(args)?, &reply.width.to_be_bytes())?;
            }
            args
        }
        GET_FONT_NAME =>
        {
            let pointer = memory.long(sp)?;
            memory.range(pointer, 256)?;
            let family = memory.word(at(4)?)? as i16;
            let reply = services.font(mac_abi::quickdraw::FONT_NAME,
                                      mac_abi::quickdraw::FontSelection { family,
                                                                          ..Default::default() },
                                      "")?;
            let name=core::str::from_utf8(&reply.name[..(reply.name_length as usize).min(reply.name.len())]).map_err(|_|BAD_ARGUMENT)?;
            let bytes = super::strings::encode(name)?;
            memory.write(pointer, &bytes)?;
            6
        }
        GET_F_NUM =>
        {
            let output = memory.long(sp)?;
            memory.range(output, 2)?;
            let pointer = memory.long(at(4)?)?;
            let length = memory.read::<1>(pointer)?[0] as usize;
            let start = pointer.checked_add(1).ok_or(BAD_ARGUMENT)?;
            let name = mac_formats::text::decode(&memory.bytes[memory.range(start, length)?]);
            let id = match services.font(mac_abi::quickdraw::FONT_ID, Default::default(), &name)
            {
                Ok(reply) => reply.selection.family,
                Err(NOT_FOUND) => 0,
                Err(s) => return Err(s)
            };
            memory.write(output, &id.to_be_bytes())?;
            8
        }
        SET_CURSOR | INIT_CURSOR | HIDE_CURSOR | SHOW_CURSOR | OBSCURE_CURSOR | SHIELD_CURSOR =>
        {
            use mac_abi::quickdraw as qd;
            let mut call = qd::CursorCall::default();
            let arguments = match instruction
            {
                SET_CURSOR =>
                {
                    let bytes = memory.read::<68>(memory.long(sp)?)?;
                    for i in 0..16
                    {
                        call.cursor.image[i] = u16::from_be_bytes([bytes[2 * i], bytes[2 * i + 1]]);
                        call.cursor.mask[i] =
                            u16::from_be_bytes([bytes[32 + 2 * i], bytes[33 + 2 * i]]);
                    }
                    call.cursor.hotspot = Point { v: i16::from_be_bytes([bytes[64], bytes[65]]),
                                                  h: i16::from_be_bytes([bytes[66], bytes[67]]) };
                    call.operation = qd::CURSOR_SET;
                    4
                }
                SHIELD_CURSOR =>
                {
                    call.offset = memory.point(sp)?;
                    call.bounds = memory.rect(memory.long(at(4)?)?)?;
                    call.operation = qd::CURSOR_SHIELD;
                    8
                }
                _ =>
                {
                    call.operation = match instruction
                    {
                        INIT_CURSOR => qd::CURSOR_INIT,
                        HIDE_CURSOR => qd::CURSOR_HIDE,
                        SHOW_CURSOR => qd::CURSOR_SHOW,
                        _ => qd::CURSOR_OBSCURE
                    };
                    0
                }
            };
            services.cursor(&mut call)?;
            arguments
        }
        GET_NEXT_EVENT | EVENT_AVAIL =>
        {
            let destination = memory.long(sp)?;
            let mask = memory.word(at(4)?)?;
            memory.range(destination, 16)?;
            memory.range(at(6)?, 2)?;
            let event = services.event(mask, instruction == GET_NEXT_EVENT)?;
            memory.write(destination, &event.to_be_bytes())?;
            memory.write(at(6)?, &[u8::from(event.kind != EventKind::Null)])?;
            6
        }
        TICK_COUNT =>
        {
            memory.write(sp, &services.ticks().to_be_bytes())?;
            0
        }
        _ => return Err(NOT_SUPPORTED)
    };
    registers.a[7] = sp.checked_add(arguments).ok_or(BAD_ARGUMENT)?;
    registers.pc = pc;
    Ok(())
}


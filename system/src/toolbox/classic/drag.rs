//! Original _DragDispatch Pascal stack projection. No native pointers or closure
//! addresses enter the resident store. Selectors/layouts: Apple Drag Manager
//! reference and pinned Multiversal DragMgr.h (see docs/DRAG-MANAGER.md).
use mac_abi::drag::*;

use super::*;
fn call(services: &mut impl Services, call: &mut Call, input: &[u8], output: &mut [u8]) -> Status
{
    services.drag(call, input, output).err().map_or(OK, |s| {
                                                match s
                                                {
                                                    NOT_FOUND | ACCESS_DENIED => -1850,
                                                    BAD_ARGUMENT => -50,
                                                    NO_MEMORY => -108,
                                                    other => other
                                                }
                                            })
}
pub(super) fn execute(registers: &ClassicRegisters, memory: &mut Memory<'_>,
                      services: &mut impl Services)
                      -> Result<u32>
{
    let sp = registers.a[7];
    let at = |offset| sp.checked_add(offset).ok_or(BAD_ARGUMENT);
    let selector = registers.d[0] as u16;
    let arguments = match selector
    {
        5 | 6 => 4,
        7 | 9 | 0x14 => 24,
        0xe | 0x19 | 0x1c => 8,
        0xf => 10,
        0x10 | 0x15 | 0x16 | 0x1a => 12,
        0x11 => 14,
        0x12 | 0x13 | 0x1d => 16,
        _ => return Err(NOT_SUPPORTED)
    };
    memory.range(at(arguments)?, 2)?; // Pascal OSErr result before mutation
    let mut request = Call::default();
    let status = match selector
    {
        5 =>
        {
            let output = memory.long(sp)?;
            memory.range(output, 4)?;
            request.operation = CREATE;
            let status = call(services, &mut request, &[], &mut []);
            if status == OK
            {
                memory.write(output, &request.id.to_be_bytes())?;
            }
            status
        }
        6 =>
        {
            request.operation = DISPOSE;
            request.id = memory.long(sp)?;
            call(services, &mut request, &[], &mut [])
        }
        7 | 9 =>
        {
            let flags_or_offset = memory.long(sp)?;
            let size = memory.long(at(4)?)? as i32;
            if size < 0 || size as usize > MAX_BYTES
            {
                return Err(BAD_ARGUMENT);
            }
            let pointer = memory.long(at(8)?)?;
            request.operation = if selector == 7 { CLASSIC_ADD } else { CLASSIC_SET };
            request.kind = memory.read(at(12)?)?;
            request.item = memory.long(at(16)?)?;
            request.id = memory.long(at(20)?)?;
            if selector == 7
            {
                request.flags = flags_or_offset;
                request.index = size as u32;
                request.error = i32::from(pointer == 0);
            }
            else
            {
                request.index = flags_or_offset;
            }
            let bytes = if pointer == 0 && selector == 7
            {
                &[][..]
            }
            else
            {
                &memory.bytes[memory.range(pointer, size as usize)?]
            };
            call(services, &mut request, bytes, &mut [])
        }
        0xe | 0xf | 0x10 | 0x11 | 0x12 | 0x13 =>
        {
            let output = memory.long(sp)?;
            let width = if matches!(selector, 0xe | 0x10) { 2 } else { 4 };
            memory.range(output, width)?;
            match selector
            {
                0xe =>
                {
                    request.operation = INFO;
                    request.id = memory.long(at(4)?)?;
                }
                0xf =>
                {
                    request.operation = ITEM_AT;
                    request.index = u32::from(memory.word(at(4)?)?).wrapping_sub(1);
                    request.id = memory.long(at(6)?)?;
                }
                0x10 =>
                {
                    request.operation = CLASSIC_COUNT;
                    request.item = memory.long(at(4)?)?;
                    request.id = memory.long(at(8)?)?;
                }
                0x11 =>
                {
                    request.operation = FLAVOR_AT;
                    request.index = u32::from(memory.word(at(4)?)?).wrapping_sub(1);
                    request.item = memory.long(at(6)?)?;
                    request.id = memory.long(at(10)?)?;
                }
                _ =>
                {
                    request.operation = CLASSIC_FLAVOR;
                    request.kind = memory.read(at(4)?)?;
                    request.item = memory.long(at(8)?)?;
                    request.id = memory.long(at(12)?)?;
                }
            }
            let status = call(services, &mut request, &[], &mut []);
            if status == OK
            {
                match selector
                {
                    0xe => memory.write(output, &(request.info.items as u16).to_be_bytes())?,
                    0xf => memory.write(output, &request.item.to_be_bytes())?,
                    0x10 => memory.write(output, &(request.flags as u16).to_be_bytes())?,
                    0x11 => memory.write(output, &request.kind)?,
                    0x12 => memory.write(output, &request.flags.to_be_bytes())?,
                    _ => memory.write(output, &(request.transferred as u32).to_be_bytes())?
                }
            }
            status
        }
        0x14 =>
        {
            request.operation = CLASSIC_READ;
            request.index = memory.long(sp)?;
            let length_pointer = memory.long(at(4)?)?;
            let length = memory.long(length_pointer)? as i32;
            if length < 0 || length as usize > MAX_BYTES
            {
                return Err(BAD_ARGUMENT);
            }
            let pointer = memory.long(at(8)?)?;
            let range = memory.range(pointer, length as usize)?;
            request.kind = memory.read(at(12)?)?;
            request.item = memory.long(at(16)?)?;
            request.id = memory.long(at(20)?)?;
            let status = call(services, &mut request, &[], &mut memory.bytes[range]);
            if status == OK
            {
                memory.write(length_pointer, &(request.transferred as u32).to_be_bytes())?;
            }
            status
        }
        0x15 | 0x16 =>
        {
            let pointer = memory.long(sp)?;
            memory.range(pointer, 8)?;
            request.operation = ITEM_BOUNDS;
            request.flags = u32::from(selector == 0x16);
            request.item = memory.long(at(4)?)?;
            request.id = memory.long(at(8)?)?;
            if selector == 0x16
            {
                request.bounds = memory.rect(pointer)?;
            }
            let status = call(services, &mut request, &[], &mut []);
            if status == OK && selector == 0x15
            {
                memory.write(pointer, &request.bounds.to_be_bytes())?;
            }
            status
        }
        0x1a | 0x1c | 0x1d =>
        {
            let outputs = match selector
            {
                0x1a => 2,
                0x1c => 1,
                _ => 3
            };
            let width = if selector == 0x1d { 2 } else { 4 };
            let mut pointers = [0; 3];
            for (i, pointer) in pointers[..outputs].iter_mut().enumerate()
            {
                *pointer = memory.long(at(i as u32 * 4)?)?;
                memory.range(*pointer, width)?;
            }
            request.id = memory.long(at(outputs as u32 * 4)?)?;
            request.operation = INFO;
            let status = call(services, &mut request, &[], &mut []);
            if status == OK
            {
                match selector
                {
                    0x1a =>
                    {
                        memory.write_point(pointers[0], request.info.point)?;
                        memory.write_point(pointers[1], request.info.point)?;
                    }
                    0x1c => memory.write_point(pointers[0], request.point)?,
                    _ =>
                    {
                        memory.write(pointers[0], &((request.flags >> 16) as u16).to_be_bytes())?;
                        memory.write(pointers[1], &(request.flags as u16).to_be_bytes())?;
                        memory.write(pointers[2], &request.info.modifiers.to_be_bytes())?;
                    }
                }
            }
            status
        }
        // Attributes need window/application transitions, not a fabricated zero.
        _ => return Err(NOT_SUPPORTED)
    };
    memory.write(at(arguments)?, &(status as i16).to_be_bytes())?;
    Ok(arguments)
}

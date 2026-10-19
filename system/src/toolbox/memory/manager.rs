//! Bounded native handle zone. No data pointer or alias escapes the service.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_abi::{memory::*, *};
const ZONE_BYTES: usize = 4 * 1024 * 1024;
const MAX_BYTES: usize = 1024 * 1024;
const MAX_HANDLES: usize = 512;
const HEADER: usize = 12;
#[derive(Clone, Copy)]
struct Block
{
    start: Option<usize>,
    size:  usize,
    state: u8
}
struct Slot
{
    generation: u16,
    block:      Option<Block>
}
#[derive(Default)]
pub struct Manager
{
    arena: Vec<u8>,
    slots: Vec<Slot>,
    error: Status
}
fn span(size: usize) -> usize
{
    (size + HEADER + 15) & !15
}
impl Manager
{
    fn index(&self, handle: u32) -> Result<usize, Status>
    {
        let index = (handle & 0xffff).checked_sub(1).ok_or(NIL_HANDLE_ERR)? as usize;
        let slot = self.slots.get(index).ok_or(NIL_HANDLE_ERR)?;
        if slot.generation as u32 != handle >> 16 || slot.block.is_none()
        {
            return Err(NIL_HANDLE_ERR);
        }
        Ok(index)
    }

    fn populated(&self, index: usize) -> Result<Block, Status>
    {
        self.slots[index].block.filter(|b| b.start.is_some()).ok_or(NIL_HANDLE_ERR)
    }

    fn fits(&self, start: usize, size: usize, ignore: Option<usize>) -> bool
    {
        let end = start + span(size);
        end <= ZONE_BYTES
        && !self.slots.iter().enumerate().any(|(index, slot)| {
                                             if Some(index) == ignore
                                             {
                                                 return false;
                                             }
                                             slot.block.is_some_and(|b| {
                                                           b.start.is_some_and(|other| {
                                                                      start < other + span(b.size)
                                                                      && other < end
                                                                  })
                                                       })
                                         })
    }

    fn space(&self, size: usize, ignore: Option<usize>) -> Result<usize, Status>
    {
        if size > MAX_BYTES
        {
            return Err(MEM_FULL_ERR);
        }
        let mut start = 0;
        loop
        {
            let end = start + span(size);
            if end > ZONE_BYTES
            {
                return Err(MEM_FULL_ERR);
            }
            let conflict = self.slots.iter().enumerate().find_map(|(index, slot)| {
                if Some(index) == ignore { return None; }
                let block = slot.block?;
                let other = block.start?;
                (start < other + span(block.size) && other < end).then_some(other + span(block.size))
            });
            match conflict
            {
                Some(next) => start = next,
                None => return Ok(start)
            }
        }
    }

    fn backing(&mut self, end: usize) -> Result<(), Status>
    {
        if end > self.arena.len()
        {
            self.arena.try_reserve_exact(end - self.arena.len()).map_err(|_| MEM_FULL_ERR)?;
            self.arena.resize(end, 0);
        }
        Ok(())
    }

    fn allocate(&mut self, size: usize, empty: bool) -> Result<u32, Status>
    {
        let index =
            match self.slots.iter().position(|s| s.block.is_none() && s.generation != u16::MAX)
            {
                Some(i) => i,
                None =>
                {
                    if self.slots.len() == MAX_HANDLES
                    {
                        return Err(MEM_FULL_ERR);
                    }
                    self.slots.try_reserve(1).map_err(|_| MEM_FULL_ERR)?;
                    self.slots.push(Slot { generation: 0,
                                           block:      None });
                    self.slots.len() - 1
                }
            };
        let start = if empty
        {
            None
        }
        else
        {
            let start = self.space(size, None)?;
            self.backing(start + span(size))?;
            self.arena[start + HEADER..start + HEADER + size].fill(0);
            Some(start)
        };
        let slot = &mut self.slots[index];
        slot.generation += 1;
        slot.block = Some(Block { start,
                                  size,
                                  state: 0 });
        Ok(((slot.generation as u32) << 16) | (index as u32 + 1))
    }

    fn resize(&mut self, index: usize, size: usize) -> Result<(), Status>
    {
        let block = self.populated(index)?;
        if size > MAX_BYTES
        {
            return Err(MEM_FULL_ERR);
        }
        let old = block.start.ok_or(NIL_HANDLE_ERR)?;
        let start = if self.fits(old, size, Some(index))
        {
            old
        }
        else
        {
            if block.state & LOCKED != 0
            {
                return Err(MEM_FULL_ERR);
            }
            self.space(size, Some(index))?
        };
        self.backing(start + span(size))?;
        let prefix = block.size.min(size);
        self.arena.copy_within(old + HEADER..old + HEADER + prefix, start + HEADER);
        self.arena[start + HEADER + prefix..start + HEADER + size].fill(0);
        self.slots[index].block = Some(Block { start: Some(start),
                                               size,
                                               ..block });
        Ok(())
    }

    fn bytes(&self, index: usize) -> Result<&[u8], Status>
    {
        let block = self.populated(index)?;
        let start = block.start.ok_or(NIL_HANDLE_ERR)? + HEADER;
        Ok(&self.arena[start..start + block.size])
    }

    fn copy(&self, index: usize) -> Result<Vec<u8>, Status>
    {
        let bytes = self.bytes(index)?;
        let mut result = Vec::new();
        result.try_reserve_exact(bytes.len()).map_err(|_| MEM_FULL_ERR)?;
        result.extend_from_slice(bytes);
        Ok(result)
    }

    fn write(&mut self, index: usize, offset: usize, input: &[u8]) -> Result<(), Status>
    {
        let block = self.populated(index)?;
        if offset > block.size || input.len() > block.size - offset
        {
            return Err(BAD_ARGUMENT);
        }
        let start = block.start.ok_or(NIL_HANDLE_ERR)? + HEADER + offset;
        self.arena[start..start + input.len()].copy_from_slice(input);
        Ok(())
    }

    fn append(&mut self, index: usize, input: &[u8]) -> Result<(), Status>
    {
        let old = self.populated(index)?.size;
        let size = old.checked_add(input.len()).ok_or(MEM_FULL_ERR)?;
        self.resize(index, size)?;
        self.write(index, old, input)
    }

    fn perform(&mut self, call: &mut MemoryCall, input: &[u8], output: &mut [u8])
               -> Result<(), Status>
    {
        if call.operation > REPLACE_BYTES
        {
            return Err(NOT_SUPPORTED);
        }
        match call.operation
        {
            ERROR =>
            {}
            NEW_CLEAR | NEW_EMPTY | FROM_BYTES =>
            {
                let size = match call.operation
                {
                    NEW_EMPTY => 0,
                    FROM_BYTES => input.len(),
                    _ => call.size as usize
                };
                call.handle = self.allocate(size, call.operation == NEW_EMPTY)?;
                if call.operation == FROM_BYTES
                {
                    self.write(self.index(call.handle)?, 0, input)?;
                }
            }
            _ =>
            {
                let index = self.index(call.handle)?;
                match call.operation
                {
                    DISPOSE =>
                    {
                        self.slots[index].block = None;
                    }
                    SIZE =>
                    {
                        call.size = self.populated(index)?.size as u32;
                    }
                    STATE =>
                    {
                        call.state = self.populated(index)?.state as u32;
                    }
                    SET_STATE | LOCK | UNLOCK | PURGEABLE | NONPURGEABLE =>
                    {
                        let mut block = self.populated(index)?;
                        block.state = match call.operation
                        {
                            SET_STATE => u8::try_from(call.state).map_err(|_| BAD_ARGUMENT)?,
                            LOCK => block.state | LOCKED,
                            UNLOCK => block.state & !LOCKED,
                            PURGEABLE => block.state | PURGEABLE_BIT,
                            _ => block.state & !PURGEABLE_BIT
                        };
                        self.slots[index].block = Some(block);
                    }
                    RESIZE => self.resize(index, call.size as usize)?,
                    EMPTY | REALLOCATE =>
                    {
                        let block = self.slots[index].block.ok_or(NIL_HANDLE_ERR)?;
                        if block.start.is_some() && block.state & LOCKED != 0
                        {
                            return Err(MEM_PUR_ERR);
                        }
                        let size = if call.operation == EMPTY { 0 } else { call.size as usize };
                        let start = if call.operation == EMPTY
                        {
                            None
                        }
                        else
                        {
                            let start = self.space(size, Some(index))?;
                            self.backing(start + span(size))?;
                            self.arena[start + HEADER..start + HEADER + size].fill(0);
                            Some(start)
                        };
                        self.slots[index].block = Some(Block { start,
                                                               size,
                                                               state: 0 });
                    }
                    DUPLICATE =>
                    {
                        let bytes = self.copy(index)?;
                        call.other = self.allocate(bytes.len(), false)?;
                        self.write(self.index(call.other)?, 0, &bytes)?;
                    }
                    APPEND_BYTES => self.append(index, input)?,
                    APPEND_HANDLE =>
                    {
                        let bytes = self.copy(self.index(call.other)?)?;
                        self.append(index, &bytes)?;
                    }
                    REPLACE_BYTES =>
                    {
                        self.resize(index, input.len())?;
                        self.write(index, 0, input)?;
                    }
                    READ =>
                    {
                        let bytes = self.bytes(index)?;
                        let offset = call.offset as usize;
                        if offset > bytes.len()
                        {
                            return Err(BAD_ARGUMENT);
                        }
                        let count = output.len().min(bytes.len() - offset);
                        output[..count].copy_from_slice(&bytes[offset..offset + count]);
                        call.length = count;
                    }
                    WRITE => self.write(index, call.offset as usize, input)?,
                    _ => return Err(NOT_SUPPORTED)
                }
            }
        }
        Ok(())
    }

    pub fn request(&mut self, call: &mut MemoryCall, input: &[u8], output: &mut [u8]) -> Status
    {
        let result = self.perform(call, input, output).map_or_else(|e| e, |()| OK);
        // Read/write are native safe equivalents of direct byte access: successful
        // copies do not change the original Memory Manager's last error.
        if !matches!(call.operation, READ | WRITE | ERROR)
        {
            self.error = result;
        }
        call.error = self.error;
        result
    }
}

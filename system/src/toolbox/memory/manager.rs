//! Bounded native handle zone. No data pointer or alias escapes the service.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_abi::{memory::*, *};

use crate::toolbox::handles::{Kind, Table};
const ZONE_BYTES: usize = 4 * 1024 * 1024;
const MAX_BYTES: usize = 1024 * 1024;
const HEADER: usize = 12;
#[derive(Clone, Copy)]
struct Block
{
    start: Option<usize>,
    size:  usize,
    state: u8
}
pub struct Manager
{
    arena: Vec<u8>,
    slots: Table<Block>,
    error: Status
}
impl Default for Manager
{
    fn default() -> Self
    {
        Self { arena: Vec::new(),
               slots: Table::new(Kind::Memory),
               error: OK }
    }
}
fn span(size: usize) -> usize
{
    (size + HEADER + 15) & !15
}
impl Manager
{
    /// Release an application's arena and live blocks without recycling old
    /// handle generations when its scheduler slot is assigned to another app.
    pub fn clear(&mut self)
    {
        self.arena = Vec::new();
        self.slots.clear();
        self.error = OK;
    }

    fn populated(&self, id: u32) -> Result<Block, Status>
    {
        self.slots.get(id).copied().filter(|b| b.start.is_some()).ok_or(NIL_HANDLE_ERR)
    }

    fn fits(&self, start: usize, size: usize, ignore: Option<u32>) -> bool
    {
        let end = start + span(size);
        end <= ZONE_BYTES
        && !self.slots.iter().any(|(id, block)| {
                                 if Some(id) == ignore
                                 {
                                     return false;
                                 }
                                 block.start.is_some_and(|other| {
                                                start < other + span(block.size) && other < end
                                            })
                             })
    }

    fn space(&self, size: usize, ignore: Option<u32>) -> Result<usize, Status>
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
            let conflict = self.slots.iter().find_map(|(id, block)| {
                if Some(id) == ignore { return None; }
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
        self.slots.reserve().map_err(|_| MEM_FULL_ERR)?;
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
        self.slots
            .insert(Block { start,
                            size,
                            state: 0 })
            .map_err(|_| MEM_FULL_ERR)
    }

    fn resize(&mut self, id: u32, size: usize) -> Result<(), Status>
    {
        let block = self.populated(id)?;
        if size > MAX_BYTES
        {
            return Err(MEM_FULL_ERR);
        }
        let old = block.start.ok_or(NIL_HANDLE_ERR)?;
        let start = if self.fits(old, size, Some(id))
        {
            old
        }
        else
        {
            if block.state & LOCKED != 0
            {
                return Err(MEM_FULL_ERR);
            }
            self.space(size, Some(id))?
        };
        self.backing(start + span(size))?;
        let prefix = block.size.min(size);
        self.arena.copy_within(old + HEADER..old + HEADER + prefix, start + HEADER);
        self.arena[start + HEADER + prefix..start + HEADER + size].fill(0);
        *self.slots.get_mut(id).ok_or(NIL_HANDLE_ERR)? = Block { start: Some(start),
                                                                 size,
                                                                 ..block };
        Ok(())
    }

    fn bytes(&self, id: u32) -> Result<&[u8], Status>
    {
        let block = self.populated(id)?;
        let start = block.start.ok_or(NIL_HANDLE_ERR)? + HEADER;
        Ok(&self.arena[start..start + block.size])
    }

    fn copy(&self, id: u32) -> Result<Vec<u8>, Status>
    {
        let bytes = self.bytes(id)?;
        let mut result = Vec::new();
        result.try_reserve_exact(bytes.len()).map_err(|_| MEM_FULL_ERR)?;
        result.extend_from_slice(bytes);
        Ok(result)
    }

    fn write(&mut self, id: u32, offset: usize, input: &[u8]) -> Result<(), Status>
    {
        let block = self.populated(id)?;
        if offset > block.size || input.len() > block.size - offset
        {
            return Err(BAD_ARGUMENT);
        }
        let start = block.start.ok_or(NIL_HANDLE_ERR)? + HEADER + offset;
        self.arena[start..start + input.len()].copy_from_slice(input);
        Ok(())
    }

    fn append(&mut self, id: u32, input: &[u8]) -> Result<(), Status>
    {
        let old = self.populated(id)?.size;
        let size = old.checked_add(input.len()).ok_or(MEM_FULL_ERR)?;
        self.resize(id, size)?;
        self.write(id, old, input)
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
                    self.write(call.handle, 0, input)?;
                }
            }
            _ =>
            {
                let id = call.handle;
                self.slots.get(id).ok_or(NIL_HANDLE_ERR)?;
                match call.operation
                {
                    DISPOSE =>
                    {
                        self.slots.remove(id).ok_or(NIL_HANDLE_ERR)?;
                    }
                    SIZE =>
                    {
                        call.size = self.populated(id)?.size as u32;
                    }
                    STATE =>
                    {
                        call.state = self.populated(id)?.state as u32;
                    }
                    SET_STATE | LOCK | UNLOCK | PURGEABLE | NONPURGEABLE =>
                    {
                        let mut block = self.populated(id)?;
                        block.state = match call.operation
                        {
                            SET_STATE => u8::try_from(call.state).map_err(|_| BAD_ARGUMENT)?,
                            LOCK => block.state | LOCKED,
                            UNLOCK => block.state & !LOCKED,
                            PURGEABLE => block.state | PURGEABLE_BIT,
                            _ => block.state & !PURGEABLE_BIT
                        };
                        *self.slots.get_mut(id).ok_or(NIL_HANDLE_ERR)? = block;
                    }
                    RESIZE => self.resize(id, call.size as usize)?,
                    EMPTY | REALLOCATE =>
                    {
                        let block = *self.slots.get(id).ok_or(NIL_HANDLE_ERR)?;
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
                            let start = self.space(size, Some(id))?;
                            self.backing(start + span(size))?;
                            self.arena[start + HEADER..start + HEADER + size].fill(0);
                            Some(start)
                        };
                        *self.slots.get_mut(id).ok_or(NIL_HANDLE_ERR)? = Block { start,
                                                                                 size,
                                                                                 state: 0 };
                    }
                    DUPLICATE =>
                    {
                        let bytes = self.copy(id)?;
                        call.other = self.allocate(bytes.len(), false)?;
                        self.write(call.other, 0, &bytes)?;
                    }
                    APPEND_BYTES => self.append(id, input)?,
                    APPEND_HANDLE =>
                    {
                        let bytes = self.copy(call.other)?;
                        self.append(id, &bytes)?;
                    }
                    REPLACE_BYTES =>
                    {
                        self.resize(id, input.len())?;
                        self.write(id, 0, input)?;
                    }
                    READ =>
                    {
                        let bytes = self.bytes(id)?;
                        let offset = call.offset as usize;
                        if offset > bytes.len()
                        {
                            return Err(BAD_ARGUMENT);
                        }
                        let count = output.len().min(bytes.len() - offset);
                        output[..count].copy_from_slice(&bytes[offset..offset + count]);
                        call.length = count;
                    }
                    WRITE => self.write(id, call.offset as usize, input)?,
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

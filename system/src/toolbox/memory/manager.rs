//! Bounded native handle zone. No data pointer or alias escapes the service.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_abi::{memory::*, *};

use crate::toolbox::handles::{Kind, KindLease, OwnerRef, Store, Table};
const ZONE_BYTES: usize = 4 * 1024 * 1024;
const MAX_BYTES: usize = 1024 * 1024;
const HEADER: usize = 12;
#[derive(Clone, Copy)]
struct Block
{
    wire:  u32,
    start: Option<usize>,
    size:  usize,
    state: u8
}
struct Zone
{
    arena:      Vec<u8>,
    projection: Table<u64>,
    error:      Status
}
impl Default for Zone
{
    fn default() -> Self
    {
        Self { arena:      Vec::new(),
               projection: Table::new(Kind::Memory),
               error:      OK }
    }
}
pub struct Manager
{
    blocks: Store<Block, Zone>
}
impl Manager
{
    pub(crate) fn new(kinds: (KindLease, KindLease)) -> Result<Self, Status>
    {
        Ok(Self { blocks:
                      Store::owned_state(kinds.0, kinds.1, |_| Ok(Zone::default())).map_err(|_| {
                                                                                       BAD_ARGUMENT
                                                                                   })? })
    }

    pub(crate) fn release(&mut self, ticket: mac_object_table::domain::ReleasedResource,
                          storage: bool)
                          -> Result<(), mac_object_table::domain::CompletionError>
    {
        if storage
        {
            self.blocks.release_storage(ticket)
        }
        else
        {
            let Some(block) = self.blocks.releasing(&ticket)
            else
            {
                return Err(<mac_object_table::domain::CompletionError as mac_object_table::registry::CleanupFailure>::registration_mismatch(ticket));
            };
            let wire = block.wire;
            if let Some(zone) = self.blocks.state_mut(ticket.key().owner().incarnation())
            {
                zone.projection.remove(wire);
            }
            drop(self.blocks.release(ticket)?);
            Ok(())
        }
    }

    pub(crate) fn request(&mut self, owner: &OwnerRef, call: &mut MemoryCall, input: &[u8],
                          output: &mut [u8])
                          -> Status
    {
        if let Err(error) = self.blocks.prepare(owner)
        {
            let status = match error
            {
                mac_object_table::domain::PoolError::Stopping { .. }
                | mac_object_table::domain::PoolError::RegistrationUnavailable { .. } =>
                {
                    SERVICE_BUSY
                }
                _ => MEM_FULL_ERR
            };
            call.error = status;
            return status;
        }
        Scope { blocks: &mut self.blocks,
                owner }.request(call, input, output)
    }
}
struct Scope<'a>
{
    blocks: &'a mut Store<Block, Zone>,
    owner:  &'a OwnerRef
}
fn span(size: usize) -> usize
{
    (size + HEADER + 15) & !15
}
impl Scope<'_>
{
    fn zone(&self) -> Result<&Zone, Status>
    {
        self.blocks.state(self.owner.owner().incarnation()).ok_or(SERVICE_BUSY)
    }

    fn zone_mut(&mut self) -> Result<&mut Zone, Status>
    {
        self.blocks.state_mut(self.owner.owner().incarnation()).ok_or(SERVICE_BUSY)
    }

    fn record(&self, wire: u32) -> Result<(u64, &Block), Status>
    {
        let id = *self.zone()?.projection.get(wire).ok_or(NIL_HANDLE_ERR)?;
        let (owner, block) = self.blocks.get_with_owner(id).ok_or(NIL_HANDLE_ERR)?;
        if owner != self.owner.owner() || block.wire != wire
        {
            return Err(NIL_HANDLE_ERR);
        }
        Ok((id, block))
    }

    fn block(&self, wire: u32) -> Result<&Block, Status>
    {
        self.record(wire).map(|(_, block)| block)
    }

    fn block_mut(&mut self, wire: u32) -> Result<&mut Block, Status>
    {
        let id = self.record(wire)?.0;
        self.blocks.get_mut(id).ok_or(NIL_HANDLE_ERR)
    }

    fn iter(&self) -> impl Iterator<Item = (u32, &Block)>
    {
        self.blocks
            .state(self.owner.owner().incarnation())
            .into_iter()
            .flat_map(|zone| zone.projection.iter())
            .filter_map(|(wire, id)| self.blocks.get(*id).map(|block| (wire, block)))
    }

    fn populated(&self, id: u32) -> Result<Block, Status>
    {
        let block = *self.block(id)?;
        block.start.is_some().then_some(block).ok_or(NIL_HANDLE_ERR)
    }

    fn fits(&self, start: usize, size: usize, ignore: Option<u32>) -> bool
    {
        let end = start + span(size);
        end <= ZONE_BYTES
        && !self.iter().any(|(id, block)| {
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
            let conflict = self.iter().find_map(|(id, block)| {
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
        let len = self.zone()?.arena.len();
        if end > len
        {
            self.zone_mut()?.arena.try_reserve_exact(end - len).map_err(|_| MEM_FULL_ERR)?;
            self.zone_mut()?.arena.resize(end, 0);
        }
        Ok(())
    }

    fn allocate(&mut self, size: usize, empty: bool) -> Result<u32, Status>
    {
        self.zone_mut()?.projection.reserve().map_err(|_| MEM_FULL_ERR)?;
        let start = if empty
        {
            None
        }
        else
        {
            let start = self.space(size, None)?;
            self.backing(start + span(size))?;
            self.zone_mut()?.arena[start + HEADER..start + HEADER + size].fill(0);
            Some(start)
        };
        // Reserve the classic projection before publishing its canonical block.
        // Zero is not a native identity; no request can observe this preparation
        // while the manager is borrowed. Refusal removes the unpublished entry.
        let wire = self.zone_mut()?.projection.insert(0).map_err(|_| MEM_FULL_ERR)?;
        let native = match self.blocks.insert(self.owner,
                                              Block { wire,
                                                      start,
                                                      size,
                                                      state: 0 })
        {
            Ok(id) => id,
            Err(error) =>
            {
                if let Ok(zone) = self.zone_mut()
                {
                    zone.projection.remove(wire);
                }
                return Err(match error
                {
                    mac_object_table::domain::PoolError::Stopping { .. }
                    | mac_object_table::domain::PoolError::RegistrationUnavailable { .. } =>
                    {
                        SERVICE_BUSY
                    }
                    _ => MEM_FULL_ERR
                });
            }
        };
        *self.zone_mut()?.projection.get_mut(wire).ok_or(NIL_HANDLE_ERR)? = native;
        Ok(wire)
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
        self.zone_mut()?.arena.copy_within(old + HEADER..old + HEADER + prefix, start + HEADER);
        self.zone_mut()?.arena[start + HEADER + prefix..start + HEADER + size].fill(0);
        *self.block_mut(id)? = Block { start: Some(start),
                                       size,
                                       ..block };
        Ok(())
    }

    fn bytes(&self, id: u32) -> Result<&[u8], Status>
    {
        let block = self.populated(id)?;
        let start = block.start.ok_or(NIL_HANDLE_ERR)? + HEADER;
        Ok(&self.zone()?.arena[start..start + block.size])
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
        self.zone_mut()?.arena[start..start + input.len()].copy_from_slice(input);
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
                self.block(id)?;
                match call.operation
                {
                    DISPOSE =>
                    {
                        let native = self.record(id)?.0;
                        self.zone_mut()?.projection.remove(id).ok_or(NIL_HANDLE_ERR)?;
                        drop(self.blocks.remove(native).ok_or(NIL_HANDLE_ERR)?);
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
                        *self.block_mut(id)? = block;
                    }
                    RESIZE => self.resize(id, call.size as usize)?,
                    EMPTY | REALLOCATE =>
                    {
                        let block = *self.block(id)?;
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
                            self.zone_mut()?.arena[start + HEADER..start + HEADER + size].fill(0);
                            Some(start)
                        };
                        *self.block_mut(id)? = Block { wire: id,
                                                       start,
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

    fn request(&mut self, call: &mut MemoryCall, input: &[u8], output: &mut [u8]) -> Status
    {
        let result = self.perform(call, input, output).map_or_else(|e| e, |()| OK);
        // An owner may stop on another hart while this admitted service still
        // holds manager storage. Cleanup retains that storage until this call
        // leaves; a stopped backing is a typed refusal, never an expectation.
        let Ok(zone) = self.zone_mut()
        else
        {
            call.error = SERVICE_BUSY;
            return SERVICE_BUSY;
        };
        // Successful bounded byte copies preserve the original last error.
        if !matches!(call.operation, READ | WRITE | ERROR)
        {
            zone.error = result;
        }
        call.error = zone.error;
        result
    }
}

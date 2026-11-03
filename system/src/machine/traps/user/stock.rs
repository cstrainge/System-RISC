//! One fallibly prepaid transport prefix per genuine callback-role Context.
//! The cell never owns its Context and idle storage contains no live authority.
use alloc::{sync::Arc, vec::Vec};
use core::{mem::size_of,
           sync::atomic::{AtomicU8, Ordering}};

use mac_abi::{definitions::CallbackCall, *};
use spinning_top::Spinlock;

use super::{Buffer, BufferWords, Call, Fixup, Layout, allocate};
use crate::{context::ThreadId,
            machine::{interrupts,
                      paging::user::{Access, Memory, ReaderStorage},
                      scheduler::service::RequestIdentity}};

struct Storage
{
    record:  Vec<u64>,
    encoded: Vec<u64>,
    buffers: Vec<Option<Buffer>>,
    fixups:  Vec<Fixup>,
    readers: Option<ReaderStorage>
}
impl Storage
{
    fn prepare() -> Result<Self, Status>
    {
        let record = allocate(size_of::<CallbackCall>())?;
        let encoded = allocate(size_of::<CallbackCall>())?;
        let readers = Some(ReaderStorage::prepare(size_of::<CallbackCall>())?);
        let mut buffers = Vec::new();
        buffers.try_reserve_exact(2).map_err(|_| NO_MEMORY)?;
        let mut fixups = Vec::new();
        fixups.try_reserve_exact(1).map_err(|_| NO_MEMORY)?;
        Ok(Self { record,
                  encoded,
                  buffers,
                  fixups,
                  readers })
    }

    fn valid(&self) -> bool
    {
        let words = size_of::<CallbackCall>().div_ceil(size_of::<u64>());
        self.record.len() == words
        && self.encoded.len() == words
        && self.buffers.is_empty()
        && self.buffers.capacity() >= 2
        && self.fixups.is_empty()
        && self.fixups.capacity() >= 1
        && self.readers.as_ref().is_some_and(ReaderStorage::is_empty)
    }

    fn clear(&mut self)
    {
        self.record.fill(0);
        self.encoded.fill(0);
        // These must already be authority-free before entering the cell gate.
        // clear also handles preparation errors before the vectors leave Loan.
        self.buffers.clear();
        for fixup in &mut self.fixups
        {
            fixup.original = 0;
            fixup.offset = 0;
            fixup.buffer = 0;
        }
        self.fixups.clear();
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(u8)]
enum Failure
{
    MalformedCheckout = 1,
    MalformedReturn   = 2,
    DuplicateReturn   = 3
}

pub(crate) struct Cell
{
    available: Spinlock<Option<Storage>>,
    failure:   AtomicU8
}
impl Cell
{
    pub(crate) fn prepare() -> Result<Arc<Self>, Status>
    {
        let storage = Storage::prepare()?;
        Arc::try_new(Self { available: Spinlock::new(Some(storage)),
                            failure:   AtomicU8::new(0) }).map_err(|_| NO_MEMORY)
    }

    fn observe(&self, cause: Failure)
    {
        // An internal structural failure is not a new operation receipt. Latch
        // the first bounded cause and refuse future checkout; observe outside
        // every metadata gate, without panic or repeated diagnostic traffic.
        if self.failure
               .compare_exchange(0, cause as u8, Ordering::AcqRel, Ordering::Acquire)
               .is_ok()
        {
            serial!("USER_CALLBACK_STOCK_INVARIANT cause={cause:?}");
        }
    }

    pub(super) fn checkout(self: &Arc<Self>, identity: RequestIdentity, pointer: usize,
                           bytes: usize, memory: Memory, layout: Layout, thread: ThreadId)
                           -> Result<Call, Status>
    {
        if bytes != size_of::<CallbackCall>()
        {
            return Err(BAD_ABI);
        }
        if self.failure.load(Ordering::Acquire) != 0
        {
            return Err(BAD_ABI);
        }
        let storage = interrupts::with_masked(|| {
                          let mut slot = self.available.lock();
                          if slot.as_ref().is_some_and(|storage| !storage.valid())
                          {
                              return Err(BAD_ABI);
                          }
                          slot.take().ok_or(SERVICE_BUSY)
                      }).map_err(|status| {
                            if status == BAD_ABI
                            {
                                self.observe(Failure::MalformedCheckout);
                            }
                            status
                        })?;
        let mut loan = Loan { cell: self.clone(),
                              storage };
        let readers = loan.storage.readers.take().ok_or(BAD_ABI)?;
        let mapping =
            match memory.pin_prepared(pointer, bytes, Access::ReadWrite, layout.align, readers)
            {
                Ok(mapping) => mapping,
                Err((status, readers)) =>
                {
                    loan.storage.readers = Some(readers);
                    return Err(status);
                }
            };
        let mut buffers = core::mem::take(&mut loan.storage.buffers);
        buffers.push(Some(Buffer { mapping: Some(mapping),
                                   words:
                                       BufferWords::Ordinary(core::mem::take(&mut loan.storage
                                                                                      .record)),
                                   bytes,
                                   output: true,
                                   encode: Some(layout.encode),
                                   encoded: Some(core::mem::take(&mut loan.storage.encoded)) }));
        let fixups = core::mem::take(&mut loan.storage.fixups);
        let mut call = Call { identity,
                              number: mac_abi::traps::USER_CALLBACK,
                              user_pointer: pointer,
                              size: bytes,
                              memory: Some(memory),
                              stock: Some(loan),
                              buffers,
                              fixups,
                              thread,
                              callback: None,
                              delivery: None,
                              direct: None,
                              surface_capture: None,
                              surface_return: None,
                              offscreen: None,
                              continuation: None };
        let buffer = call.owned_mut(0)?;
        let input =
            unsafe { core::slice::from_raw_parts_mut(buffer.words.as_mut_ptr().cast(), bytes) };
        buffer.mapping.as_ref().ok_or(BAD_ABI)?.copy_in(input)?;
        if !(layout.valid)(input)
        {
            return Err(BAD_ARGUMENT);
        }
        call.nested()?;
        Ok(call)
    }
}

// Private construction/unique checkout means there is at most one live loan.
// No reference to Context, Process, Activity or Job occurs in this owner.
pub(super) struct Loan
{
    cell:    Arc<Cell>,
    storage: Storage
}
impl Loan
{
    pub(super) fn restore(&mut self, record: Vec<u64>, encoded: Vec<u64>,
                          buffers: Vec<Option<Buffer>>, fixups: Vec<Fixup>,
                          readers: Option<ReaderStorage>)
    {
        self.storage = Storage { record,
                                 encoded,
                                 buffers,
                                 fixups,
                                 readers };
    }
}
impl Drop for Loan
{
    fn drop(&mut self)
    {
        self.storage.clear();
        if !self.storage.valid()
        {
            self.cell.observe(Failure::MalformedReturn);
        }
        let storage = core::mem::replace(&mut self.storage,
                                         Storage { record:  Vec::new(),
                                                   encoded: Vec::new(),
                                                   buffers: Vec::new(),
                                                   fixups:  Vec::new(),
                                                   readers: None });
        // No allocations, destruction, notifications or semantic owner releases
        // while holding this gate. An impossible duplicate preserves the first
        // available stock and drops only the already-cleared surplus outside.
        let surplus = interrupts::with_masked(|| {
            let mut slot = self.cell.available.lock();
            if slot.is_none()
            {
                *slot = Some(storage);
                None
            }
            else
            {
                Some(storage)
            }
        });
        if surplus.is_some()
        {
            self.cell.observe(Failure::DuplicateReturn);
        }
        drop(surplus);
    }
}

//! Checked user requests. A replay owns the same resident snapshot and mapping
//! pins; no resident service borrows user Rust storage or follows a user pointer.
pub(crate) mod wire;
mod callback;
pub(crate) mod stock;
pub(crate) mod surface;

use alloc::vec::Vec;
use core::{mem::{align_of, offset_of, size_of},
           ptr};

use mac_abi::{self as abi, traps::*, *};
use wire::Wire;

use crate::{machine::{paging::user::{Access, Memory, Range},
                      scheduler::service::RequestIdentity},
            toolbox::resources::definitions::{self, BoundDefinition, DefinitionRef,
                                              callbacks::{Continuation, Delivery}}};

type Encoder = unsafe fn(*const u8, *mut u8);
unsafe fn encode<T: Wire>(input: *const u8, output: *mut u8)
{
    unsafe { T::encode(input.cast::<T>(), output) };
}
struct Layout
{
    align:  usize,
    valid:  fn(&[u8]) -> bool,
    encode: Encoder
}
impl Layout
{
    fn of<T: Wire>() -> Self
    {
        Self { align:  align_of::<T>(),
               valid:  T::validate,
               encode: encode::<T> }
    }
}
// The whole affine prepaid loan stays here until the authentic RETURN commit.
// It is never reduced to a raw Vec or detached from its original broker Job.
enum BufferWords
{
    Ordinary(Vec<u64>),
    Prepaid(definitions::callbacks::ReturnLoan)
}
impl core::ops::Deref for BufferWords
{
    type Target = [u64];

    fn deref(&self) -> &[u64]
    {
        match self
        {
            Self::Ordinary(words) => words,
            Self::Prepaid(loan) => loan.storage().words()
        }
    }
}
impl core::ops::DerefMut for BufferWords
{
    fn deref_mut(&mut self) -> &mut [u64]
    {
        match self
        {
            Self::Ordinary(words) => words,
            Self::Prepaid(loan) => loan.storage_mut().words_mut()
        }
    }
}
struct Buffer
{
    mapping: Option<Range>,
    words:   BufferWords,
    bytes:   usize,
    output:  bool,
    encode:  Option<Encoder>,
    encoded: Option<Vec<u64>>
}
impl Buffer
{
    fn pointer(&mut self) -> *mut u8
    {
        self.words.as_mut_ptr().cast()
    }

    fn input(&self) -> &[u8]
    {
        // All words are initialized before any typed service sees this buffer.
        unsafe { core::slice::from_raw_parts(self.words.as_ptr().cast(), self.bytes) }
    }

    fn release_prepaid_mapping(&mut self) -> Result<(), Status>
    {
        if let BufferWords::Prepaid(loan) = &mut self.words
        {
            if let Some(mapping) = self.mapping.take()
            {
                let readers = mapping.release_storage();
                loan.restore_reader_storage(readers).map_err(|(status, readers)| {
                                                         drop(readers);
                                                         status
                                                     })?;
            }
        }
        Ok(())
    }
}
impl Drop for Buffer
{
    fn drop(&mut self)
    {
        if let Err(cause) = self.release_prepaid_mapping()
        {
            // Real metadata failure, outside all gates; the original Job loan
            // still returns through its own Drop without fabricating an ACK.
            serial!("USER_CALLBACK_READER_RESTORE_REFUSED cause={cause}");
        }
    }
}
struct Fixup
{
    buffer:   usize,
    offset:   usize,
    original: usize
}
/// Owned copied bytes keep their original aligned allocation layout. Callback
/// results may move this owner; no borrowed user storage or Vec layout cast.
pub(crate) struct OwnedBytes
{
    words: Vec<u64>,
    bytes: usize
}
impl OwnedBytes
{
    pub(crate) fn prepare_callback(bytes: usize) -> Result<Self, Status>
    {
        Ok(Self { words: allocate(bytes)?,
                  bytes })
    }

    pub(crate) fn words(&self) -> &[u64]
    {
        &self.words
    }

    pub(crate) fn words_mut(&mut self) -> &mut [u64]
    {
        &mut self.words
    }

    pub(crate) fn take(&mut self) -> Self
    {
        Self { words: core::mem::take(&mut self.words),
               bytes: core::mem::take(&mut self.bytes) }
    }

    pub(crate) fn bytes(&self) -> &[u8]
    {
        // Only checked transport extraction constructs this initialized span.
        unsafe { core::slice::from_raw_parts(self.words.as_ptr().cast(), self.bytes) }
    }

    pub(crate) fn len(&self) -> usize
    {
        self.bytes
    }
}
impl AsRef<[u8]> for OwnedBytes
{
    fn as_ref(&self) -> &[u8]
    {
        self.bytes()
    }
}

enum CallbackInput
{
    Ordinary(OwnedBytes),
    Prepaid(definitions::callbacks::ReturnLoan)
}

// Minted only while copying this actual CallbackCall. Callers cannot choose a
// buffer index or substitute an unrelated resident pointer as input authority.
struct CallbackBuffer
{
    operation: u32,
    bytes:     usize,
    index:     Option<usize>
}

/// Captured from this envelope's real ProcessSpace and original addresses.
/// Copying this private view grants no authority to change the held source.
#[derive(Clone, Copy)]
pub(crate) struct DirectArguments
{
    pub(crate) reference: DefinitionRef,
    pub(crate) owner:     mac_object_table::domain::DomainId,
    pub(crate) image_id:  u64,
    pub(crate) object:    usize,
    pub(crate) surface:   usize,
    pub(crate) param:     i64,
    pub(crate) process:   bool
}
struct DirectDefinition
{
    arguments: DirectArguments,
    _binding:  BoundDefinition
}

/// Only the current authenticated outer U request constructs this envelope.
/// Its context slot retains it through deferred I/O/replay and releases it at
/// real completion or stop, outside scheduler/manager locks.
pub(crate) struct Call
{
    identity:                RequestIdentity,
    number:                  u32,
    user_pointer:            usize,
    size:                    usize,
    memory:                  Option<Memory>,
    stock:                   Option<stock::Loan>,
    buffers:                 Vec<Option<Buffer>>,
    fixups:                  Vec<Fixup>,
    thread:                  crate::context::ThreadId,
    callback:                Option<CallbackBuffer>,
    delivery:                Option<Delivery>,
    direct:                  Option<DirectDefinition>,
    surface_capture:         Option<surface::Capture>,
    surface_return:          Option<surface::Origin>,
    offscreen:               Option<crate::toolbox::controls::offscreen::Operation>,
    pub(crate) continuation: Option<Continuation>
}

fn allocate(bytes: usize) -> Result<Vec<u64>, Status>
{
    let count = bytes.checked_add(7).ok_or(BAD_ARGUMENT)? / 8;
    let mut words = Vec::new();
    words.try_reserve_exact(count).map_err(|_| NO_MEMORY)?;
    words.resize(count, 0);
    Ok(words)
}
fn checked_span(start: usize, bytes: usize) -> Result<core::ops::Range<usize>, Status>
{
    if bytes > isize::MAX as usize
    {
        return Err(BAD_ARGUMENT);
    }
    Ok(start..start.checked_add(bytes).ok_or(BAD_ARGUMENT)?)
}

pub(crate) fn prepare_or_resume(identity: RequestIdentity, number: u32, pointer: *mut (),
                                bytes: usize)
                                -> Result<Call, Status>
{
    let context = crate::context::current_thread_context();
    if let Some(call) = context.take_user_call(identity, number, pointer.addr(), bytes)?
    {
        return Ok(call);
    }
    let layout = layout(number, bytes)?;
    if layout.align > align_of::<u64>()
    {
        return Err(BAD_ABI);
    }
    let memory = Memory::current()?;
    if number == USER_CALLBACK
    {
        // The installed cell proves preparation on a genuine callback Context;
        // actual running-thread authentication precedes any stock checkout.
        crate::machine::scheduler::callbacks::authenticate_current()?;
        let cell = context.callback_stock().ok_or(ACCESS_DENIED)?;
        return cell.checkout(identity,
                             pointer.addr(),
                             bytes,
                             memory,
                             layout,
                             context.identity().thread);
    }
    let mapping = memory.pin(pointer.addr(), bytes, Access::ReadWrite, layout.align)?;
    let mut words = allocate(bytes)?;
    let input = unsafe { core::slice::from_raw_parts_mut(words.as_mut_ptr().cast(), bytes) };
    mapping.copy_in(input)?;
    if !(layout.valid)(input)
    {
        return Err(BAD_ARGUMENT);
    }
    let mut buffers = Vec::new();
    buffers.try_reserve_exact(1).map_err(|_| NO_MEMORY)?;
    let encoded = allocate(bytes)?;
    buffers.push(Some(Buffer { mapping: Some(mapping),
                               words: BufferWords::Ordinary(words),
                               bytes,
                               output: true,
                               encode: Some(layout.encode),
                               encoded: Some(encoded) }));
    let mut call = Call { identity,
                          number,
                          user_pointer: pointer.addr(),
                          size: bytes,
                          memory: Some(memory),
                          stock: None,
                          buffers,
                          fixups: Vec::new(),
                          thread: context.identity().thread,
                          callback: None,
                          delivery: None,
                          direct: None,
                          surface_capture: None,
                          surface_return: None,
                          offscreen: None,
                          continuation: None };
    call.nested()?;
    Ok(call)
}
impl Call
{
    pub(crate) fn dispatch_definition(
        &mut self, suspension: Result<(), Status>)
        -> Option<crate::machine::scheduler::service::RequestResult>
    {
        if let Some(result) = self.dispatch_surface(suspension)
        {
            return Some(result);
        }
        let direct = self.definition_arguments();
        let (pointer, size) = match self.request()
        {
            Ok(request) => request,
            Err(status) =>
            {
                return Some(crate::machine::scheduler::service::RequestResult::done(status));
            }
        };
        let number = native_string_trap(self.number).unwrap_or(self.number);
        // The envelope owns the exclusive snapshot and every nested buffer.
        unsafe {
            definitions::callbacks::dispatch_user(number, pointer, size, &mut self.continuation,
                                                  direct, suspension)
        }
    }

    pub(crate) fn definition_arguments(&self) -> Option<DirectArguments>
    {
        self.direct.as_ref().map(|direct| direct.arguments)
    }

    pub(crate) fn matches(&self, identity: RequestIdentity, number: u32, pointer: usize,
                          bytes: usize)
                          -> bool
    {
        self.identity == identity
        && self.number == number
        && self.user_pointer == pointer
        && self.size == bytes
    }

    pub(crate) fn request(&mut self) -> Result<(*mut (), usize), Status>
    {
        Ok((self.owned_mut(0)?.pointer().cast(), self.size))
    }

    fn owned(&self, index: usize) -> Result<&Buffer, Status>
    {
        self.buffers.get(index).and_then(Option::as_ref).ok_or(BAD_ABI)
    }

    fn owned_mut(&mut self, index: usize) -> Result<&mut Buffer, Status>
    {
        self.buffers.get_mut(index).and_then(Option::as_mut).ok_or(BAD_ABI)
    }

    pub(crate) fn retain(self) -> Result<(), (Status, Self)>
    {
        let context = crate::context::current_thread_context();
        if context.identity().thread != self.thread
        {
            return Err((ACCESS_DENIED, self));
        }
        context.park_user_call(self)
    }

    fn callback_buffer(&mut self, operation: u32) -> Result<(Option<usize>, usize), Status>
    {
        if crate::context::current_thread_context().identity().thread != self.thread
           || self.number != USER_CALLBACK
           || self.size != size_of::<abi::definitions::CallbackCall>()
        {
            return Err(ACCESS_DENIED);
        }
        let (captured_operation, captured_bytes, captured_index) = {
            let captured = self.callback.as_ref().ok_or(BAD_ARGUMENT)?;
            (captured.operation, captured.bytes, captured.index)
        };
        if captured_operation != operation
        {
            return Err(ACCESS_DENIED);
        }
        let call =
            unsafe { &*self.owned_mut(0)?.pointer().cast::<abi::definitions::CallbackCall>() };
        if call.header.version != ABI_VERSION
           || call.header.size as usize != size_of::<abi::definitions::CallbackCall>()
           || call.operation != operation
           || call.reserved != 0
           || call.reserved2 != 0
           || call.length != captured_bytes
        {
            return Err(BAD_ARGUMENT);
        }
        let expected = match captured_index
        {
            Some(index) =>
            {
                let buffer = self.owned(index)?;
                if index == 0
                   || buffer.bytes != captured_bytes
                   || buffer.words
                            .len()
                            .checked_mul(size_of::<u64>())
                            .is_none_or(|n| n < buffer.bytes)
                   || buffer.encode.is_some()
                   || buffer.output != (operation == abi::definitions::CALLBACK_NEXT)
                {
                    return Err(BAD_ABI);
                }
                buffer.words.as_ptr().addr()
            }
            None if captured_bytes == 0 => 0,
            None => return Err(BAD_ABI)
        };
        if call.buffer.addr() != expected
        {
            return Err(ACCESS_DENIED);
        }
        Ok((captured_index, captured_bytes))
    }

    /// Attach the genuine broker delivery after its authentic callback-thread
    /// admission. Refusal returns that same affine delivery for outside Drop.
    pub(crate) fn attach_delivery(&mut self, delivery: Delivery) -> Result<(), (Status, Delivery)>
    {
        if self.delivery.is_some()
        {
            return Err((SERVICE_BUSY, delivery));
        }
        let (index, capacity) = match self.callback_buffer(abi::definitions::CALLBACK_NEXT)
        {
            Ok(value) => value,
            Err(status) => return Err((status, delivery))
        };
        let length = delivery.bytes().len();
        if length > capacity
        {
            return Err((BAD_ARGUMENT, delivery));
        }
        let record = match self.owned_mut(0)
        {
            Ok(buffer) => buffer.pointer(),
            Err(status) => return Err((status, delivery))
        };
        match index
        {
            Some(index) =>
            {
                let output = match self.owned_mut(index)
                {
                    Ok(buffer) => buffer.pointer(),
                    Err(status) => return Err((status, delivery))
                };
                unsafe { ptr::copy_nonoverlapping(delivery.bytes().as_ptr(), output, length) };
            }
            None if length == 0 => (),
            None => return Err((BAD_ABI, delivery))
        }
        let call = unsafe { &mut *record.cast::<abi::definitions::CallbackCall>() };
        call.required = length;
        call.present = 1;
        self.callback.take();
        self.delivery = Some(delivery);
        Ok(())
    }

    /// Move only this exact captured RETURN allocation; slot/mapping/fixup
    /// indices remain intact and the input is never part of output copying.
    fn take_callback_input(&mut self) -> Result<CallbackInput, Status>
    {
        let (index, bytes) = self.callback_buffer(abi::definitions::CALLBACK_RETURN)?;
        if let Some(index) = index
        {
            self.owned_mut(index)?.release_prepaid_mapping()?;
        }
        self.callback.take();
        let words = match index
        {
            Some(index) =>
            {
                core::mem::replace(&mut self.owned_mut(index)?.words,
                                   BufferWords::Ordinary(Vec::new()))
            }
            None => BufferWords::Ordinary(Vec::new())
        };
        Ok(match words
        {
            BufferWords::Ordinary(words) =>
            {
                CallbackInput::Ordinary(OwnedBytes { words,
                                                     bytes })
            }
            BufferWords::Prepaid(loan) => CallbackInput::Prepaid(loan)
        })
    }

    pub(crate) fn finish(mut self) -> Result<(), Status>
    {
        self.restore_surface()?;
        if self.buffers.iter().any(Option::is_none)
        {
            return Err(BAD_ABI);
        }
        // Restore every original user address before encoding. Resident aliases
        // never escape through pointer-bearing output records.
        for at in 0..self.fixups.len()
        {
            let (buffer, offset, original) = {
                let fixup = &self.fixups[at];
                (fixup.buffer, fixup.offset, fixup.original)
            };
            let destination = self.owned_mut(buffer)?.words.as_mut_ptr().cast::<u8>();
            unsafe { ptr::write_unaligned(destination.add(offset).cast::<usize>(), original) };
        }
        // Nested output precedes the record, matching the service's direct ABI.
        // Each mapping remains pinned until every copy has finished.
        for index in (0..self.buffers.len()).rev()
        {
            let buffer = self.owned_mut(index)?;
            if !buffer.output
            {
                continue;
            }
            if let Some(encode) = buffer.encode
            {
                let encoded = buffer.encoded.as_mut().ok_or(BAD_ABI)?;
                unsafe { encode(buffer.words.as_ptr().cast(), encoded.as_mut_ptr().cast()) };
                let bytes =
                    unsafe { core::slice::from_raw_parts(encoded.as_ptr().cast(), buffer.bytes) };
                buffer.mapping.as_ref().ok_or(BAD_ABI)?.copy_out(bytes)?;
            }
            else
            {
                buffer.mapping.as_ref().ok_or(BAD_ABI)?.copy_out(buffer.input())?;
            }
        }
        if let Some(delivery) = self.delivery.take()
        {
            delivery.commit()?;
        }
        Ok(())
    }

    fn buffer(&mut self, address: usize, bytes: usize, access: Access, align: usize, output: bool,
              layout: Option<Layout>)
              -> Result<usize, Status>
    {
        let mapping = self.memory.as_ref().ok_or(BAD_ABI)?.pin(address, bytes, access, align)?;
        let mut words = allocate(bytes)?;
        let input = unsafe { core::slice::from_raw_parts_mut(words.as_mut_ptr().cast(), bytes) };
        mapping.copy_in(input)?;
        if let Some(layout) = &layout
        {
            if !(layout.valid)(input)
            {
                return Err(BAD_ARGUMENT);
            }
        }
        self.buffers.try_reserve(1).map_err(|_| NO_MEMORY)?;
        let index = self.buffers.len();
        let encoded = if layout.is_some() { Some(allocate(bytes)?) } else { None };
        self.buffers.push(Some(Buffer { mapping: Some(mapping),
                                        words: BufferWords::Ordinary(words),
                                        bytes,
                                        output,
                                        encode: layout.map(|x| x.encode),
                                        encoded }));
        Ok(index)
    }

    fn callback_return_buffer(&mut self, bytes: usize) -> Result<Option<usize>, Status>
    {
        let execution = crate::machine::scheduler::callbacks::authenticate_current()?;
        let loan =
            definitions::callbacks::checkout_prepaid_return(execution.queue(), &execution, bytes)?;
        let Some(mut loan) = loan
        else
        {
            return self.point(0,
                              offset_of!(abi::definitions::CallbackCall, buffer),
                              bytes,
                              align_of::<u8>(),
                              false);
        };
        let offset = offset_of!(abi::definitions::CallbackCall, buffer);
        let original = unsafe {
            ptr::read_unaligned(self.owned_mut(0)?.pointer().add(offset).cast::<usize>())
        };
        if loan.storage().len() != bytes
        {
            return Err(BAD_ABI);
        }
        if bytes == 0
        {
            // A prepaid Packet necessarily includes its nonempty checked wire
            // header. Do not silently turn a malformed loan into ordinary input.
            return Err(BAD_ABI);
        }
        let memory = self.memory.as_ref().ok_or(BAD_ABI)?;
        let readers = loan.take_reader_storage()?;
        let mapping =
            match memory.pin_prepared(original, bytes, Access::Read, align_of::<u8>(), readers)
            {
                Ok(mapping) => mapping,
                Err((status, readers)) =>
                {
                    if let Err((cause, readers)) = loan.restore_reader_storage(readers)
                    {
                        serial!("USER_CALLBACK_READER_RESTORE_REFUSED original={status} \
                                 cause={cause}");
                        drop(readers);
                    }
                    return Err(status);
                }
            };
        // The local Buffer is already the affine return guard before copy or
        // any further fallible admission. Its Drop releases real readers and
        // restores the same empty metadata before returning the Job loan.
        let mut buffer = Buffer { mapping: Some(mapping),
                                  words: BufferWords::Prepaid(loan),
                                  bytes,
                                  output: false,
                                  encode: None,
                                  encoded: None };
        if buffer.words.len().checked_mul(size_of::<u64>()).is_none_or(|n| n < bytes)
        {
            return Err(BAD_ABI);
        }
        let input =
            unsafe { core::slice::from_raw_parts_mut(buffer.words.as_mut_ptr().cast(), bytes) };
        buffer.mapping.as_ref().ok_or(BAD_ABI)?.copy_in(input)?;
        self.buffers.try_reserve(1).map_err(|_| NO_MEMORY)?;
        let index = self.buffers.len();
        self.buffers.push(Some(buffer));
        let replacement = self.owned_mut(index)?.pointer();
        self.fixup(0, offset, original, replacement)?;
        Ok(Some(index))
    }

    fn point(&mut self, buffer: usize, offset: usize, bytes: usize, align: usize, output: bool)
             -> Result<Option<usize>, Status>
    {
        let original = unsafe {
            ptr::read_unaligned(self.owned_mut(buffer)?.pointer().add(offset).cast::<usize>())
        };
        if bytes == 0
        {
            // Zero-length native spans are never dereferenced by services.
            self.fixup(buffer, offset, original, ptr::null_mut())?;
            return Ok(None);
        }
        let access = if output { Access::ReadWrite } else { Access::Read };
        let index = self.buffer(original, bytes, access, align, output, None)?;
        let pointer = self.owned_mut(index)?.pointer();
        self.fixup(buffer, offset, original, pointer)?;
        Ok(Some(index))
    }

    fn fixup(&mut self, buffer: usize, offset: usize, original: usize, replacement: *mut u8)
             -> Result<(), Status>
    {
        self.fixups.try_reserve(1).map_err(|_| NO_MEMORY)?;
        self.fixups.push(Fixup { buffer,
                                 offset,
                                 original });
        unsafe {
            ptr::write_unaligned(self.owned_mut(buffer)?.pointer().add(offset).cast::<*mut u8>(),
                                 replacement)
        };
        Ok(())
    }

    fn surface(&mut self, offset: usize) -> Result<(), Status>
    {
        let original = unsafe {
            ptr::read_unaligned(self.owned_mut(0)?.pointer().add(offset).cast::<usize>())
        };
        if original == 0
        {
            return Ok(());
        }
        let index = self.buffer(original,
                                size_of::<Surface>(),
                                Access::ReadWrite,
                                align_of::<Surface>(),
                                true,
                                Some(Layout::of::<Surface>()))?;
        let surface = unsafe { &*self.owned_mut(index)?.pointer().cast::<Surface>() };
        let bytes = (surface.width as usize).checked_mul(surface.height as usize)
                                            .and_then(|x| x.checked_mul(size_of::<u32>()))
                                            .ok_or(BAD_ARGUMENT)?;
        let pixels =
            self.point(index, offset_of!(Surface, pixels), bytes, align_of::<u32>(), true)?;
        self.surface_capture = Some(surface::Capture { field: offset,
                                                       header: index,
                                                       pixels });
        let pointer = self.owned_mut(index)?.pointer();
        self.fixup(0, offset, original, pointer)
    }

    fn definition_value<T: Wire>(&mut self, field: usize, address: usize, output: bool)
                                 -> Result<usize, Status>
    {
        let index = self.buffer(address,
                                size_of::<T>(),
                                if output { Access::ReadWrite } else { Access::Read },
                                align_of::<T>(),
                                output,
                                Some(Layout::of::<T>()))?;
        let pointer = self.owned_mut(index)?.pointer();
        self.fixup(0, field, address, pointer)?;
        Ok(index)
    }

    fn definition_nested(&mut self) -> Result<(), Status>
    {
        use abi::{controls::reflow as r, definitions as d};
        let call = unsafe { self.owned_mut(0)?.pointer().cast::<d::UserCall>().read() };
        if !call.header.supports::<d::UserCall>() || call.reserved != 0
        {
            return Err(BAD_ABI);
        }
        let process = crate::loader::process_definition(call.kind.to_be_bytes(), call.id)
            .map_err(crate::loader::status)?;
        if !process && !matches!(call.kind, d::WDEF | d::CDEF | abi::menus::MDEF)
        {
            return Err(NOT_SUPPORTED);
        }
        let reference = crate::loader::prepare_definition(call.kind.to_be_bytes(), call.id)
            .map_err(crate::loader::status)?;
        let binding = definitions::bind(reference)?;
        let process = definitions::is_user(reference)?;
        let arguments =
            DirectArguments { reference,
                              owner: self.memory.as_ref().ok_or(BAD_ABI)?.owner(),
                              image_id: self.memory.as_ref().ok_or(BAD_ABI)?.image_id(),
                              object: call.object,
                              surface: call.surface,
                              param: call.param,
                              process };
        self.direct = Some(DirectDefinition { arguments,
                                              _binding: binding });
        if process
        {
            // Only that same Process's U callback consumes these addresses.
            // No kernel buffer/reference is formed for opaque author payloads.
            return Ok(());
        }
        let object = offset_of!(d::UserCall, object);
        let param = offset_of!(d::UserCall, param);
        match call.kind
        {
            d::WDEF =>
            {
                self.definition_value::<d::WindowDefinitionState>(object, call.object, true)?;
                if call.message == d::GROW
                {
                    self.definition_value::<abi::geometry::Rect>(param, call.param as usize,
                                                                 false)?;
                }
            }
            d::CDEF =>
            {
                if call.message == abi::controls::CHANGE_BOUNDS_DEFINITION
                {
                    self.definition_value::<abi::controls::ControlChange>(object, call.object,
                                                                          true)?;
                }
                else if call.id == 1
                {
                    self.definition_value::<abi::controls::ScrollbarState>(object, call.object,
                                                                           true)?;
                }
                else
                {
                    self.definition_value::<d::ControlDefinitionState>(object, call.object, true)?;
                }
                if call.message == r::CALCULATE_GRID_AXIS
                {
                    let index =
                        self.definition_value::<r::GridAxisCall>(param, call.param as usize, true)?;
                    let axis = unsafe {
                        self.owned_mut(index)?.pointer().cast::<r::GridAxisCall>().read()
                    };
                    let tracks = axis.track_count
                                     .checked_mul(size_of::<r::GridTrack>())
                                     .ok_or(BAD_ARGUMENT)?;
                    let items = axis.item_count
                                    .checked_mul(size_of::<r::GridItem>())
                                    .ok_or(BAD_ARGUMENT)?;
                    if axis.tracks.is_null() || axis.items.is_null()
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    let a = checked_span(axis.tracks.addr(), tracks)?;
                    let b = checked_span(axis.items.addr(), items)?;
                    let c = checked_span(call.param as usize, size_of::<r::GridAxisCall>())?;
                    for (x, y) in [(&a, &b), (&a, &c), (&b, &c)]
                    {
                        if x.start < y.end && y.start < x.end
                        {
                            return Err(BAD_ARGUMENT);
                        }
                    }
                    // These scalar-only array layouts contain no padding.
                    // A future layout change must add an explicit slice encoder.
                    const {
                        assert!(size_of::<r::GridTrack>() == 96);
                    }
                    const {
                        assert!(size_of::<r::GridItem>() == 48);
                    }
                    self.point(index,
                               offset_of!(r::GridAxisCall, tracks),
                               tracks,
                               align_of::<r::GridTrack>(),
                               true)?;
                    self.point(index,
                               offset_of!(r::GridAxisCall, items),
                               items,
                               align_of::<r::GridItem>(),
                               true)?;
                    self.zero_definition_span(index, offset_of!(r::GridAxisCall, tracks), tracks)?;
                    self.zero_definition_span(index, offset_of!(r::GridAxisCall, items), items)?;
                }
                else if (r::ASK_SIZE..=r::RESET_SPLIT_SIZES).contains(&call.message)
                {
                    self.definition_value::<r::Payload>(param, call.param as usize, true)?;
                }
                else if call.id == abi::controls::CDEF_TABLE
                          && call.message == d::DRAW
                          && call.param != 0
                {
                    let index =
                        self.definition_value::<r::GridPaint>(param, call.param as usize, false)?;
                    let grid =
                        unsafe { self.owned_mut(index)?.pointer().cast::<r::GridPaint>().read() };
                    if grid.cells.is_null()
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    let bytes = grid.count
                                    .checked_mul(size_of::<abi::geometry::Rect>())
                                    .ok_or(BAD_ARGUMENT)?;
                    self.point(index,
                               offset_of!(r::GridPaint, cells),
                               bytes,
                               align_of::<abi::geometry::Rect>(),
                               false)?;
                    self.zero_definition_span(index, offset_of!(r::GridPaint, cells), bytes)?;
                }
            }
            abi::menus::MDEF =>
            {
                self.definition_value::<abi::menus::MenuDefinitionState>(object, call.object,
                                                                         true)?;
            }
            _ => return Err(NOT_SUPPORTED)
        }
        self.surface(offset_of!(d::UserCall, surface))
    }

    fn zero_definition_span(&mut self, index: usize, offset: usize, bytes: usize)
                            -> Result<(), Status>
    {
        if bytes == 0
        {
            // A zero-sized ABI slice still requires a nonnull aligned pointer.
            // The existing private fixup retains/restores the original U value.
            let empty = self.owned_mut(0)?.pointer();
            unsafe {
                ptr::write_unaligned(self.owned_mut(index)?
                                         .pointer()
                                         .add(offset)
                                         .cast::<*mut u8>(),
                                     empty);
            }
        }
        Ok(())
    }

    fn nested(&mut self) -> Result<(), Status>
    {
        macro_rules! span {
            ($ty:ty, $field:ident, $length:ident, $element:ty, $output:expr) => {{
                let length =
                    unsafe { (*self.owned_mut(0)?.pointer().cast::<$ty>()).$length } as usize;
                let bytes = length.checked_mul(size_of::<$element>())
                                  .filter(|x| *x <= isize::MAX as usize)
                                  .ok_or(BAD_ARGUMENT)?;
                self.point(0, offset_of!($ty, $field), bytes, align_of::<$element>(), $output)?;
            }};
        }
        macro_rules! surface {
            ($ty:ty) => {
                self.surface(offset_of!($ty, surface))?
            };
        }
        let number = native_string_trap(self.number).unwrap_or(self.number);
        match number
        {
            DEFINITION => self.definition_nested()?,
            USER_CALLBACK =>
            {
                let operation = unsafe {
                    (*self.owned_mut(0)?.pointer().cast::<abi::definitions::CallbackCall>()).operation
                };
                if !matches!(operation,
                             abi::definitions::CALLBACK_NEXT | abi::definitions::CALLBACK_RETURN)
                {
                    return Err(BAD_ARGUMENT);
                }
                let bytes = unsafe {
                    (*self.owned_mut(0)?.pointer().cast::<abi::definitions::CallbackCall>()).length
                };
                if bytes > isize::MAX as usize
                {
                    return Err(BAD_ARGUMENT);
                }
                let index = if operation == abi::definitions::CALLBACK_RETURN
                {
                    self.callback_return_buffer(bytes)?
                }
                else
                {
                    self.point(0,
                               offset_of!(abi::definitions::CallbackCall, buffer),
                               bytes,
                               align_of::<u8>(),
                               true)?
                };
                self.callback = Some(CallbackBuffer { operation,
                                                      bytes,
                                                      index });
            }
            LOG =>
            {
                span!(LogCall, bytes, length, u8, false);
            }
            CRASH =>
            {
                span!(abi::crashes::CrashCall, data, length, u8, false);
                span!(abi::crashes::CrashCall, output, capacity, u8, true);
            }
            ENTROPY =>
            {
                span!(abi::entropy::Call, output, capacity, u8, true);
            }
            PIPES =>
            {
                let call = unsafe { &*self.owned_mut(0)?.pointer().cast::<abi::pipes::Call>() };
                if call.length > isize::MAX as usize
                {
                    return Err(BAD_ARGUMENT);
                }
                // READ writes into the caller's buffer; WRITE only reads it.
                let writable = call.operation == abi::pipes::READ;
                self.point(0,
                           offset_of!(abi::pipes::Call, buffer),
                           call.length,
                           align_of::<u8>(),
                           writable)?;
            }
            NATIVE_THREAD =>
            {
                span!(abi::native_threads::Call, name, name_length, u8, false);
            }
            CLASSIC =>
            {
                let length =
                    unsafe { (*self.owned_mut(0)?.pointer().cast::<ClassicCall>()).memory_length };
                if length > 128 * 1024 * 1024
                {
                    return Err(BAD_ARGUMENT);
                }
                span!(ClassicCall, memory, memory_length, u8, true);
            }
            SURFACE =>
            {
                span!(SurfaceCall, text, text_length, u8, false);
                surface!(SurfaceCall);
            }
            QUICKDRAW =>
            {
                span!(abi::quickdraw::GraphicsCall,
                      points,
                      point_count,
                      abi::geometry::Point,
                      false);
                span!(abi::quickdraw::GraphicsCall, region, region_length, u8, false);
                surface!(abi::quickdraw::GraphicsCall);
            }
            FONT if self.size == size_of::<abi::quickdraw::FontSuitcaseCall>() =>
            {
                span!(abi::quickdraw::FontSuitcaseCall, data, data_length, u8, false);
                span!(abi::quickdraw::FontSuitcaseCall, resource, resource_length, u8, false);
            }
            FONT =>
            {
                span!(abi::quickdraw::FontCall, text, text_length, u8, false);
                surface!(abi::quickdraw::FontCall);
            }
            REGION =>
            {
                span!(abi::quickdraw::RegionCall, a, a_length, u8, false);
                span!(abi::quickdraw::RegionCall, b, b_length, u8, false);
                span!(abi::quickdraw::RegionCall, points, point_count, abi::geometry::Point, false);
                span!(abi::quickdraw::RegionCall, output, capacity, u8, true);
            }
            COPY_BITS =>
            {
                span!(abi::quickdraw::BitmapCall, data, data_length, u8, false);
                span!(abi::quickdraw::BitmapCall, palette, palette_length, u32, false);
                span!(abi::quickdraw::BitmapCall, mask, mask_length, u8, false);
                surface!(abi::quickdraw::BitmapCall);
            }
            WORLD =>
            {
                let write = unsafe {
                    (*self.owned_mut(0)?.pointer().cast::<abi::quickdraw::WorldCall>()).operation
                } == abi::quickdraw::WORLD_WRITE;
                span!(abi::quickdraw::WorldCall, pixels, pixel_length, u32, !write);
            }
            CURSOR if self.size == size_of::<abi::quickdraw::CursorCall>() =>
            {
                span!(abi::quickdraw::CursorCall, colors, color_count, u32, false);
            }
            MAPPING =>
            {
                span!(abi::quickdraw::MappingCall, points, point_count, abi::geometry::Point, true);
            }
            PICTURE =>
            {
                span!(abi::quickdraw::PictureCall, data, length, u8, false);
                surface!(abi::quickdraw::PictureCall);
            }
            ICON if self.size == size_of::<abi::quickdraw::IconDataCall>() =>
            {
                span!(abi::quickdraw::IconDataCall, data, length, u8, false);
                surface!(abi::quickdraw::IconDataCall);
            }
            ICON if self.size == size_of::<abi::quickdraw::FileIconCall>() =>
            {
                surface!(abi::quickdraw::FileIconCall);
            }
            ICON if self.size == size_of::<abi::quickdraw::DefaultFileIconCall>() =>
            {
                surface!(abi::quickdraw::DefaultFileIconCall);
            }
            ICON =>
            {
                surface!(abi::quickdraw::IconCall);
            }
            IMAGE =>
            {
                span!(abi::images::ImageCall, data, length, u8, false);
                surface!(abi::images::ImageCall);
            }
            RESOURCE if self.size == size_of::<abi::resources::ResourceCall>() =>
            {
                span!(abi::resources::ResourceCall, name, name_length, u8, false);
                span!(abi::resources::ResourceCall, output, capacity, u8, true);
            }
            RESOURCE if self.size == size_of::<abi::resources::FileResourceCall>() =>
            {
                span!(abi::resources::FileResourceCall, name, name_length, u8, false);
                span!(abi::resources::FileResourceCall, output, capacity, u8, true);
            }
            MEMORY =>
            {
                span!(abi::memory::MemoryCall, input, input_length, u8, false);
                span!(abi::memory::MemoryCall, output, capacity, u8, true);
            }
            SCRAP =>
            {
                span!(abi::scrap::ScrapCall, input, input_length, u8, false);
                span!(abi::scrap::ScrapCall, output, capacity, u8, true);
            }
            DRAG =>
            {
                span!(abi::drag::Call, input, length, u8, false);
                span!(abi::drag::Call, output, capacity, u8, true);
            }
            TEXTEDIT if self.size == size_of::<abi::textedit::TextEditCall>() =>
            {
                span!(abi::textedit::TextEditCall, input, input_length, u8, false);
                span!(abi::textedit::TextEditCall, output, capacity, u8, true);
            }
            CONTROL =>
            {
                surface!(abi::controls::ControlCall);
            }
            SCROLL_BOX =>
            {
                surface!(abi::scrollbox::Call);
            }
            FILE if self.size == size_of::<abi::files::FileCall>() =>
            {
                span!(abi::files::FileCall, name, name_length, u8, false);
                span!(abi::files::FileCall, input, input_length, u8, false);
                span!(abi::files::FileCall, output, capacity, u8, true);
            }
            FILE if self.size == size_of::<abi::files::DataWriteCall>() =>
            {
                span!(abi::files::DataWriteCall, name, name_length, u8, false);
                span!(abi::files::DataWriteCall, data, data_length, u8, false);
            }
            FILE if self.size == size_of::<abi::files::ForkWriteCall>() =>
            {
                span!(abi::files::ForkWriteCall, name, name_length, u8, false);
                span!(abi::files::ForkWriteCall, comment, comment_length, u8, false);
                span!(abi::files::ForkWriteCall, data, count, u8, false);
                span!(abi::files::ForkWriteCall, output, capacity, u8, true);
            }
            FILE if self.size == size_of::<abi::files::FileTransferCall>() =>
            {
                span!(abi::files::FileTransferCall,
                      sources,
                      source_count,
                      abi::files::FileTransferSource,
                      false);
            }
            FILE if self.size == size_of::<abi::files::IconMoveCall>() =>
            {
                span!(abi::files::IconMoveCall, entries, count, abi::files::IconMove, false);
            }
            FILE if self.size == size_of::<abi::files::IconGroupCall>() =>
            {
                span!(abi::files::IconGroupCall, entries, count, abi::files::IconMove, false);
            }
            _ =>
            {}
        }
        Ok(())
    }
}

fn layout(number: u32, bytes: usize) -> Result<Layout, Status>
{
    // These numbers are trusted module/worker gates. In particular, never
    // decode a user-supplied Rust fn pointer or invoke it with System privilege.
    let number = native_string_trap(number).unwrap_or(number);
    macro_rules! select {
        ($($ty:ty),+ $(,)?) => { {
            $(if bytes == size_of::<$ty>() { return Ok(Layout::of::<$ty>()); })+
            return Err(BAD_ARGUMENT);
        }};
    }
    match number
    {
        USER_CALLBACK => select!(abi::definitions::CallbackCall),
        DEFINITION => select!(abi::definitions::UserCall),
        DEFINITION_PANIC => select!(abi::definitions::DefinitionPanic),
        LOG => select!(LogCall),
        WINDOW => select!(WindowCall),
        DRAW => select!(DrawCall),
        NEXT_EVENT => select!(NextEventCall),
        WAIT_EVENT => select!(WaitEventCall),
        POSITIONED_EVENT => select!(abi::events::PositionedEventCall),
        CLASSIC => select!(ClassicCall),
        GEOMETRY => select!(GeometryCall),
        SURFACE => select!(SurfaceCall),
        QUICKDRAW => select!(abi::quickdraw::GraphicsCall),
        FONT => select!(abi::quickdraw::FontSuitcaseCall, abi::quickdraw::FontCall),
        REGION => select!(abi::quickdraw::RegionCall),
        COPY_BITS => select!(abi::quickdraw::BitmapCall),
        WORLD => select!(abi::quickdraw::WorldCall),
        CURSOR => select!(abi::quickdraw::CursorResourceCall, abi::quickdraw::CursorCall),
        MAPPING => select!(abi::quickdraw::MappingCall),
        PICTURE => select!(abi::quickdraw::PictureCall),
        ICON =>
        {
            select!(abi::quickdraw::IconDataCall,
                    abi::quickdraw::DefaultFileIconCall,
                    abi::quickdraw::FileIconCall,
                    abi::quickdraw::IconCall)
        }
        IMAGE => select!(abi::images::ImageCall),
        RESOURCE =>
        {
            select!(abi::resources::FileResourceCall,
                    abi::resources::FileVersionCall,
                    abi::resources::ResourceCall)
        }
        FILE =>
        {
            select!(abi::files::FileCall,
                    abi::files::DataWriteCall,
                    abi::files::FileScanCall,
                    abi::files::ForkWriteCall,
                    abi::files::IconMoveCall,
                    abi::files::IconGroupCall,
                    abi::files::FileTransferCall)
        }
        TEXTEDIT =>
        {
            select!(abi::textedit::TextEditDisplayCall,
                    abi::textedit::TextEditFontCall,
                    abi::textedit::TextEditCall)
        }
        MEMORY => select!(abi::memory::MemoryCall),
        SCRAP => select!(abi::scrap::ScrapCall),
        DRAG => select!(abi::drag::Call),
        CONTROL => select!(abi::controls::ControlCall),
        SCROLL_BOX => select!(abi::scrollbox::Call),
        EVENTS => select!(abi::events::EventCall),
        CLOCK => select!(abi::dates::ClockCall),
        MONOTONIC_CLOCK => select!(abi::events::MonotonicCall),
        STANDARD_FILE => select!(abi::standard_file::StandardFileCall),
        DIALOG => select!(abi::dialogs::DialogCall),
        MENU => select!(abi::menus::MenuCall),
        PROCESS =>
        {
            select!(abi::processes::ProcessCall,
                    abi::processes::DocumentCall,
                    abi::processes::NativeExitCall)
        }
        APPLICATION_HEAP => select!(abi::application_heap::Call),
        NATIVE_THREAD => select!(abi::native_threads::Call),
        ADDRESS_WAIT => select!(abi::address_wait::Call),
        PIPES => select!(abi::pipes::Call),
        TLS => select!(abi::tls::Call),
        ENTROPY => select!(abi::entropy::Call),
        POWER => select!(abi::power::PowerCall),
        CRASH => select!(abi::crashes::CrashCall),
        INSPECTION => select!(abi::inspection::Call, abi::inspection::memory::Call),
        CPU_INFO => select!(abi::inspection::cpus::Call),
        THEME => select!(abi::theme::Palette),
        _ => Err(NOT_SUPPORTED)
    }
}

impl Drop for Call
{
    fn drop(&mut self)
    {
        if let Some(origin) = &self.surface_return
        {
            origin.cancel();
        }

        let Some(mut loan) = self.stock.take()
        else
        {
            return;
        };
        let mut buffers = core::mem::take(&mut self.buffers);
        let mut record = Vec::new();
        let mut encoded = Vec::new();
        let mut readers = None;
        if !buffers.is_empty()
        {
            let first = buffers.remove(0);
            if let Some(mut first) = first
            {
                let words = core::mem::replace(&mut first.words, BufferWords::Ordinary(Vec::new()));
                if let BufferWords::Ordinary(words) = words
                {
                    record = words;
                }
                encoded = first.encoded.take().unwrap_or_default();
                readers = first.mapping.take().map(Range::release_storage);
                drop(first);
            }
        }
        // Range/ReturnLoan and all semantic owners die before the stock gate.
        buffers.clear();
        drop(self.offscreen.take());
        drop(self.continuation.take());
        drop(self.delivery.take());
        drop(self.direct.take());
        drop(self.memory.take());
        let fixups = core::mem::take(&mut self.fixups);
        loan.restore(record, encoded, buffers, fixups, readers);
    }
}

//! Affine custody of an elected User draw's ACTUAL checked Surface buffers.
//! No raw descriptor/pixel pointer or user reference crosses a wait.
#![forbid(unsafe_op_in_unsafe_fn)]
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{mem::{offset_of, size_of},
           sync::atomic::{AtomicBool, Ordering}};

use mac_abi::{controls,
              geometry::{Point, Rect},
              scrollbox, *};
use mac_object_table::{domain::{DomainId, DomainKind},
                       shared::InvocationGate as _};
use spinning_top::Spinlock;

use super::{Buffer, BufferWords, Call};
use crate::{context::ThreadId,
            machine::scheduler::{accounting::AllocationAuthority, service::RequestIdentity},
            toolbox::{handles::InvocationGate, quickdraw::Canvas,
                      resources::definitions::callbacks::Notice}};

#[derive(Clone, Copy)]
pub(super) struct Capture
{
    pub(super) field:  usize,
    pub(super) header: usize,
    pub(super) pixels: Option<usize>
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Geometry
{
    pub(crate) width:  usize,
    pub(crate) height: usize,
    pub(crate) origin: Point,
    pub(crate) clip:   Rect
}
struct Metadata
{
    request:  RequestIdentity,
    thread:   ThreadId,
    number:   u32,
    owner:    DomainId,
    capture:  Capture,
    geometry: Geometry
}
/// The node owns original Buffer/Range values, never replicas or a fake empty
/// buffer. A refused duplicate return is retained intrusively without allocating.
struct Storage
{
    header: Buffer,
    pixels: Buffer,
    next:   Option<Box<Storage>>
}
struct State
{
    returned: Option<Box<Storage>>,
    loaned:   bool,
    failed:   bool,
    notice:   Option<Arc<Notice>>
}
struct Cell
{
    meta:   Metadata,
    closed: AtomicBool,
    state:  Spinlock<State>
}
/// Private actual call instance identity, not a numeric issuer or Window/CTRL.
#[derive(Clone)]
pub(crate) struct Origin(Arc<Cell>);
impl Origin
{
    pub(crate) fn same(&self, other: &Self) -> bool
    {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub(crate) fn owner(&self) -> DomainId
    {
        self.0.meta.owner
    }

    pub(crate) fn thread(&self) -> ThreadId
    {
        self.0.meta.thread
    }

    pub(crate) fn geometry(&self) -> Geometry
    {
        self.0.meta.geometry
    }

    pub(crate) fn cancelled(&self) -> bool
    {
        self.0.closed.load(Ordering::Acquire)
    }

    pub(crate) fn observes(&self, notice: &Arc<Notice>) -> bool
    {
        InvocationGate::run(|| {
            self.0.state.lock().notice.as_ref().is_some_and(|current| Arc::ptr_eq(current, notice))
        })
    }

    pub(crate) fn observe(&self, notice: Arc<Notice>) -> Result<(), Status>
    {
        InvocationGate::run(|| {
            let mut state = self.0.state.lock();
            if self.cancelled() || state.failed
            {
                return Err(OPERATION_CANCELLED);
            }
            if state.notice.as_ref().is_some_and(|current| !Arc::ptr_eq(current, &notice))
            {
                return Err(ACCESS_DENIED);
            }
            if state.notice.is_none()
            {
                state.notice = Some(notice);
            }
            Ok(())
        })
    }

    pub(crate) fn cancel(&self)
    {
        if !self.0.closed.swap(true, Ordering::AcqRel)
        {
            InvocationGate::run(|| {
                if let Some(notice) = &self.0.state.lock().notice
                {
                    notice.request();
                }
            });
        }
    }
}
#[must_use = "return the SAME checked Surface before final copyout"]
pub(crate) struct Loan
{
    origin:     Origin,
    allocation: AllocationAuthority,
    storage:    Option<Box<Storage>>
}
impl Loan
{
    pub(crate) fn origin(&self) -> Origin
    {
        self.origin.clone()
    }

    pub(crate) fn geometry(&self) -> Geometry
    {
        self.origin.geometry()
    }

    /// Actual full requested Surface staging, paid by the original account.
    /// No output buffer is changed until whole ordered drawing has completed.
    pub(crate) fn prepare_staging(&mut self) -> Result<Staging, Status>
    {
        let target =
            crate::allocator::Target::Owner { account: self.allocation.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        crate::allocator::with_target(target, || self.prepare_staging_owned())
    }

    fn prepare_staging_owned(&mut self) -> Result<Staging, Status>
    {
        if self.origin.cancelled()
        {
            return Err(OPERATION_CANCELLED);
        }
        let geometry = self.geometry();
        let count = geometry.width.checked_mul(geometry.height).ok_or(BAD_ARGUMENT)?;
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(count).map_err(|_| NO_MEMORY)?;
        self.with_original(|canvas| pixels.extend_from_slice(canvas.pixels))?;
        Ok(Staging { origin: self.origin.clone(),
                     allocation: self.allocation.clone(),
                     geometry,
                     pixels })
    }

    pub(crate) fn commit(&mut self, staging: &Staging) -> Result<(), Status>
    {
        if !self.origin.same(&staging.origin) || self.geometry() != staging.geometry
        {
            return Err(ACCESS_DENIED);
        }
        if self.origin.cancelled()
        {
            return Err(OPERATION_CANCELLED);
        }
        let expected =
            self.geometry().width.checked_mul(self.geometry().height).ok_or(BAD_ARGUMENT)?;
        if staging.pixels.len() != expected
        {
            return Err(BAD_ABI);
        }
        self.with_original(|canvas| canvas.pixels.copy_from_slice(&staging.pixels))
    }

    fn with_original<R>(&mut self, run: impl FnOnce(&mut Canvas<'_>) -> R) -> Result<R, Status>
    {
        let storage = self.storage.as_mut().ok_or(BAD_ABI)?;
        with_buffers(&storage.header, &mut storage.pixels, |canvas| {
            let actual = Geometry { width:  canvas.width,
                                    height: canvas.height,
                                    origin: canvas.origin(),
                                    clip:   canvas.clip() };
            if actual != self.origin.geometry()
            {
                return Err(SERVICE_BUSY);
            }
            Ok(run(canvas))
        })?
    }
}
impl Drop for Loan
{
    fn drop(&mut self)
    {
        let Some(mut storage) = self.storage.take()
        else
        {
            return;
        };
        InvocationGate::run(|| {
            let mut state = self.origin.0.state.lock();
            if !state.loaned || state.returned.is_some()
            {
                state.failed = true;
            }
            // Preserve all genuine nodes even on a duplicate/malformed return.
            // No allocation, semantic buffer Drop or copyout under this gate.
            storage.next = state.returned.take();
            state.returned = Some(storage);
            state.loaned = false;
        });
    }
}
pub(crate) struct Staging
{
    origin:     Origin,
    allocation: AllocationAuthority,
    geometry:   Geometry,
    pixels:     Vec<u32>
}
impl Staging
{
    pub(crate) fn with_canvas<R>(&mut self, run: impl FnOnce(&mut Canvas<'_>) -> R)
                                 -> Result<R, Status>
    {
        let target =
            crate::allocator::Target::Owner { account: self.allocation.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        crate::allocator::with_target(target, || self.with_canvas_owned(run))
    }

    fn with_canvas_owned<R>(&mut self, run: impl FnOnce(&mut Canvas<'_>) -> R)
                            -> Result<R, Status>
    {
        if self.origin.cancelled()
        {
            return Err(OPERATION_CANCELLED);
        }
        let g = self.geometry;
        let mut canvas = Canvas::new(&mut self.pixels, g.width, g.height);
        // Geometry is captured from the real normalized Canvas, in physical
        // coordinates; preserve it without offsetting the clip a second time.
        canvas.set_clip(g.clip);
        canvas.set_origin(g.origin.h, g.origin.v);
        Ok(run(&mut canvas))
    }
}
fn with_buffers<R>(header: &Buffer, pixels: &mut Buffer, run: impl FnOnce(&mut Canvas<'_>) -> R)
                   -> Result<R, Status>
{
    if header.bytes != size_of::<Surface>()
       || !header.output
       || !pixels.output
       || header.mapping.is_none()
       || pixels.mapping.is_none()
    {
        return Err(BAD_ABI);
    }
    let surface = super::wire::decode_value::<Surface>(header.input())?;
    let count =
        (surface.width as usize).checked_mul(surface.height as usize).ok_or(BAD_ARGUMENT)?;
    let bytes = count.checked_mul(size_of::<u32>()).ok_or(BAD_ARGUMENT)?;
    if surface.width == 0
       || surface.height == 0
       || surface.width > i16::MAX as u32
       || surface.height > i16::MAX as u32
       || count > crate::display::pixels()
       || pixels.bytes != bytes
       || pixels.words.len().checked_mul(8).is_none_or(|n| n < bytes)
       || !matches!(header.words, BufferWords::Ordinary(_))
       || !matches!(pixels.words, BufferWords::Ordinary(_))
    {
        return Err(BAD_ARGUMENT);
    }
    let pointer = pixels.pointer().cast::<u32>();
    if surface.pixels != pointer
    {
        return Err(BAD_ABI);
    }
    // Sole audited borrow: the original initialized u64 allocation has >=u32
    // alignment, checked length and exclusive ownership. No Vec-layout cast.
    let data = unsafe { core::slice::from_raw_parts_mut(pointer, count) };
    let mut canvas = Canvas::new(data, surface.width as usize, surface.height as usize);
    canvas.set_clip(surface.clip);
    canvas.set_origin(surface.origin.h, surface.origin.v);
    Ok(run(&mut canvas))
}
impl Call
{
    /// Only after genuine User-source election. Ordinary calls have no cell,
    /// identity, metadata node or scope allocation added by this mechanism.
    pub(crate) fn checkout_surface(&mut self, allocation: &AllocationAuthority)
                                   -> Result<Loan, Status>
    {
        if self.surface_return.is_some()
        {
            return Err(SERVICE_BUSY);
        }
        if self.thread != crate::context::current_thread_context().identity().thread
        {
            return Err(ACCESS_DENIED);
        }
        let owner = self.memory.as_ref().ok_or(BAD_ABI)?.owner();
        if owner.kind() != DomainKind::Process || crate::context::recovery_activity().is_some()
        {
            return Err(NOT_SUPPORTED);
        }
        if allocation.owner() != owner
        {
            return Err(ACCESS_DENIED);
        }
        let capture = self.surface_capture.ok_or(BAD_ARGUMENT)?;
        let expected_field = match self.number
        {
            mac_abi::traps::CONTROL if self.size == size_of::<controls::ControlCall>() =>
            {
                let call =
                    unsafe { &*self.owned_mut(0)?.pointer().cast::<controls::ControlCall>() };
                if !matches!(call.operation, controls::DRAW_STANDARD | controls::DRAW_SCROLLBAR)
                {
                    return Err(BAD_ARGUMENT);
                }
                offset_of!(controls::ControlCall, surface)
            }
            mac_abi::traps::SCROLL_BOX if self.size == size_of::<scrollbox::Call>() =>
            {
                let call = unsafe { &*self.owned_mut(0)?.pointer().cast::<scrollbox::Call>() };
                if call.operation != scrollbox::DRAW
                {
                    return Err(BAD_ARGUMENT);
                }
                offset_of!(scrollbox::Call, surface)
            }
            _ => return Err(BAD_ARGUMENT)
        };
        let pixel_index = capture.pixels.ok_or(BAD_ARGUMENT)?;
        if capture.field != expected_field
           || capture.header == 0
           || pixel_index <= capture.header
           || pixel_index >= self.buffers.len()
        {
            return Err(BAD_ABI);
        }
        let header_pointer = self.owned_mut(capture.header)?.pointer();
        let selected = unsafe {
            core::ptr::read_unaligned(self.owned_mut(0)?
                                          .pointer()
                                          .add(expected_field)
                                          .cast::<*mut u8>())
        };
        if selected != header_pointer
        {
            return Err(ACCESS_DENIED);
        }
        let geometry = {
            let (before, after) = self.buffers.split_at_mut(pixel_index);
            let header = before[capture.header].as_ref().ok_or(BAD_ABI)?;
            let pixels = after[0].as_mut().ok_or(BAD_ABI)?;
            with_buffers(header, pixels, |canvas| {
                Geometry { width:  canvas.width,
                           height: canvas.height,
                           origin: canvas.origin(),
                           clip:   canvas.clip() }
            })?
        };
        let target =
            crate::allocator::Target::Owner { account: allocation.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        let (cell, node) = crate::allocator::with_target(target, || {
            let node = Box::<Storage>::try_new_uninit().map_err(|_| NO_MEMORY)?;
            let cell = Arc::try_new(Cell { meta: Metadata { request: self.identity,
                thread: self.thread, number: self.number, owner, capture, geometry },
                closed: AtomicBool::new(false), state: Spinlock::new(State { returned: None,
                    loaned: true, failed: false, notice: None }) }).map_err(|_| NO_MEMORY)?;
            Ok::<_, Status>((cell, node))
        })?;
        let header = self.buffers[capture.header].take().ok_or(BAD_ABI)?;
        let pixels = match self.buffers[pixel_index].take()
        {
            Some(pixels) => pixels,
            None =>
            {
                self.buffers[capture.header] = Some(header);
                return Err(BAD_ABI);
            }
        };
        let node = Box::write(node,
                              Storage { header,
                                        pixels,
                                        next: None });
        let origin = Origin(cell);
        self.surface_return = Some(origin.clone());
        Ok(Loan { origin,
                  allocation: allocation.clone(),
                  storage: Some(node) })
    }

    pub(super) fn restore_surface(&mut self) -> Result<(), Status>
    {
        let Some(origin) = &self.surface_return
        else
        {
            return Ok(());
        };
        let meta = &origin.0.meta;
        let pixels = meta.capture.pixels.ok_or(BAD_ABI)?;
        if meta.request != self.identity
           || meta.thread != self.thread
           || meta.number != self.number
           || meta.capture.header == 0
           || pixels <= meta.capture.header
           || pixels >= self.buffers.len()
           || self.buffers[meta.capture.header].is_some()
           || self.buffers[pixels].is_some()
        {
            return Err(BAD_ABI);
        }
        let node = InvocationGate::run(|| {
            let mut state = origin.0.state.lock();
            if state.loaned
            {
                return Err(SERVICE_BUSY);
            }
            if state.failed || state.returned.as_ref().is_none_or(|node| node.next.is_some())
            {
                return Err(BAD_ABI);
            }
            state.returned.take().ok_or(BAD_ABI)
        })?;
        let Storage { header,
                      pixels: data,
                      next: _ } = *node;
        self.buffers[meta.capture.header] = Some(header);
        self.buffers[pixels] = Some(data);
        origin.cancel();
        Ok(())
    }
}

impl Call
{
    /// Checked-envelope election before generic resident dispatch. No raw
    /// ControlCall/Surface reference crosses a wait, including Desktop refusal.
    pub(super) fn dispatch_surface(&mut self, suspension: Result<(), Status>)
                                   -> Option<crate::machine::scheduler::service::RequestResult>
    {
        use crate::{machine::scheduler::service::{RequestAction, RequestResult},
                    toolbox::{controls::offscreen::Input, runtime::WindowBeginError}};
        if self.offscreen.is_none()
        {
            let input = match self.number
            {
                mac_abi::traps::CONTROL if self.size == size_of::<controls::ControlCall>() =>
                {
                    let call = match self.owned(0)
                    {
                        Ok(buffer) =>
                        unsafe {
                            &*buffer.words.as_ptr().cast::<controls::ControlCall>()
                        },
                        Err(s) => return Some(RequestResult::done(s))
                    };
                    if !matches!(call.operation, controls::DRAW_STANDARD | controls::DRAW_SCROLLBAR)
                       || call.surface.is_null()
                    {
                        return None;
                    }
                    Input::control(call)
                }
                mac_abi::traps::SCROLL_BOX if self.size == size_of::<scrollbox::Call>() =>
                {
                    let call = match self.owned(0)
                    {
                        Ok(buffer) =>
                        unsafe { &*buffer.words.as_ptr().cast::<scrollbox::Call>() },
                        Err(s) => return Some(RequestResult::done(s))
                    };
                    if call.operation != scrollbox::DRAW || call.surface.is_null()
                    {
                        return None;
                    }
                    Input::scroll_box(call)
                }
                _ => return None
            };
            let input = match input
            {
                Ok(input) => input,
                Err(s) => return Some(RequestResult::done(s))
            };
            let admission = match crate::toolbox::runtime::admit_offscreen(input)
            {
                Ok(Some(admission)) => admission,
                Ok(None) => return None,
                Err(WindowBeginError::Desktop { cause: crate::state::AccessError::Busy,
                                                epoch }) =>
                {
                    return Some(match crate::machine::scheduler::callbacks::wait_desktop(epoch)
                                {
                                    Ok(_) =>
                                    {
                                        RequestResult { status: OK,
                                                        action: RequestAction::Replay }
                                    }
                                    Err(s) => RequestResult::done(s)
                                });
                }
                Err(cause) => return Some(RequestResult::done(cause.status()))
            };
            if let Err(status) = suspension
            {
                return Some(RequestResult::done(status));
            }
            let loan = match self.checkout_surface(admission.allocation())
            {
                Ok(loan) => loan,
                Err(s) => return Some(RequestResult::done(s))
            };
            match admission.start(loan)
            {
                Ok(operation) => self.offscreen = Some(operation),
                Err(s) => return Some(RequestResult::done(s))
            }
        }
        let mut operation = match self.offscreen.take()
        {
            Some(operation) => operation,
            None => return Some(RequestResult::done(BAD_ABI))
        };
        match operation.advance()
        {
            Ok(Some(status)) =>
            {
                let output =
                    if status != OK
                    {
                        Ok(())
                    }
                    else
                    {
                        match self.number {
                        mac_abi::traps::CONTROL=>self.owned_mut(0).and_then(|buffer| {
                            let root=unsafe{&mut *buffer.pointer().cast::<controls::ControlCall>()};
                            let mut next=*root;
                            operation.write_control(&mut next)?;
                            operation.commit()?;
                            *root=next;
                            Ok(())
                        }),
                        mac_abi::traps::SCROLL_BOX=>self.owned_mut(0).and_then(|buffer| {
                            let root=unsafe{&mut *buffer.pointer().cast::<scrollbox::Call>()};
                            let mut next=*root;
                            operation.write_scroll_box(&mut next)?;
                            operation.commit()?;
                            *root=next;
                            Ok(())
                        }),
                        _=>Err(BAD_ABI)
                    }
                    };
                // Restores the original loan before final fixups/copyout.
                drop(operation);
                Some(RequestResult::done(output.map_or_else(|s| s, |()| status)))
            }
            Ok(None) =>
            {
                let waited = operation.wait();
                if waited.is_ok()
                {
                    self.offscreen = Some(operation);
                }
                else
                {
                    drop(operation);
                }
                Some(match waited
                     {
                         Ok(()) =>
                         {
                             RequestResult { status: OK,
                                             action: RequestAction::Replay }
                         }
                         Err(s) => RequestResult::done(s)
                     })
            }
            Err(status) =>
            {
                drop(operation);
                Some(RequestResult::done(status))
            }
        }
    }
}

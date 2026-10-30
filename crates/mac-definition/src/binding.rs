//! The single audited pointer/message boundary for control definitions.
use core::mem::align_of;

use mac_abi::{controls::{CHANGE_BOUNDS_DEFINITION, ControlChange, VALIDATE_DEFINITION},
              definitions::CDEF,
              *};

use crate::{Error, Message, Result, Services, SurfacePort, families::*};
struct Clipped<'a>
{
    surface:  &'a mut Surface,
    previous: Rect
}
impl Drop for Clipped<'_>
{
    fn drop(&mut self)
    {
        self.surface.clip = self.previous;
    }
}
#[derive(Clone, Copy)]
enum Operation
{
    Draw,
    Hit,
    Calculate,
    Track,
    Initialize,
    Dispose,
    Validate,
    Change
}
fn operation<F: Family>(wire: i16) -> Result<Operation>
{
    match wire
    {
        0 => Ok(Operation::Draw),
        1 => Ok(Operation::Hit),
        2 => Ok(if F::TRACK { Operation::Track } else { Operation::Calculate }),
        3 => Ok(Operation::Initialize),
        4 => Ok(Operation::Dispose),
        VALIDATE_DEFINITION => Ok(Operation::Validate),
        CHANGE_BOUNDS_DEFINITION if F::DELTA => Ok(Operation::Change),
        _ => Err(Error::NotSupported)
    }
}
fn encode(operation: Operation, reply: Reply) -> Result<i64>
{
    match (operation, reply)
    {
        (Operation::Hit, Reply::Hit(part)) => Ok(part as i64),
        (Operation::Track, Reply::Value(value)) if value <= i16::MAX as u16 => Ok(i64::from(value)),
        (Operation::Draw
         | Operation::Calculate
         | Operation::Initialize
         | Operation::Dispose
         | Operation::Validate
         | Operation::Change,
         Reply::Done) => Ok(0),
        _ => Err(Error::BadAbi)
    }
}
unsafe fn entry<F: Family>(pointer: *mut DefinitionCall,
                           handler: for<'a> fn(Message<'a, F::View<'a>>,
                                      Services<'a>)
                                      -> Result<Reply>)
                           -> i64
{
    let result = (|| -> Result<i64> {
        if pointer.is_null() || pointer.addr() % align_of::<DefinitionCall>() != 0
        {
            return Err(Error::BadArgument);
        }
        // SAFETY: host lends accessible exclusive call/state/pixels; shape
        // checks diagnose malformed input, not the future MMU/copy-user proof.
        let call = unsafe { &mut *pointer };
        if !call.header.supports::<DefinitionCall>()
        {
            return Err(Error::BadAbi);
        }
        if call.kind != CDEF || call.id != F::ID || call.reserved != 0
        {
            return Err(Error::NotSupported);
        }
        F::variation(call.variation)?;
        let reflow = crate::Reflow::try_from(call.reflow).map_err(|_| Error::BadArgument)?;
        let layout = (30..=42).contains(&call.message);
        if layout && reflow != crate::Reflow::Yes
        {
            return Err(Error::NotSupported);
        }
        let operation = if layout { Operation::Validate } else { operation::<F>(call.message)? };
        let services = SystemServices { header:       Header::for_type::<SystemServices>(),
                                        trap:         call.gate,
                                        capabilities: 0 };
        let services = Services::new(mac_traps::Client::new(&services)?, reflow);
        if call.object.is_null()
        {
            return Err(Error::BadArgument);
        }
        if layout
        {
            let payload = call.param as usize as *mut mac_abi::controls::reflow::Payload;
            if !call.surface.is_null()
               || payload.is_null()
               || payload.addr() % align_of::<mac_abi::controls::reflow::Payload>() != 0
               || call.object.addr() % align_of::<F::Wire>() != 0
            {
                return Err(Error::BadArgument);
            }
            // SAFETY: the resident invocation lends separate, exclusive payload
            // and immutable family state for this call; neither borrow escapes.
            let payload = unsafe { &mut *payload };
            if payload.message != call.message
            {
                return Err(Error::BadAbi);
            }
            let request = crate::reflow::decode_leaf(payload).map_err(|error| {
                                                                 Error::from_status(error.status())
                                                             })?;
            let creation = payload.creation.try_into().map_err(|_| Error::BadArgument)?;
            let state = F::decode(unsafe { &*call.object.cast::<F::Wire>() }, call.variation)?;
            let reply = handler(Message::Reflow { state,
                                                  request,
                                                  creation },
                                services)?;
            return match reply
            {
                Reply::Measurement(measurement) if call.message == controls::reflow::ASK_SIZE =>
                {
                    payload.measurement = measurement.into();
                    Ok(0)
                }
                Reply::Done if call.message != controls::reflow::ASK_SIZE => Ok(0),
                _ => Err(Error::BadAbi)
            };
        }
        let reply = if matches!(operation, Operation::Change)
        {
            if call.object.addr() % align_of::<ControlChange>() != 0
            {
                return Err(Error::BadArgument);
            }
            let change = unsafe { &mut *call.object.cast::<ControlChange>() };
            let (before, after) = F::change(&change.before, &change.after, call.variation)?;
            handler(Message::ChangeBounds { before,
                                            after,
                                            damage: &mut change.damage },
                    services)?
        }
        else
        {
            if call.object.addr() % align_of::<F::Wire>() != 0
            {
                return Err(Error::BadArgument);
            }
            let state = F::decode(unsafe { &*call.object.cast::<F::Wire>() }, call.variation)?;
            let message = match operation
            {
                Operation::Draw =>
                {
                    if call.surface.is_null() || call.surface.addr() % align_of::<Surface>() != 0
                    {
                        return Err(Error::BadArgument);
                    }
                    let surface = unsafe { &mut *call.surface };
                    let previous = surface.clip;
                    let offset = |n: i16, d: i16| {
                        (i32::from(n) + i32::from(d)).clamp(i32::from(i16::MIN),
                                                            i32::from(i16::MAX))
                        as i16
                    };
                    let bounds = F::bounds(&state);
                    if reflow == crate::Reflow::Yes
                    {
                        crate::reflow::LayoutRect::try_from(bounds).map_err(|_| {
                                                                       Error::BadArgument
                                                                   })?;
                        if bounds.is_empty()
                        {
                            return Ok(0);
                        }
                    }
                    let bounds = Rect::new(offset(bounds.left, surface.origin.h),
                                           offset(bounds.top, surface.origin.v),
                                           offset(bounds.right, surface.origin.h),
                                           offset(bounds.bottom, surface.origin.v));
                    let clip = Rect::new(previous.left.max(bounds.left),
                                         previous.top.max(bounds.top),
                                         previous.right.min(bounds.right),
                                         previous.bottom.min(bounds.bottom));
                    let clipped = Clipped { surface,
                                            previous };
                    clipped.surface.clip = if clip.is_empty() { Rect::ZERO } else { clip };
                    let connection =
                        unsafe { mac_traps::SurfaceConnection::new(clipped.surface, call.gate) };
                    return encode(operation,
                                  handler(Message::Draw { state,
                                                          port: SurfacePort::new(connection) },
                                          services)?);
                }
                Operation::Hit =>
                {
                    Message::HitTest { state,
                                       point: Point { h: call.param as i16,
                                                      v: (call.param as u32 >> 16) as i16 } }
                }
                Operation::Track =>
                {
                    Message::Track { state,
                                     point: Point { h: call.param as i16,
                                                    v: (call.param as u64 >> 16) as i16 },
                                     anchor: Point { h: (call.param as u64 >> 32) as i16,
                                                     v: (call.param as u64 >> 48) as i16 } }
                }
                Operation::Calculate => Message::CalculateRegions(state),
                Operation::Initialize => Message::Initialize(state),
                Operation::Dispose => Message::Dispose(state),
                Operation::Validate => Message::Validate(state),
                Operation::Change => unreachable!("change decoded before family object borrow")
            };
            handler(message, services)?
        };
        encode(operation, reply)
    })();
    result.unwrap_or_else(|error| i64::from(error.status()))
}
// These exported entry adapters are the only unsafe author-facing boundary.
// The entry attribute owns their use; authors provide ordinary Rust handlers.
macro_rules! adapter {
    ($name:ident,$family:ty,$view:ty) => {
        /// # Safety
        /// Host guarantees accessible/exclusive live family state and pixels,
        /// a synchronous resident service gate, and pinned executable backing.
        /// Reflow calls additionally lend a separate exclusive Payload through
        /// param; it must not overlap the call or the family state.
        pub unsafe fn $name(pointer: *mut DefinitionCall,
                            handler: for<'a> fn(Message<'a, $view>, Services<'a>) -> Result<Reply>)
                            -> i64
        {
            unsafe { entry::<$family>(pointer, handler) }
        }
    };
}
adapter!(static_text_entry, StaticFamily, crate::StaticText<'a>);
adapter!(buttons_entry, ButtonFamily, crate::Button<'a>);
adapter!(progress_entry, ProgressFamily, crate::Progress);
adapter!(scrollbar_entry, ScrollbarFamily, crate::Scrollbar);
adapter!(table_entry, TableFamily, crate::Table);

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
        let operation = operation::<F>(call.message)?;
        let services = SystemServices { header:       Header::for_type::<SystemServices>(),
                                        trap:         call.gate,
                                        capabilities: 0 };
        let services = Services::new(mac_traps::Client::new(&services)?);
        if call.object.is_null()
        {
            return Err(Error::BadArgument);
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
// The declaration macros own their use; authors provide ordinary Rust handlers.
macro_rules! adapter {
    ($name:ident,$family:ty,$view:ty) => {
        /// # Safety
        /// Host guarantees accessible/exclusive live family state and pixels,
        /// a synchronous resident service gate, and pinned executable backing.
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

#[cfg(test)]
mod tests
{
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::{Button, Part, Progress, Scrollbar, StaticText};
    static ENTERED: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn gate(_: u32, _: *mut (), _: usize) -> i64
    {
        0
    }
    fn reply<S>(message: Message<'_, S>) -> Result<Reply>
    {
        ENTERED.fetch_add(1, Ordering::Relaxed);
        match message
        {
            Message::Validate(_)
            | Message::CalculateRegions(_)
            | Message::Initialize(_)
            | Message::Dispose(_)
            | Message::Draw { .. }
            | Message::ChangeBounds { .. } => Ok(Reply::Done),
            Message::HitTest { .. } => Ok(Reply::Hit(Part::None)),
            Message::Track { .. } => Ok(Reply::Value(17))
        }
    }
    fn static_author<'a>(m: Message<'a, StaticText<'a>>, _: Services<'a>) -> Result<Reply>
    {
        reply(m)
    }
    fn button_author<'a>(m: Message<'a, Button<'a>>, _: Services<'a>) -> Result<Reply>
    {
        reply(m)
    }
    fn progress_author<'a>(m: Message<'a, Progress>, _: Services<'a>) -> Result<Reply>
    {
        reply(m)
    }
    fn scroll_author<'a>(m: Message<'a, Scrollbar>, _: Services<'a>) -> Result<Reply>
    {
        reply(m)
    }
    #[test]
    fn every_family_rejects_malformed_calls_before_entering_safe_author_code()
    {
        ENTERED.store(0, Ordering::Relaxed);
        let mut standard =
            mac_abi::definitions::ControlDefinitionState::new(Rect::new(1, 2, 80, 20), "Café");
        standard.maximum = 0;
        let mut scroll = mac_abi::controls::ScrollbarState { bounds: Rect::new(0, 0, 16, 100),
                                                             maximum: 100,
                                                             active: 1,
                                                             ..Default::default() };
        let mut call = DefinitionCall { header: Header::for_type::<DefinitionCall>(),
                                        kind: CDEF,
                                        id: 0,
                                        variation: 0,
                                        message: VALIDATE_DEFINITION,
                                        reserved: 0,
                                        param: 0,
                                        object: core::ptr::from_mut(&mut standard).cast(),
                                        surface: core::ptr::null_mut(),
                                        gate };
        for id in [0, 1, mac_abi::controls::CDEF_STATIC_TEXT, mac_abi::controls::CDEF_PROGRESS_BAR]
        {
            call.header = Header::for_type::<DefinitionCall>();
            call.kind = CDEF;
            call.id = id;
            call.variation = 0;
            call.message = VALIDATE_DEFINITION;
            if id == mac_abi::controls::CDEF_PROGRESS_BAR
            {
                standard.title_length = 0;
            }
            else
            {
                standard.title_length = 5;
            }
            call.object = if id == 1
            {
                core::ptr::from_mut(&mut scroll).cast()
            }
            else
            {
                core::ptr::from_mut(&mut standard).cast()
            };
            let run = |call: &mut DefinitionCall| unsafe {
                match id
                {
                    0 => buttons_entry(call, button_author),
                    1 => scrollbar_entry(call, scroll_author),
                    mac_abi::controls::CDEF_STATIC_TEXT => static_text_entry(call, static_author),
                    mac_abi::controls::CDEF_PROGRESS_BAR => progress_entry(call, progress_author),
                    _ => unreachable!()
                }
            };
            assert_eq!(run(&mut call), 0);
            let count = ENTERED.load(Ordering::Relaxed);
            let object = call.object;
            call.message = 99;
            call.object = core::ptr::null_mut();
            assert_eq!(run(&mut call),
                       i64::from(NOT_SUPPORTED),
                       "unknown message must precede object access");
            call.message = VALIDATE_DEFINITION;
            assert_eq!(run(&mut call), i64::from(BAD_ARGUMENT));
            call.object = core::ptr::without_provenance_mut(1);
            assert_eq!(run(&mut call), i64::from(BAD_ARGUMENT));
            call.object = object;
            call.variation = 99;
            assert_eq!(run(&mut call), i64::from(NOT_SUPPORTED));
            call.variation = 0;
            call.header.version = 0;
            assert_eq!(run(&mut call), i64::from(BAD_ABI));
            assert_eq!(ENTERED.load(Ordering::Relaxed), count, "malformed call reached author");
        }
    }
    #[test]
    fn wrong_native_reply_kind_cannot_be_reinterpreted_as_success()
    {
        assert_eq!(encode(Operation::Validate, Reply::Hit(Part::Button)), Err(Error::BadAbi));
        assert_eq!(encode(Operation::Hit, Reply::Value(10)), Err(Error::BadAbi));
        assert_eq!(encode(Operation::Track, Reply::Value(u16::MAX)), Err(Error::BadAbi));
        assert_eq!(encode(Operation::Hit, Reply::Hit(Part::Inactive)), Ok(254));
        assert_eq!(encode(Operation::Track, Reply::Value(17)), Ok(17));
    }
}

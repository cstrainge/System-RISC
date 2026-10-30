//! Wire decoding is separate from the safe TLS manager. User callbacks never
//! run here; NEXT returns one actual-thread-owned callback receipt to CRT.
use core::fmt::Write;

use mac_abi::{native_threads as threads, tls as wire};

use super::*;
struct Detail<'a>
{
    bytes:  &'a mut [u8],
    length: usize
}
impl Write for Detail<'_>
{
    fn write_str(&mut self, text: &str) -> fmt::Result
    {
        let mut count = text.len().min(self.bytes.len() - self.length);
        while !text.is_char_boundary(count)
        {
            count -= 1;
        }
        self.bytes[self.length..self.length + count].copy_from_slice(&text.as_bytes()[..count]);
        self.length += count;
        Ok(())
    }
}
fn detail(cause: Error, bytes: &mut [u8], length: &mut u32) -> Status
{
    let status = cause.status();
    let mut out = Detail { bytes,
                           length: 0 };
    let _ = write!(out, "{cause}");
    *length = out.length as u32;
    status
}
pub(crate) fn current(call: &mut threads::Call) -> Status
{
    call.thread = 0;
    call.detail_length = 0;
    let result = crate::context::without_recovery(|| {
        if call.operation != threads::CURRENT || call.reserved != 0 || call.reserved2 != 0
        {
            return Err(Error::Invalid);
        }
        crate::context::with_application_executor(|executor| {
            executor.with_tls(|slots| {
                        if slots.owner() != executor.owner().owner()
                        {
                            return Err(Error::Authority);
                        }
                        Ok(executor.thread())
                    })?
        }).map_err(Error::Context)?
    });
    match result
    {
        Ok(thread) =>
        {
            call.thread = thread;
            OK
        }
        Err(cause) => detail(cause, &mut call.detail, &mut call.detail_length)
    }
}
pub(crate) fn request(call: &mut wire::Call) -> Status
{
    call.detail_length = 0;
    call.present = 0;
    call.round = 0;
    let result = crate::context::without_recovery(|| {
        if call.flags != 0 || call.reserved != [0; 2] || call.reserved2 != 0
        {
            return Err(Error::Invalid);
        }
        crate::context::with_application_executor(|executor| operation(executor,call)).map_err(Error::Context)?
    });
    match result
    {
        Ok(()) => OK,
        Err(cause) => detail(cause, &mut call.detail, &mut call.detail_length)
    }
}
fn operation(executor: &crate::context::ApplicationExecutor<'_>, call: &mut wire::Call)
             -> Result<(), Error>
{
    let owner = executor.owner();
    match call.operation
    {
        wire::CREATE if call.key == 0 && call.value == 0 && call.ticket == 0 =>
        {
            let root = executor.with_tls(|slots| {
                                   if slots.owner() != owner.owner() || slots.finished()
                                   {
                                       return Err(Error::Authority);
                                   }
                                   Ok(slots.root().clone())
                               })??;
            call.key = keys::create(owner, executor.image(), &root, call.destructor)?;
            Ok(())
        }
        wire::DELETE
            if call.key != 0 && call.value == 0 && call.destructor == 0 && call.ticket == 0 =>
        {
            keys::delete(owner, call.key)
        }
        wire::GET | wire::SET if call.key != 0 && call.destructor == 0 && call.ticket == 0 =>
        {
            let actual = key(owner, call.key)?;
            let result = executor.with_tls(|slots| {
                                     if call.operation == wire::GET
                                     {
                                         call.value = slots.get(&actual, call.key)?;
                                     }
                                     else
                                     {
                                         slots.set(&actual, call.key, call.value)?;
                                     }
                                     Ok(())
                                 })?;
            drop(actual);
            result
        }
        wire::BEGIN
            if call.key == 0 && call.value == 0 && call.destructor == 0 && call.ticket == 0 =>
        {
            executor.with_tls(|slots| slots.begin())?
        }
        wire::NEXT
            if call.key == 0 && call.value == 0 && call.destructor == 0 && call.ticket == 0 =>
        {
            executor.with_tls(|slots| slots.next(call))?
        }
        wire::ACK
            if call.key == 0 && call.value == 0 && call.destructor == 0 && call.ticket != 0 =>
        {
            let pin = executor.with_tls(|slots| slots.ack(call.ticket))??;
            drop(pin);
            // The final-reader hook schedules actual detached cleanup after
            // all context/manager guards end at canonical maintenance.
            Ok(())
        }
        wire::FINISH
            if call.key == 0 && call.value == 0 && call.destructor == 0 && call.ticket == 0 =>
        {
            executor.with_tls(|slots| slots.finish())?
        }
        _ => Err(Error::Invalid)
    }
}

//! Native thread wire decoding and action selection. No borrowed request or
//! executor crosses the returned scheduler action; callbacks run on child entry.
use core::fmt::{self, Write};

use mac_abi::{Status, native_threads as wire};

use crate::machine::scheduler::{application as resident,
                                service::{RequestAction, RequestIdentity, RequestResult}};
struct Detail<'a>
{
    bytes: &'a mut [u8],
    used:  usize
}
impl Write for Detail<'_>
{
    fn write_str(&mut self, text: &str) -> fmt::Result
    {
        let mut count = text.len().min(self.bytes.len() - self.used);
        while !text.is_char_boundary(count)
        {
            count -= 1;
        }
        self.bytes[self.used..self.used + count].copy_from_slice(&text.as_bytes()[..count]);
        self.used += count;
        Ok(())
    }
}
fn validate(call: &wire::Call) -> Result<(), resident::Error>
{
    if call.flags != 0 || call.reserved != [0; 2] || call.reserved2 != 0 || call.reserved3 != 0
    {
        return Err(resident::Error::Invalid("unknown flags or nonzero reserved fields"));
    }
    let base = call.entry == 0 && call.data == 0 && call.stack_bytes == 0;
    let duration = call.seconds == 0 && call.nanoseconds == 0;
    let name = call.name.is_null() && call.name_length == 0;
    match call.operation
    {
        wire::CREATE if call.entry != 0 && call.right == 0 && duration && name => Ok(()),
        wire::QUERY | wire::JOIN | wire::DETACH if call.right != 0 && base && duration && name =>
        {
            Ok(())
        }
        wire::CURRENT | wire::EXIT | wire::YIELD | wire::PARALLELISM
            if call.right == 0 && base && duration && name =>
        {
            Ok(())
        }
        wire::SLEEP if call.right == 0 && base && name => Ok(()),
        wire::NAME if call.right == 0 && base && duration => Ok(()),
        _ =>
        {
            Err(resident::Error::Invalid("operation/input combination is not a native thread \
                                          request"))
        }
    }
}
/// The architecture boundary supplies bounded, decoded name text only after
/// scalar request validation. This safe service never dereferences ABI pointers.
pub(crate) fn request<'a>(call: &mut wire::Call, identity: RequestIdentity,
                          suspension: Result<(), Status>,
                          read_name: impl FnOnce(*const u8, usize) -> Result<&'a str, resident::Error>)
                          -> RequestResult
{
    call.thread = 0;
    call.state = wire::PREPARING;
    call.outcome = wire::NORMAL_OUTCOME;
    call.parallelism = 0;
    call.detail_length = 0;
    let result = crate::context::without_recovery(|| {
        validate(call)?;
        if call.operation != wire::JOIN && resident::cancel_current()
        {
            return Err(resident::Error::Invalid("join replay changed the original operation"));
        }
        if matches!(call.operation, wire::JOIN | wire::EXIT | wire::YIELD | wire::SLEEP)
        {
            suspension.map_err(|status| {
                          resident::Error::Admission { status,
                                                       stage: "outer preemptible thread action" }
                      })?;
        }
        let text = if call.operation == wire::NAME
        {
            read_name(call.name, call.name_length as usize)?
        }
        else
        {
            ""
        };
        crate::context::with_application_executor(|executor| {
            match call.operation
            {
                wire::CURRENT =>
                {
                    executor.with_tls(|slots| {
                                if slots.owner() == executor.owner().owner()
                                {
                                    Ok(())
                                }
                                else
                                {
                                    Err(super::Error::Authority)
                                }
                            })
                            .map_err(resident::Error::Tls)?
                            .map_err(resident::Error::Tls)?;
                    call.thread = executor.thread();
                    Ok(RequestAction::Return)
                }
                wire::CREATE =>
                {
                    resident::create(executor, call)?;
                    Ok(RequestAction::Return)
                }
                wire::QUERY =>
                {
                    resident::query(executor, call)?;
                    Ok(RequestAction::Return)
                }
                wire::JOIN =>
                {
                    if let Some(thread) = resident::join(executor, call.right, identity)?
                    {
                        call.thread = thread;
                        call.state = wire::COMPLETE;
                        Ok(RequestAction::Return)
                    }
                    else
                    {
                        Ok(RequestAction::Replay)
                    }
                }
                wire::DETACH =>
                {
                    resident::detach(executor, call.right)?;
                    Ok(RequestAction::Return)
                }
                wire::EXIT =>
                {
                    resident::exit(executor)?;
                    Ok(RequestAction::Exit)
                }
                wire::YIELD => Ok(RequestAction::Yield),
                wire::SLEEP =>
                {
                    resident::sleep(executor, call.seconds, call.nanoseconds)?;
                    Ok(if call.seconds == 0 && call.nanoseconds == 0
                       {
                           RequestAction::Return
                       }
                       else
                       {
                           RequestAction::Yield
                       })
                }
                wire::NAME =>
                {
                    resident::name(executor, text)?;
                    Ok(RequestAction::Return)
                }
                wire::PARALLELISM =>
                {
                    call.parallelism = crate::machine::eligible_harts().max(1) as u32;
                    Ok(RequestAction::Return)
                }
                _ => Err(resident::Error::Invalid("unsupported native thread operation"))
            }
        }).map_err(resident::Error::Context)?
    });
    match result
    {
        Ok(action) =>
        {
            RequestResult { status: mac_abi::OK,
                            action }
        }
        Err(cause) =>
        {
            resident::cancel_current();
            // Failure publishes no child ownership. Original join right inputs
            // remain unchanged for a typed caller disposal/retry decision.
            if call.operation == wire::CREATE
            {
                call.thread = 0;
                call.right = 0;
            }
            let status = cause.status();
            let mut detail = Detail { bytes: &mut call.detail,
                                      used:  0 };
            let _ = write!(detail, "{cause}");
            call.detail_length = detail.used as u32;
            RequestResult::done(status)
        }
    }
}

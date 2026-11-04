//! Explicit owned stages carried by the original checked user request. The
//! caller resumes these stages, never the already executed service prefix.
use core::mem::{align_of, size_of};

use mac_abi::{controls::*, traps::*, *};

use crate::{machine::scheduler::{callbacks::wait_call,
                                 service::{RequestAction, RequestResult}},
            toolbox::runtime::{AutomaticControlPrelude, AutomaticControlProgress,
                               TrustedWindowClose, UserControlBirth, UserControlOperation,
                               UserControlOperationProgress, UserDialogClose,
                               UserScrollBoxOperation, UserWindowBirth, UserWindowOperation,
                               UserWindowProgress, WindowBeginError}};

pub(crate) enum Continuation
{
    Direct(super::Pending),
    ScrollBoxStart,
    ScrollBox(UserScrollBoxOperation),
    Window(UserWindowBirth),
    WindowStart,
    WindowOperation(UserWindowOperation),
    DialogClose(UserDialogClose),
    TrustedWindowClose(TrustedWindowClose),
    Control(UserControlBirth),
    ControlOperation(UserControlOperation),
    ControlAutomatic(AutomaticControlPrelude),
    ControlAutomaticDone
}

fn wait(pending: &super::Pending) -> RequestResult
{
    match wait_call(pending)
    {
        Ok(_) =>
        {
            RequestResult { status: OK,
                            action: RequestAction::Replay }
        }
        Err(status) => RequestResult::done(status)
    }
}

/// Common original-envelope prefix after its real polling wire type/operation
/// was checked. No DTO conversion, event removal or event service replay here.
fn event_prelude(state: &mut Option<Continuation>, suspension: Result<(), Status>)
                 -> Option<RequestResult>
{
    if matches!(state, Some(Continuation::ControlAutomaticDone))
    {
        return None;
    }
    if state.is_none()
    {
        match AutomaticControlPrelude::begin(suspension)
        {
            Ok(Some(prelude)) => *state = Some(Continuation::ControlAutomatic(prelude)),
            Ok(None) => return None,
            Err(status) => return Some(RequestResult::done(status))
        }
    }
    let Some(Continuation::ControlAutomatic(prelude)) = state.as_mut()
    else
    {
        return Some(RequestResult::done(BAD_ARGUMENT));
    };
    match prelude.advance()
    {
        Ok(AutomaticControlProgress::Wait(pending)) => Some(wait(&pending)),
        Ok(AutomaticControlProgress::Complete) =>
        {
            // Real terminal work/readers/lease leave outside Desktop now; only
            // the Done sentinel remains in this SAME original Call identity.
            *state = Some(Continuation::ControlAutomaticDone);
            None
        }
        Err(status) => Some(RequestResult::done(status))
    }
}

/// # Safety
/// Root lends an exclusive, typed/validated supervisor snapshot belonging to
/// the original RequestIdentity. All nested inputs have already been copied or
/// pinned by user transport. This function never follows a raw user pointer.
/// No borrow of that snapshot survives a returned Replay/Yield.
pub(crate) unsafe fn dispatch_user(number: u32, pointer: *mut (), size: usize,
                                   state: &mut Option<Continuation>,
                                   direct: Option<crate::machine::traps::user::DirectArguments>,
                                   suspension: Result<(), Status>)
                                   -> Option<RequestResult>
{
    let result = match number
    {
        DEFINITION =>
        {
            let call = unsafe { snapshot::<definitions::UserCall>(pointer, size) }?;
            let Some(arguments) = direct
            else
            {
                return Some(RequestResult::done(ACCESS_DENIED));
            };
            if !arguments.process
            {
                let result = unsafe { super::direct::invoke_user_trusted(call, arguments) };
                return Some(RequestResult::done(result.map_or_else(|s| s, |()| OK)));
            }
            if state.is_none()
            {
                if let Err(status) = suspension
                {
                    return Some(RequestResult::done(status));
                }
                match super::direct::begin(call, arguments)
                {
                    Ok(pending) => *state = Some(Continuation::Direct(pending)),
                    Err(status) => return Some(RequestResult::done(status))
                }
            }
            let Some(Continuation::Direct(pending)) = state.as_mut()
            else
            {
                return Some(RequestResult::done(BAD_ARGUMENT));
            };
            match pending.poll()
            {
                Ok(Some(reply)) =>
                {
                    call.result = reply.result;
                    RequestResult::done(OK)
                }
                Ok(None) => wait(pending),
                Err(status) => RequestResult::done(status)
            }
        }
        DIALOG =>
        {
            let call = unsafe { snapshot::<dialogs::DialogCall>(pointer, size) }?;
            if call.operation != dialogs::CLOSE
            {
                return None;
            }
            if state.is_none()
            {
                if let Err(status) = suspension
                {
                    return Some(RequestResult::done(status));
                }
                call.item = 0;
                match UserDialogClose::prepare(call)
                {
                    Ok(close) => *state = Some(Continuation::DialogClose(close)),
                    Err(status) => return Some(RequestResult::done(status))
                }
            }
            let Some(Continuation::DialogClose(close)) = state.as_mut()
            else
            {
                return Some(RequestResult::done(BAD_ARGUMENT));
            };
            return Some(match close.advance()
                        {
                            Ok(Some(status)) => RequestResult::done(status),
                            Ok(None) =>
                            {
                                match close.wait()
                                {
                                    Ok(()) =>
                                    {
                                        RequestResult { status: OK,
                                                        action: RequestAction::Replay }
                                    }
                                    Err(status) => RequestResult::done(status)
                                }
                            }
                            Err(status) => RequestResult::done(status)
                        });
        }
        WINDOW =>
        {
            let call = unsafe { snapshot::<WindowCall>(pointer, size) }?;
            if call.operation != NEW_WINDOW
            {
                if matches!(state, Some(Continuation::WindowStart))
                {
                    *state = None;
                }
                if state.is_none()
                {
                    if let Err(status) = suspension
                    {
                        return Some(RequestResult::done(status));
                    }
                    match UserWindowOperation::begin(call.operation, &call.request)
                    {
                        Ok(None) =>
                        {
                            match TrustedWindowClose::begin(call.operation, &call.request)
                            {
                                Ok(Some(close)) =>
                                {
                                    *state = Some(Continuation::TrustedWindowClose(close))
                                }
                                Ok(None) => return None,
                                Err(cause) => return Some(window_begin_refused(cause, state))
                            }
                        }
                        Ok(Some(operation)) =>
                        {
                            *state = Some(Continuation::WindowOperation(operation))
                        }
                        Err(cause) => return Some(window_begin_refused(cause, state))
                    }
                }
                if let Some(Continuation::TrustedWindowClose(close)) = state.as_mut()
                {
                    return Some(match close.advance()
                                {
                                    Ok(Some(status)) => RequestResult::done(status),
                                    Ok(None) =>
                                    {
                                        match close.wait()
                                        {
                                            Ok(()) =>
                                            {
                                                RequestResult { status: OK,
                                                                action: RequestAction::Replay }
                                            }
                                            Err(status) => RequestResult::done(status)
                                        }
                                    }
                                    Err(status) => RequestResult::done(status)
                                });
                }
                let Some(Continuation::WindowOperation(operation)) = state.as_mut()
                else
                {
                    return Some(RequestResult::done(BAD_ARGUMENT));
                };
                return Some(match operation.advance()
                            {
                                Ok(Some(status)) => RequestResult::done(status),
                                Ok(None) =>
                                {
                                    match operation.wait()
                                    {
                                        Ok(()) =>
                                        {
                                            RequestResult { status: OK,
                                                            action: RequestAction::Replay }
                                        }
                                        Err(status) => RequestResult::done(status)
                                    }
                                }
                                Err(status) => RequestResult::done(status)
                            });
            }
            if state.is_none()
            {
                let id = call.request.procedure >> 4;
                match crate::loader::process_definition(*b"WDEF", id)
                {
                    Ok(false) => return None,
                    Err(cause) => return Some(RequestResult::done(crate::loader::status(cause))),
                    Ok(true) => ()
                }
                if let Err(status) = suspension
                {
                    return Some(RequestResult::done(status));
                }
                match UserWindowBirth::begin(&call.request)
                {
                    Ok(birth) => *state = Some(Continuation::Window(birth)),
                    Err(status) => return Some(RequestResult::done(status))
                }
            }
            let Some(Continuation::Window(birth)) = state.as_mut()
            else
            {
                return Some(RequestResult::done(BAD_ARGUMENT));
            };
            match birth.advance(&mut call.request)
            {
                Ok(UserWindowProgress::Wait) =>
                {
                    // The owner returns Wait only for an actual admitted job;
                    // its original thread parks through atomic readiness hints.
                    let Some(job) = birth.current_job()
                    else
                    {
                        return Some(RequestResult::done(BAD_ABI));
                    };
                    match crate::machine::scheduler::callbacks::wait_window_call(&job)
                    {
                        Ok(_) =>
                        {
                            RequestResult { status: OK,
                                            action: RequestAction::Replay }
                        }
                        Err(status) => RequestResult::done(status)
                    }
                }
                Ok(UserWindowProgress::Complete(status)) | Err(status) =>
                {
                    RequestResult::done(status)
                }
            }
        }
        SCROLL_BOX =>
        {
            let call = unsafe { snapshot::<mac_abi::scrollbox::Call>(pointer, size) }?;
            if matches!(state, Some(Continuation::ScrollBoxStart))
            {
                *state = None;
            }
            if state.is_none()
            {
                if let Err(status) = suspension
                {
                    return Some(RequestResult::done(status));
                }
                match UserScrollBoxOperation::begin(call)
                {
                    Ok(None) => return None,
                    Ok(Some(operation)) => *state = Some(Continuation::ScrollBox(operation)),
                    Err(WindowBeginError::Desktop { cause: crate::state::AccessError::Busy,
                                                    epoch }) =>
                    {
                        *state = Some(Continuation::ScrollBoxStart);
                        return Some(match crate::machine::scheduler::callbacks::wait_desktop(epoch)
                                    {
                                        Ok(_) =>
                                        {
                                            RequestResult { status: OK,
                                                            action: RequestAction::Replay }
                                        }
                                        Err(status) => RequestResult::done(status)
                                    });
                    }
                    Err(cause) => return Some(RequestResult::done(cause.status()))
                }
            }
            let Some(Continuation::ScrollBox(operation)) = state.as_mut()
            else
            {
                return None;
            };
            match operation.advance(call)
            {
                Ok(Some(status)) => RequestResult::done(status),
                Ok(None) =>
                {
                    match operation.wait()
                    {
                        Ok(()) =>
                        {
                            RequestResult { status: OK,
                                            action: RequestAction::Replay }
                        }
                        Err(status) => RequestResult::done(status)
                    }
                }
                Err(status) => RequestResult::done(status)
            }
        }
        mac_abi::traps::EVENTS =>
        {
            let call = unsafe { snapshot::<mac_abi::events::EventCall>(pointer, size) }?;
            if !matches!(call.operation,
                         mac_abi::events::NATIVE_PEEK
                         | mac_abi::events::OS_PEEK
                         | mac_abi::events::OS_GET)
            {
                return None;
            }
            return event_prelude(state, suspension);
        }
        mac_abi::traps::POSITIONED_EVENT =>
        {
            let call = unsafe { snapshot::<mac_abi::events::PositionedEventCall>(pointer, size) }?;
            if !matches!(call.operation,
                         mac_abi::events::POSITION_NEXT
                         | mac_abi::events::POSITION_PEEK
                         | mac_abi::events::POSITION_WAIT)
            {
                return None;
            }
            return event_prelude(state, suspension);
        }
        mac_abi::traps::NEXT_EVENT =>
        {
            unsafe { snapshot::<mac_abi::traps::NextEventCall>(pointer, size) }?;
            return event_prelude(state, suspension);
        }
        mac_abi::traps::WAIT_EVENT =>
        {
            unsafe { snapshot::<mac_abi::traps::WaitEventCall>(pointer, size) }?;
            return event_prelude(state, suspension);
        }
        CONTROL =>
        {
            let call = unsafe { snapshot::<ControlCall>(pointer, size) }?;
            if !matches!(call.operation, CREATE_STANDARD | CREATE_SCROLLBAR)
            {
                if state.is_none()
                {
                    match UserControlOperation::begin(call, suspension)
                    {
                        Ok(Some(operation)) =>
                        {
                            *state = Some(Continuation::ControlOperation(operation))
                        }
                        Ok(None) => return None,
                        Err(status) => return Some(RequestResult::done(status))
                    }
                }
                let Some(Continuation::ControlOperation(operation)) = state.as_mut()
                else
                {
                    return Some(RequestResult::done(BAD_ARGUMENT));
                };
                return Some(match operation.advance(call)
                            {
                                Ok(UserControlOperationProgress::ClaimWait(ticket)) => {
                                    match crate::machine::scheduler::callbacks::wait_control_claim(&ticket) {
                                        Ok(_)=>RequestResult {status:OK,action:RequestAction::Replay},
                                        Err(status)=>RequestResult::done(status),
                                    }
                                },
                                Ok(UserControlOperationProgress::Wait(pending)) => wait(&pending),
                                Ok(UserControlOperationProgress::DisposalWait(cell)) => {
                                    match crate::machine::scheduler::callbacks::wait_control_disposal(&cell) {
                                        Ok(_) => RequestResult {status:OK,action:RequestAction::Replay},
                                        Err(status) => RequestResult::done(status),
                                    }
                                },
                                Ok(UserControlOperationProgress::Complete(status))
                                | Err(status) => RequestResult::done(status)
                            });
            }
            if state.is_none()
            {
                let id = match crate::toolbox::controls::creation_definition_id(call)
                {
                    Ok(id) => id,
                    Err(status) => return Some(RequestResult::done(status))
                };
                match crate::loader::process_definition(*b"CDEF", id)
                {
                    Ok(false) => return None,
                    Err(cause) => return Some(RequestResult::done(crate::loader::status(cause))),
                    Ok(true) => ()
                }
                if let Err(status) = suspension
                {
                    return Some(RequestResult::done(status));
                }
                match UserControlBirth::begin(call)
                {
                    Ok(birth) => *state = Some(Continuation::Control(birth)),
                    Err(status) => return Some(RequestResult::done(status))
                }
            }
            let Some(Continuation::Control(birth)) = state.as_mut()
            else
            {
                return Some(RequestResult::done(BAD_ARGUMENT));
            };
            match birth.advance(call)
            {
                Ok(Some(status)) | Err(status) => RequestResult::done(status),
                Ok(None) => wait(birth.pending())
            }
        }
        _ => return None
    };
    if matches!(result.action, RequestAction::Return | RequestAction::Exit)
    {
        // Original permits/payloads and completed callback pins dispose here,
        // after every Desktop/metadata guard in advance() has ended.
        drop(state.take());
    }
    Some(result)
}

unsafe fn snapshot<'a, T>(pointer: *mut (), size: usize) -> Option<&'a mut T>
{
    if pointer.is_null()
       || size != size_of::<T>()
       || !pointer.addr().is_multiple_of(align_of::<T>())
    {
        return None;
    }
    // SAFETY: the outer checked-envelope contract supplies the exclusive owned
    // typed snapshot. The layout checks above do not claim to validate U memory.
    Some(unsafe { &mut *pointer.cast::<T>() })
}

fn window_begin_refused(cause: WindowBeginError, state: &mut Option<Continuation>)
                        -> RequestResult
{
    match cause
    {
        WindowBeginError::Desktop { cause: crate::state::AccessError::Busy,
                                    epoch } =>
        {
            *state = Some(Continuation::WindowStart);
            match crate::machine::scheduler::callbacks::wait_desktop(epoch)
            {
                Ok(_) =>
                {
                    RequestResult { status: OK,
                                    action: RequestAction::Replay }
                }
                Err(status) => RequestResult::done(status)
            }
        }
        cause => RequestResult::done(cause.status())
    }
}

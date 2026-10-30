//! Core-only native thread/TLS transport for the platform CRT. Resident code
//! authenticates the actual Process and THRD; copied IDs grant no authority.
use core::fmt;

use mac_abi::{native_threads as thread, tls};

use crate::{Client, Error, ThreadId, types::InlineText};
#[derive(Clone, Copy, Debug)]
pub enum Failure
{
    Resident
    {
        cause:  Error,
        detail: InlineText<192>
    },
    Protocol(&'static str)
}
impl Failure
{
    pub fn cause(&self) -> Error
    {
        match self
        {
            Self::Resident { cause,
                             .. } => *cause,
            Self::Protocol(_) => Error::BadAbi
        }
    }
}
impl fmt::Display for Failure
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Resident { cause,
                             detail } => write!(out, "{cause}: {detail}"),
            Self::Protocol(detail) => write!(out, "native thread/TLS protocol refusal: {detail}")
        }
    }
}
impl core::error::Error for Failure
{
}
pub type Result<T> = core::result::Result<T, Failure>;
pub(crate) fn result(status: i64, bytes: [u8; 192], length: u32) -> Result<()>
{
    let status: mac_abi::Status =
        status.try_into().map_err(|_| Failure::Protocol("status exceeds Status width"))?;
    if status == mac_abi::OK
    {
        return if length == 0
        {
            Ok(())
        }
        else
        {
            Err(Failure::Protocol("success returned failure detail"))
        };
    }
    if status > 0
    {
        return Err(Failure::Protocol("positive status is not a successful result"));
    }
    let detail = InlineText::from_reply(bytes, length).map_err(|_| {
                                                          Failure::Protocol("resident detail is \
                                                                             not bounded UTF-8")
                                                      })?;
    if detail.as_str().is_empty()
    {
        return Err(Failure::Protocol("refusal omitted its typed diagnostic"));
    }
    Err(Failure::Resident { cause: Error::from_status(status),
                            detail })
}
/// Unique right to observe/join one actual child; it retains no kernel Core,
/// stack or context. It is !Send/!Sync: retain it on the creating application
/// thread, and drop it in that thread's ordinary native application context.
/// Dropping it detaches the child without cancelling it. This first owner does
/// not supply std's cross-thread Send JoinHandle contract.
pub struct NativeJoinRight<'a>
{
    client:  Client<'a>,
    right:   Option<JoinIdentity>,
    thread:  ThreadId,
    _affine: core::marker::PhantomData<*mut ()>
}
struct JoinIdentity(u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeThreadState
{
    Preparing,
    Running,
    Returning,
    Complete,
    Aborted
}
impl NativeJoinRight<'_>
{
    pub fn thread(&self) -> ThreadId
    {
        self.thread
    }

    pub fn state(&self) -> Result<NativeThreadState>
    {
        let right =
            self.right.as_ref().ok_or(Failure::Protocol("join right has already been consumed"))?;
        let mut call = thread::Call { operation: thread::QUERY,
                                      right: right.0,
                                      ..Default::default() };
        // SAFETY: querying this uniquely owned right transfers no pointer.
        unsafe {
            self.client.native_thread(&mut call).map_err(|cause| cause.diagnostic())?;
        }
        if ThreadId(call.thread) != self.thread
        {
            return Err(Failure::Protocol("QUERY changed this join right's canonical child"));
        }
        match call.state
        {
            thread::PREPARING => Ok(NativeThreadState::Preparing),
            thread::RUNNING => Ok(NativeThreadState::Running),
            thread::RETURNING => Ok(NativeThreadState::Returning),
            thread::COMPLETE => Ok(NativeThreadState::Complete),
            thread::ABORTED => Ok(NativeThreadState::Aborted),
            _ => Err(Failure::Protocol("QUERY returned an unknown child state"))
        }
    }

    /// Waits for actual TLS FINISH and final execution-claim release. A typed
    /// refusal returns this same owner for retry, inspection or ordinary Drop.
    pub fn join(mut self) -> core::result::Result<ThreadId, (Failure, Self)>
    {
        let Some(right) = self.right.as_ref()
        else
        {
            return Err((Failure::Protocol("join right has already been consumed"), self));
        };
        let mut call = thread::Call { operation: thread::JOIN,
                                      right: right.0,
                                      ..Default::default() };
        // SAFETY: this object owns the one right; no pointer crosses the call.
        if let Err(cause) = unsafe { self.client.native_thread(&mut call) }
        {
            return Err((cause.diagnostic(), self));
        }
        self.right = None;
        Ok(self.thread)
    }

    pub fn detach(mut self) -> core::result::Result<(), (Failure, Self)>
    {
        let Some(right) = self.right.as_ref()
        else
        {
            return Err((Failure::Protocol("join right has already been consumed"), self));
        };
        let mut call = thread::Call { operation: thread::DETACH,
                                      right: right.0,
                                      ..Default::default() };
        // SAFETY: the unique right is consumed only after real acceptance.
        if let Err(cause) = unsafe { self.client.native_thread(&mut call) }
        {
            return Err((cause.diagnostic(), self));
        }
        self.right = None;
        Ok(())
    }
}
impl Drop for NativeJoinRight<'_>
{
    fn drop(&mut self)
    {
        if let Some(right) = self.right.take()
        {
            let mut call = thread::Call { operation: thread::DETACH,
                                          right: right.0,
                                          ..Default::default() };
            // SAFETY: create_native_thread's contract retains this affine
            // owner on its creating native application continuation.
            match unsafe { self.client.native_thread(&mut call) }
            {
                Ok(()) => (),
                Err(cause) if cause.cause() == Error::NotFound =>
                {
                    // This private SDK owner came only from accepted CREATE.
                    // Its generation is now invalid: the genuine owner walk or
                    // earlier removal already owns the original disposal.
                }
                Err(cause) =>
                {
                    // Unsupported context/protocol corruption is not reported
                    // as cleanup. No panic or fabricated disposal is attempted.
                    let _ = self.client
                                .log("NATIVE_JOIN_RIGHT_RELEASE_REFUSED: cleanup not established; \
                                      original registered object remains retained\n");
                    match cause.diagnostic()
                    {
                        Failure::Resident { detail,
                                            .. } =>
                        {
                            let _ = self.client.log(detail.as_str());
                        }
                        Failure::Protocol(detail) =>
                        {
                            let _ = self.client.log(detail);
                        }
                    }
                }
            }
        }
    }
}
/// CREATE refusal and transfer uncertainty are different ownership outcomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreateTransfer
{
    Retained,
    Indeterminate
}
#[derive(Clone, Copy, Debug)]
pub struct CreateFailure
{
    cause:    Failure,
    transfer: CreateTransfer
}
impl CreateFailure
{
    pub fn cause(&self) -> Failure
    {
        self.cause
    }

    pub fn transfer(&self) -> CreateTransfer
    {
        self.transfer
    }
}
impl fmt::Display for CreateFailure
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        write!(out, "{}; CREATE data transfer: {:?}", self.cause, self.transfer)
    }
}
impl core::error::Error for CreateFailure
{
}
impl<'a> Client<'a>
{
    /// # Safety
    /// Entry/data follow `native_thread` CREATE's transfer and TLS contract.
    /// The returned !Send/!Sync owner must stay on this actual creating
    /// application thread and be dropped only in its ordinary application
    /// context, outside module/definition/policy callbacks or manually
    /// nonpreemptible/IRQ-masked scopes. This obligation is part of unsafe CRT
    /// construction; this owner is not std's transferable Send JoinHandle.
    /// Only CreateTransfer::Retained proves data remains owned by the caller.
    /// Indeterminate requires preserving that uncertainty; freeing data could
    /// race an accepted child whose reply was malformed. A successful result
    /// owns exactly one affine join right for the accepted child.
    pub unsafe fn create_native_thread(
        self, entry: unsafe extern "C" fn(*mut u8), data: *mut u8, stack_bytes: usize)
        -> core::result::Result<NativeJoinRight<'a>, CreateFailure>
    {
        let mut call = thread::Call { operation: thread::CREATE,
                                      entry: entry as usize,
                                      data: data as usize,
                                      stack_bytes: stack_bytes as u64,
                                      ..Default::default() };
        // SAFETY: the caller supplies the documented native CRT contract.
        if let Err(failure) = unsafe { self.native_thread(&mut call) }
        {
            return Err(CreateFailure { cause:    failure.diagnostic(),
                                       transfer: failure.create_transfer()
                                                        .unwrap_or(CreateTransfer::Indeterminate) });
        }
        Ok(NativeJoinRight { client:  self,
                             right:   Some(JoinIdentity(call.right)),
                             thread:  ThreadId(call.thread),
                             _affine: core::marker::PhantomData })
    }
}
/// A raw request preserves CREATE's data-transfer uncertainty in its error.
#[derive(Clone, Copy, Debug)]
pub struct RequestFailure
{
    cause:    Failure,
    transfer: Option<CreateTransfer>
}
impl RequestFailure
{
    pub fn cause(&self) -> Error
    {
        self.cause.cause()
    }

    pub fn diagnostic(&self) -> Failure
    {
        self.cause
    }

    pub fn create_transfer(&self) -> Option<CreateTransfer>
    {
        self.transfer
    }
}
impl fmt::Display for RequestFailure
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        self.cause.fmt(out)?;
        if let Some(transfer) = self.transfer
        {
            write!(out, "; CREATE data transfer: {transfer:?}")?;
        }
        Ok(())
    }
}
impl core::error::Error for RequestFailure
{
}
impl Client<'_>
{
    pub fn current_native_thread(self) -> Result<ThreadId>
    {
        let mut call = thread::Call::default();
        // SAFETY: CURRENT transfers no pointer or child ownership.
        unsafe {
            self.native_thread(&mut call).map_err(|cause| cause.diagnostic())?;
        }
        if call.thread == 0
        {
            return Err(Failure::Protocol("CURRENT returned no canonical THRD"));
        }
        Ok(ThreadId(call.thread))
    }

    /// Native platform-CRT thread transport. Copied thread/right IDs do not
    /// grant authority; the System validates the genuine current Process.
    ///
    /// # Safety
    /// CREATE's entry is executable in this Process with signature
    /// `unsafe extern "C" fn(*mut u8)`. A successful CREATE transfers the opaque
    /// data exactly once and may start before this call returns; resident refusal leaves
    /// it with the caller. The entry must run `teardown_tls` on its actual thread
    /// before returning, and must not unwind across the C boundary. JOIN consumes
    /// its unique right on success; DETACH consumes it without cancelling work.
    /// A retained join right may be dropped only from the creating native
    /// application thread's ordinary context, outside module/definition/policy
    /// callbacks, manual critical/IRQ-masked scopes and foreign Process execution.
    /// NAME points at live readable UTF-8 until the call ends. EXIT is child-only,
    /// nonreturning after acceptance, and requires actual TLS FINISH first.
    pub unsafe fn native_thread(self, call: &mut thread::Call)
                                -> core::result::Result<(), RequestFailure>
    {
        let original = *call;
        let status = self.invoke(mac_abi::traps::NATIVE_THREAD, call);
        let response = (|| -> Result<()> {
            if call.operation != original.operation
               || call.flags != original.flags
               || call.reserved != original.reserved
               || call.reserved2 != original.reserved2
               || call.reserved3 != original.reserved3
               || call.entry != original.entry
               || call.data != original.data
               || call.stack_bytes != original.stack_bytes
               || call.seconds != original.seconds
               || call.nanoseconds != original.nanoseconds
               || call.name != original.name
               || call.name_length != original.name_length
               || (original.operation != thread::CREATE && call.right != original.right)
            {
                return Err(Failure::Protocol("native thread reply changed input fields"));
            }
            result(status, call.detail, call.detail_length)?;
            if call.state > thread::ABORTED || call.outcome > thread::ABORTED_OUTCOME
            {
                return Err(Failure::Protocol("native thread reply contains an unknown state or \
                                              outcome"));
            }
            if original.operation == thread::CREATE && (call.thread == 0 || call.right == 0)
            {
                return Err(Failure::Protocol("CREATE omitted the published THRD or unique join \
                                              right"));
            }
            if original.operation == thread::PARALLELISM && call.parallelism == 0
            {
                return Err(Failure::Protocol("PARALLELISM returned no eligible execution"));
            }
            Ok(())
        })();
        response.map_err(|cause| {
                    RequestFailure { transfer: if original.operation == thread::CREATE
                                     {
                                         Some(match cause
                                              {
                                                  Failure::Resident { .. } =>
                                                  {
                                                      CreateTransfer::Retained
                                                  }
                                                  Failure::Protocol(_) =>
                                                  {
                                                      CreateTransfer::Indeterminate
                                                  }
                                              })
                                     }
                                     else
                                     {
                                         None
                                     },
                                     cause }
                })
    }

    /// # Safety
    /// CREATE's nonzero callback has the C destructor signature and remains
    /// executable in this Process. Values are opaque. NEXT's callback must run
    /// only in this thread, before ACK; no callback may unwind across the ABI.
    /// The caller owns teardown ordering and must not use TLS after FINISH.
    pub unsafe fn tls(self, call: &mut tls::Call) -> Result<()>
    {
        let original = *call;
        let status = self.invoke(mac_abi::traps::TLS, call);
        if call.operation != original.operation
           || call.flags != original.flags
           || call.reserved != original.reserved
           || call.reserved2 != original.reserved2
           || call.present > 1
           || call.round > tls::DESTRUCTOR_ROUNDS
        {
            return Err(Failure::Protocol("TLS returned invalid control fields"));
        }
        result(status, call.detail, call.detail_length)
    }
}

/// # Safety
/// The actual native application thread owns teardown. Resident validates each
/// callback against its pinned Process executable and retains it until ACK.
pub unsafe fn teardown_tls(client: Client<'_>) -> Result<()>
{
    use mac_abi::tls as wire;
    let mut begin = wire::Call { operation: wire::BEGIN,
                                 ..Default::default() };
    unsafe {
        client.tls(&mut begin)?;
    }
    loop
    {
        let mut next = wire::Call { operation: wire::NEXT,
                                    ..Default::default() };
        unsafe {
            client.tls(&mut next)?;
        }
        if next.present == 0
        {
            break;
        }
        if next.destructor == 0 || next.ticket == 0
        {
            return Err(Failure::Protocol("NEXT omitted callback receipt"));
        }
        let callback: unsafe extern "C" fn(*mut u8) =
            unsafe { core::mem::transmute(next.destructor) };
        unsafe {
            callback(next.value as *mut u8);
        }
        let mut ack = wire::Call { operation: wire::ACK,
                                   ticket: next.ticket,
                                   ..Default::default() };
        unsafe {
            client.tls(&mut ack)?;
        }
    }
    let mut finish = wire::Call { operation: wire::FINISH,
                                  ..Default::default() };
    unsafe { client.tls(&mut finish) }
}

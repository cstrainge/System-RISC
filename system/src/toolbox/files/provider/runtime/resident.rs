//! Owned nonparking requests for resident user interfaces. The preflighted
//! continuation itself becomes the cancellation queue node if its UI disappears.
//! No allocation or foreign callback is needed to hand cleanup to the System.
#![forbid(unsafe_code)]
use super::*;

#[derive(Clone, Copy)]
pub(crate) enum Reply
{
    File
    {
        operation:   u32,
        index:       u32,
        volume_info: VolumeInfo,
        entry:       CatalogInfo
    },
    Scan(FileScanCall),
    PreparedName
    {
        bytes:  [u8; INLINE_STRING_BYTES],
        length: usize
    }
}
#[derive(Clone, Copy)]
enum Call
{
    File(Input),
    Scan(FileScanCall),
    PrepareName(u32)
}
#[derive(Clone, Copy)]
struct Input
{
    operation: u32,
    volume:    u32,
    cnid:      u32,
    index:     u32,
    position:  u64
}
impl Input
{
    fn wire(self) -> FileCall
    {
        FileCall { operation: self.operation,
                   volume: self.volume,
                   cnid: self.cnid,
                   index: self.index,
                   position: self.position,
                   ..Default::default() }
    }
}
struct Continuation
{
    subject:           IoSubject,
    call:              Call,
    flight:            Option<Flight>,
    reply:             Option<Result<Reply, Status>>,
    next:              Option<Box<Continuation>>,
    recovery_activity: Option<crate::recovery::Activity>,
    failure:           Option<Status>
}
static CLEANUP: Spinlock<Option<Box<Continuation>>> = Spinlock::new(None);
static CLEANUP_PENDING: AtomicBool = AtomicBool::new(false);

/// This is an owned continuation, not an issued object identity. Its provider
/// work uses the existing registered job and genuine original DIOP authority.
pub(crate) struct Request
{
    continuation: Option<Box<Continuation>>
}
impl Request
{
    pub(crate) fn file(subject: &IoSubject, call: FileCall, text: &str) -> Result<Self, Status>
    {
        if !matches!(call.operation,
                     VOLUME_INFO | VOLUME_INDEX | VOLUME_COUNT | CATALOG | LOOKUP | NEW_FOLDER)
        {
            return Err(NOT_SUPPORTED);
        }
        if !call.name.is_null()
           || !call.output.is_null()
           || call.name_length != 0
           || call.capacity != 0
           || call.reserved != 0
        {
            return Err(BAD_ARGUMENT);
        }
        let input = Input { operation: call.operation,
                            volume:    call.volume,
                            cnid:      call.cnid,
                            index:     call.index,
                            position:  call.position };
        Self::begin(subject, Call::File(input), text)
    }

    pub(crate) fn scan(subject: &IoSubject, call: FileScanCall) -> Result<Self, Status>
    {
        Self::begin(subject, Call::Scan(call), "")
    }

    pub(crate) fn prepare_name(subject: &IoSubject, volume: u32, name: &str)
                               -> Result<Self, Status>
    {
        Self::begin(subject, Call::PrepareName(volume), name)
    }

    fn begin(subject: &IoSubject, call: Call, text: &str) -> Result<Self, Status>
    {
        // Admit cancellation storage before publishing any provider operation.
        let mut continuation = Box::try_new(Continuation { subject: subject.clone(),
                                                           call,
                                                           flight: None,
                                                           reply: None,
                                                           next: None,
                                                           recovery_activity:
                                                               crate::context::recovery_activity(),
                                                           failure: None }).map_err(|_| NO_MEMORY)?;
        let progress = crate::context::with_io_subject(subject.clone(), || {
            let access = super::super::super::subject::Access::Operation(subject.scope());
            match call
            {
                Call::File(input) => background::metadata(access, &mut input.wire(), text, 0),
                Call::Scan(mut call) => background::scan(access, &mut call),
                Call::PrepareName(volume) => background::prepare_name(access, volume, text)
            }
        })?;
        continuation.accept(progress);
        Ok(Self { continuation: Some(continuation) })
    }

    /// Advance at most one existing job transition. The caller holds no Files,
    /// Desktop, UI-owner or other manager guard while polling.
    pub(crate) fn poll(&mut self) -> Result<Option<Reply>, Status>
    {
        let node = self.continuation.as_mut().ok_or(NOT_FOUND)?;
        if node.reply.is_none()
        {
            let subject = node.subject.clone();
            // Move the receipt only after entry succeeds. A refused context
            // must leave the live continuation available for cancellation.
            let activity = node.recovery_activity.clone();
            let progress =
                in_context(subject, activity.as_ref(), || node.flight.take().map(advance))?;
            if let Some(progress) = progress
            {
                node.accept(progress);
            }
        }
        match node.reply.take()
        {
            Some(reply) =>
            {
                self.continuation = None;
                reply.map(Some)
            }
            None => Ok(None)
        }
    }

    pub(crate) fn pending(&self) -> bool
    {
        self.continuation.as_ref().is_some_and(|node| node.flight.is_some())
    }

    pub(crate) fn cancel(&mut self)
    {
        if let Some(flight) = self.continuation.as_mut().and_then(|node| node.flight.as_mut())
        {
            flight.abandon();
        }
    }
}
impl Continuation
{
    fn accept(&mut self, progress: Progress)
    {
        match progress
        {
            Progress::Pending(flight) => self.flight = Some(flight),
            Progress::Done(reply) => self.reply = Some(reply.and_then(|reply| self.project(reply)))
        }
    }

    fn project(&self, reply: super::Reply) -> Result<Reply, Status>
    {
        match (self.call, reply)
        {
            (Call::Scan(_), super::Reply::Scan(reply)) => Ok(Reply::Scan(reply)),
            (Call::PrepareName(_), super::Reply::Bytes(reply)) =>
            {
                let prepared = fs::PreparedName::decode(&reply).map_err(|_| BAD_ABI)?;
                let mut bytes = [0; INLINE_STRING_BYTES];
                let name = prepared.name.as_bytes();
                bytes.get_mut(..name.len()).ok_or(BAD_ABI)?.copy_from_slice(name);
                Ok(Reply::PreparedName { bytes,
                                         length: name.len() })
            }
            (Call::File(input), reply) =>
            {
                let mut index = input.index;
                let mut volume_info = VolumeInfo::default();
                let mut entry = CatalogInfo::default();
                match reply
                {
                    super::Reply::Count(count) => index = count,
                    super::Reply::Mounted(volume) => volume_info = volume,
                    super::Reply::Entry(found) => entry = found,
                    _ => return Err(BAD_ABI)
                }
                Ok(Reply::File { operation: input.operation,
                                 index,
                                 volume_info,
                                 entry })
            }
            _ => Err(BAD_ABI)
        }
    }
}
fn enqueue(mut node: Box<Continuation>)
{
    InvocationGate::run(|| {
        let mut queue = CLEANUP.lock();
        node.next = queue.take();
        *queue = Some(node);
        CLEANUP_PENDING.store(true, Ordering::Release);
    });
}
impl Drop for Request
{
    fn drop(&mut self)
    {
        if let Some(mut node) = self.continuation.take()
           && let Some(flight) = node.flight.as_mut()
        {
            flight.abandon();
            enqueue(node);
        }
    }
}
pub(super) fn cleanup_pending() -> bool
{
    CLEANUP_PENDING.load(Ordering::Acquire)
}
pub(super) fn pump_cleanup()
{
    if !cleanup_pending()
    {
        return;
    }
    let node = InvocationGate::run(|| {
        let mut queue = CLEANUP.lock();
        let mut node = queue.take()?;
        *queue = node.next.take();
        CLEANUP_PENDING.store(queue.is_some(), Ordering::Release);
        Some(node)
    });
    let Some(mut node) = node
    else
    {
        return;
    };
    let subject = node.subject.clone();
    let activity = node.recovery_activity.clone();
    let advanced = in_context(subject, activity.as_ref(), || node.flight.take().map(advance));
    match advanced
    {
        Ok(Some(progress)) => node.accept(progress),
        Ok(None) => (),
        Err(cause) =>
        {
            if node.failure != Some(cause)
            {
                node.failure = Some(cause);
                serial!("RESIDENT_UI_IO_CLEANUP_RETAINED status={cause}; original receipt \
                         retained for retry");
            }
        }
    }
    if node.flight.is_some()
    {
        enqueue(node);
    }
}

fn in_context<R>(subject: IoSubject, activity: Option<&crate::recovery::Activity>,
                 operation: impl FnOnce() -> R)
                 -> Result<R, Status>
{
    if let Some(activity) = activity
    {
        activity.run(|| crate::context::with_io_subject(subject, operation))?
    }
    else
    {
        crate::context::with_io_subject(subject, operation)
    }
}

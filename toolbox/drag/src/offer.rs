use core::{future::Future,
           pin::Pin,
           task::{Context, Poll}};

use super::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropAction
{
    Copy,
    Move
}
/// Actions the receiver implements. The manager chooses from current physical
/// state; a response never coerces a forced copy into a move (or the reverse).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropActions
{
    Copy,
    Move,
    CopyOrMove
}
impl DropActions
{
    const fn flags(self) -> u32
    {
        match self
        {
            Self::Copy => wire::ACCEPT_COPY,
            Self::Move => wire::ACCEPT_MOVE,
            Self::CopyOrMove => wire::ACCEPT_COPY | wire::ACCEPT_MOVE
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragOutcome
{
    Copied,
    Moved,
    Rejected,
    Cancelled
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackPhase
{
    Entered,
    Within,
    Left
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target
{
    Folder
    {
        volume: VolumeId,
        id:     Cnid
    },
    Trash
    {
        volume: VolumeId,
        id:     Cnid
    },
    /// Rearrange icons already in this folder, including immutable volumes.
    /// Positions are view state; no file is copied or moved between folders.
    IconLayout
    {
        volume: VolumeId,
        id:     Cnid
    },
    Application
    {
        volume: VolumeId,
        id:     Cnid
    },
    /// Unmount one volume offered as its root-directory reference. Read-only
    /// mounts are valid; the resident service rejects startup/busy volumes.
    Eject
    {
        volume: VolumeId
    },
    Content
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Response
{
    Reject,
    Accept
    {
        target:    Target,
        /// Willingness, not a forced action. The manager also validates mounted
        /// filesystem support; an unsupported action records a normal rejection.
        actions:   DropActions,
        highlight: Rect
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Item(u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlavorInfo
{
    pub kind:     FlavorType,
    pub promised: bool,
    pub length:   usize
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flavors<'a>
{
    route:   Route<'a>,
    summary: Option<(usize, u32)>
}
impl Flavors<'_>
{
    /// Intersection computed once when the source begins, carried by each offer.
    pub fn all_file_items(self) -> bool
    {
        self.summary.is_some_and(|(_, common)| common & 1 != 0)
    }

    /// Frozen once at Begin; no service call or item inspection during motion.
    pub fn is_volume_root(self) -> bool
    {
        self.summary.is_some_and(|(_, common)| common & 4 != 0)
    }

    pub fn item_count(self) -> Result<usize>
    {
        if let Some((count, _)) = self.summary
        {
            return Ok(count);
        }
        self.route
            .call(wire::Call { operation: wire::INFO,
                               ..Default::default() },
                  &[],
                  &mut [])
            .map(|c| c.info.items as usize)
    }

    pub fn item(self, index: usize) -> Result<Item>
    {
        self.route
            .call(wire::Call { operation: wire::ITEM_AT,
                               index: u32::try_from(index).map_err(|_| DragError::NoSession)?,
                               ..Default::default() },
                  &[],
                  &mut [])
            .map(|c| Item(c.item))
    }

    pub fn flavor(self, item: Item, index: usize) -> Result<FlavorInfo>
    {
        let call = self.route.call(wire::Call { operation: wire::FLAVOR_AT,
                                                 item: item.0,
                                                 index: u32::try_from(index).map_err(|_| {
                                                                                DragError::NoSession
                                                                            })?,
                                                 ..Default::default() },
                                    &[],
                                    &mut [])?;
        Ok(FlavorInfo { kind:     FlavorType(call.kind),
                        promised: call.flags != 0,
                        length:   call.transferred })
    }

    pub fn has<T: FlavorData>(self, item: Item) -> Result<bool>
    {
        self.has_type(item, T::TYPE)
    }

    pub fn has_type(self, item: Item, kind: FlavorType) -> Result<bool>
    {
        for index in 0..wire::MAX_FLAVORS
        {
            match self.flavor(item, index)
            {
                Ok(info) if info.kind == kind => return Ok(true),
                Ok(_) => (),
                Err(DragError::NoSession) => return Ok(false),
                Err(DragError::Unknown(wire::FLAVOR_UNAVAILABLE)) => return Ok(false),
                Err(error) => return Err(error)
            }
        }
        Ok(false)
    }
}
/// Tracking owns a response guard, never a data-taking capability.
///
/// ```compile_fail
/// fn copyable<T: Copy>() {}
/// copyable::<toolbox_drag::TrackingOffer<'static>>();
/// ```
/// An ignored or failed response rejects this position automatically.
#[must_use = "an ignored tracking offer rejects its position"]
#[derive(Debug, PartialEq, Eq)]
pub struct TrackingOffer<'a>
{
    route:     Route<'a>,
    window:    Option<WindowId>,
    at:        Point,
    summary:   (usize, u32),
    pub phase: TrackPhase,
    responded: bool
}
impl<'a> TrackingOffer<'a>
{
    pub const fn id(&self) -> wire::DragId
    {
        wire::DragId(self.route.id)
    }

    pub const fn flavors(&self) -> Flavors<'a>
    {
        Flavors { route:   self.route,
                  summary: Some(self.summary) }
    }

    pub const fn window(&self) -> Option<WindowId>
    {
        self.window
    }

    pub const fn position(&self) -> Point
    {
        self.at
    }

    /// Inspect immutable inline reference metadata without taking a flavor or
    /// invoking a promise. Promised-only references remain unavailable here.
    pub fn file_metadata(&self, index: usize) -> Result<FileItem>
    {
        let mut bytes = [0; wire::FILE_ITEM_BYTES];
        let call = self.route.call(wire::Call { operation: wire::FILE_ITEM_METADATA,
                                                 index:
                                                     u32::try_from(index).map_err(|_| {
                                                         DragError::Unknown(mac_abi::BAD_ARGUMENT)
                                                     })?,
                                                 ..Default::default() },
                                    &[],
                                    &mut bytes)?;
        FileItem::decode(bytes.get(..call.transferred)
                              .ok_or(DragError::Unknown(mac_abi::BAD_ABI))?)
    }

    pub fn respond(mut self, response: Response) -> Result<()>
    {
        let (flags, volume, parent, bounds) = match response
        {
            Response::Reject => (0, 0, 0, Rect::default()),
            Response::Accept { target,
                               actions,
                               highlight } =>
            {
                let flags = 1 | actions.flags();
                match target
                {
                    Target::Folder { volume,
                                     id } => (flags, volume.0, id.0, highlight),
                    Target::Trash { volume,
                                    id } => (flags | wire::TARGET_TRASH, volume.0, id.0, highlight),
                    Target::IconLayout { volume,
                                         id } =>
                    {
                        (flags | wire::TARGET_ICON_LAYOUT, volume.0, id.0, highlight)
                    }
                    Target::Application { volume,
                                          id } =>
                    {
                        (flags | wire::TARGET_APPLICATION, volume.0, id.0, highlight)
                    }
                    Target::Content => (flags, 0, 0, highlight),
                    Target::Eject { volume } =>
                    {
                        (flags | wire::TARGET_EJECT, volume.0, mac_abi::files::ROOT_ID, highlight)
                    }
                }
            }
        };
        self.route.call(wire::Call { operation: wire::ACCEPT,
                                      point: self.at,
                                      window: self.window.map_or(0, |w| w.0),
                                      flags,
                                      volume,
                                      parent,
                                      bounds,
                                      ..Default::default() },
                         &[],
                         &mut [])?;
        self.responded = true;
        Ok(())
    }
}
impl Drop for TrackingOffer<'_>
{
    fn drop(&mut self)
    {
        if !self.responded && self.phase != TrackPhase::Left
        {
            let _ = self.route.call(wire::Call { operation: wire::ABANDON,
                                                 index: wire::TRACKING,
                                                 point: self.at,
                                                 window: self.window.map_or(0, |w| w.0),
                                                 ..Default::default() },
                                    &[],
                                    &mut []);
        }
    }
}
/// Sole receiver owner. Leaving scope without completing rejects the drop.
///
/// ```compile_fail
/// fn copyable<T: Copy>() {}
/// copyable::<toolbox_drag::DropPayload<'static>>();
/// ```
/// Metadata and take futures borrow its capability; completion consumes it.
#[must_use = "dropping an unhandled payload rejects the drop"]
#[derive(Debug, PartialEq, Eq)]
pub struct DropPayload<'a>
{
    route:      Route<'a>,
    window:     Option<WindowId>,
    at:         Point,
    pub action: DropAction,
    completed:  bool
}
impl<'a> DropPayload<'a>
{
    pub const fn flavors(&self) -> Flavors<'a>
    {
        Flavors { route:   self.route,
                  summary: None }
    }

    pub const fn window(&self) -> Option<WindowId>
    {
        self.window
    }

    pub const fn position(&self) -> Point
    {
        self.at
    }

    pub fn origin(&self) -> Result<Point>
    {
        self.route
            .call(wire::Call { operation: wire::INFO,
                               ..Default::default() },
                  &[],
                  &mut [])
            .map(|c| c.point)
    }

    pub fn custom<'b>(&self, item: Item, flavor: FlavorType, output: &'b mut [u8])
                      -> TakeFuture<'a, 'b>
    {
        TakeFuture { state: TakeState::new(self.route, self.window, item, flavor),
                     output }
    }

    pub async fn text<'b>(&self, item: Item, output: &'b mut [u8]) -> Result<&'b str>
    {
        let n = self.custom(item, TEXT, output).await?;
        core::str::from_utf8(&output[..n]).map_err(|_| DragError::Unknown(mac_abi::BAD_ABI))
    }

    /// Owns its bounded result buffer so an event-driven receiver can retain
    /// the pending take alongside its payload without a self-reference or heap.
    pub fn file(&self, item: Item) -> FileTakeFuture<'a>
    {
        FileTakeFuture { state: TakeState::new(self.route, self.window, item, FILE_REFERENCE),
                         bytes: [0; wire::FILE_ITEM_BYTES] }
    }

    /// True after Escape/source cancellation while this receiver drains storage.
    /// The System keeps the session dropping until its receiver acknowledges.
    pub fn cancellation_requested(&self) -> Result<bool>
    {
        self.route
            .call(wire::Call { operation: wire::INFO,
                               ..Default::default() },
                  &[],
                  &mut [])
            .map(|call| call.info.phase == wire::DROPPING && call.info.outcome == wire::CANCELLED)
    }

    /// Request cancellation without relinquishing the receiver's ownership.
    /// Keep draining storage and complete only after undo is acknowledged.
    pub fn request_cancel(&self) -> Result<()>
    {
        self.route
            .call(wire::Call { operation: wire::CANCEL_RECEIVE,
                               window: self.window.map_or(0, |w| w.0),
                               ..Default::default() },
                  &[],
                  &mut [])
            .map(|_| ())
    }

    /// Finish a bounded event-driven receive after storage commits or rollback
    /// is acknowledged. Like receive(), completion consumes the sole owner.
    pub fn complete(mut self, result: Result<()>) -> Result<()>
    {
        self.acknowledge(result)?;
        result
    }

    /// Complete receive work once. Rejection and dropped futures undo resident
    /// feedback; storage clients still own rollback of their staged outputs.
    pub fn receive(mut self, work: impl Future<Output = Result<()>>)
                   -> impl Future<Output = Result<()>>
    {
        // Ownership enters the future at construction, before its first poll.
        async move {
            let result = work.await;
            self.acknowledge(result)?;
            result
        }
    }

    fn acknowledge(&mut self, result: Result<()>) -> Result<()>
    {
        self.route.call(wire::Call { operation: wire::ACKNOWLEDGE,
                                      window: self.window.map_or(0, |w| w.0),
                                      flags: u32::from(result.is_ok()),
                                      error: result.err().map_or(0, |e| {
                                                             match e
                                                             {
                                                                 DragError::Unknown(s) => s,
                                                                 DragError::Cancelled =>
                                                                 {
                                                                     mac_abi::OPERATION_CANCELLED
                                                                 }
                                                                 _ => mac_abi::IO_ERROR
                                                             }
                                                         }),
                                      ..Default::default() },
                         &[],
                         &mut [])?;
        self.completed = true;
        Ok(())
    }
}
impl Drop for DropPayload<'_>
{
    fn drop(&mut self)
    {
        if !self.completed
        {
            let _ = self.route.call(wire::Call { operation: wire::ABANDON,
                                                 index: wire::DROPPING,
                                                 window: self.window.map_or(0, |w| w.0),
                                                 ..Default::default() },
                                    &[],
                                    &mut []);
        }
    }
}
/// Borrowed output for arbitrary flavors; its state never borrows the payload.
#[must_use = "takes make progress only when polled by an executor or event loop"]
pub struct TakeFuture<'a, 'b>
{
    state:  TakeState<'a>,
    output: &'b mut [u8]
}
impl Future for TakeFuture<'_, '_>
{
    type Output = Result<usize>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output>
    {
        let this = self.get_mut();
        this.state.poll(context, this.output)
    }
}
/// Allocation-free owned file-reference take, compatible with std executors and
/// the app's ordinary event loop. Dropping it removes its wake registration.
#[must_use = "takes make progress only when polled by an executor or event loop"]
pub struct FileTakeFuture<'a>
{
    state: TakeState<'a>,
    bytes: [u8; wire::FILE_ITEM_BYTES]
}
impl Future for FileTakeFuture<'_>
{
    type Output = Result<FileItem>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output>
    {
        let this = self.get_mut();
        match this.state.poll(context, &mut this.bytes)
        {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) =>
            {
                Poll::Ready(result.and_then(|n| FileItem::decode(&this.bytes[..n])))
            }
        }
    }
}
struct TakeState<'a>
{
    route:      Route<'a>,
    window:     Option<WindowId>,
    item:       Item,
    flavor:     FlavorType,
    done:       bool,
    requested:  bool,
    registered: bool
}
impl<'a> TakeState<'a>
{
    fn new(route: Route<'a>, window: Option<WindowId>, item: Item, flavor: FlavorType) -> Self
    {
        Self { route,
               window,
               item,
               flavor,
               done: false,
               requested: false,
               registered: false }
    }

    fn poll(&mut self, context: &mut Context<'_>, output: &mut [u8]) -> Poll<Result<usize>>
    {
        if self.done
        {
            return Poll::Ready(Err(DragError::FlavorUnavailable { flavor: self.flavor }));
        }
        let mut call = wire::Call { operation: wire::READ,
                                    flags: u32::from(self.requested),
                                    id: self.route.id,
                                    window: self.window.map_or(0, |w| w.0),
                                    item: self.item.0,
                                    kind: self.flavor.0,
                                    ..Default::default() };
        match self.route.client.drag(&mut call, &[], output)
        {
            Err(ToolboxError::ServiceBusy) =>
            {
                self.requested = true;
                match super::wake::register(self.route.id,
                                            self.item.0,
                                            self.flavor,
                                            context.waker())
                {
                    Ok(()) =>
                    {
                        self.registered = true;
                        Poll::Pending
                    }
                    Err(error) =>
                    {
                        self.done = true;
                        Poll::Ready(Err(error))
                    }
                }
            }
            result =>
            {
                if self.registered
                {
                    super::wake::remove(self.route.id, self.item.0, self.flavor);
                    self.registered = false;
                }
                self.done = true;
                Poll::Ready(match result {
                    Ok(()) if call.transferred <= output.len() => Ok(call.transferred),
                    Ok(()) => Err(DragError::Unknown(mac_abi::BAD_ABI)),
                Err(error) if error.status() == wire::FLAVOR_UNAVAILABLE => Err(DragError::FlavorUnavailable { flavor: self.flavor }),
                Err(error) if error.status() == wire::PROMISE_FAILED => Err(DragError::PromiseFailed { flavor: self.flavor, source: ToolboxError::from_status(call.error) }),
                Err(error) => Err(error.into())
            })
            }
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum DragEvent<'a>
{
    Tracking(TrackingOffer<'a>),
    Dropped(DropPayload<'a>),
    Ended
    {
        outcome: DragOutcome
    },
    /// Wake the SDK source owner; a producer is never called in a foreign app.
    DataRequested,
    DataReady,
    /// Receiver must cancel/undo storage, then complete its payload. A committed
    /// racing operation still reports its actual outcome, never a false undo.
    CancelRequested
}
impl<'a> DragEvent<'a>
{
    #[doc(hidden)]
    pub fn decode(client: Client<'a>, event: mac_abi::Event) -> Option<Self>
    {
        let route = Route { client,
                            id: event.payload as u32 };
        let window = WindowId(event.window);
        let window = (window.0 != 0).then_some(window);
        let at = event.where_;
        Some(match event.message & 0xff00_0000
             {
                 wire::ENTERED_EVENT | wire::WITHIN_EVENT | wire::LEFT_EVENT =>
                 {
                     Self::Tracking(TrackingOffer { route,
                                                    responded: false,
                                                    window,
                                                    at,
                                                    summary: ((event.message & 0xfff) as usize,
                                                              (event.message >> 12) & 0xff),
                                                    phase: match event.message & 0xff00_0000
                                                    {
                                                        wire::ENTERED_EVENT => TrackPhase::Entered,
                                                        wire::WITHIN_EVENT => TrackPhase::Within,
                                                        _ => TrackPhase::Left
                                                    } })
                 }
                 wire::DROP_EVENT =>
                 {
                     Self::Dropped(DropPayload { route,
                                                 completed: false,
                                                 window,
                                                 at,
                                                 action: if event.message & 0x00ff_ffff
                                                            == wire::COPY
                                                 {
                                                     DropAction::Copy
                                                 }
                                                 else
                                                 {
                                                     DropAction::Move
                                                 } })
                 }
                 wire::PROMISE_EVENT => Self::DataRequested,
                 wire::DATA_READY_EVENT =>
                 {
                     super::wake::notify(route.id);
                     Self::DataReady
                 }
                 wire::CANCEL_REQUESTED_EVENT =>
                 {
                     super::wake::notify(route.id);
                     Self::CancelRequested
                 }
                 wire::FINISHED_EVENT =>
                 {
                     Self::Ended { outcome: match event.message & 0x00ff_ffff
                                   {
                                       wire::COPIED => DragOutcome::Copied,
                                       wire::MOVED | wire::CONSUMED => DragOutcome::Moved,
                                       wire::REJECTED => DragOutcome::Rejected,
                                       _ => DragOutcome::Cancelled
                                   } }
                 }
                 _ => return None
             })
    }

    pub const fn window(&self) -> Option<WindowId>
    {
        match self
        {
            Self::Tracking(o) => o.window,
            Self::Dropped(d) => d.window,
            _ => None
        }
    }
}

impl DragEvent<'_>
{
    /// Explicit event transport conversion; ordinary clients use the enum.
    #[doc(hidden)]
    pub fn as_record(&self, when: u32, modifiers: u16) -> mac_abi::Event
    {
        let (message, id, window, at) = match self
        {
            Self::Tracking(o) =>
            {
                (match o.phase
                 {
                     TrackPhase::Entered => wire::ENTERED_EVENT,
                     TrackPhase::Within => wire::WITHIN_EVENT,
                     TrackPhase::Left => wire::LEFT_EVENT
                 }
                 | o.summary.0 as u32
                 | (o.summary.1 << 12),
                 o.route.id,
                 o.window.map_or(0, |w| w.0),
                 o.at)
            }
            Self::Dropped(d) =>
            {
                (wire::DROP_EVENT
                 | match d.action
                 {
                     DropAction::Copy => wire::COPY,
                     DropAction::Move => wire::MOVE
                 },
                 d.route.id,
                 d.window.map_or(0, |w| w.0),
                 d.at)
            }
            Self::Ended { outcome } =>
            {
                (wire::FINISHED_EVENT
                 | match outcome
                 {
                     DragOutcome::Copied => wire::COPIED,
                     DragOutcome::Moved => wire::MOVED,
                     DragOutcome::Rejected => wire::REJECTED,
                     DragOutcome::Cancelled => wire::CANCELLED
                 },
                 0,
                 0,
                 Point::default())
            }
            Self::DataRequested => (wire::PROMISE_EVENT, 0, 0, Point::default()),
            Self::DataReady => (wire::DATA_READY_EVENT, 0, 0, Point::default()),
            Self::CancelRequested => (wire::CANCEL_REQUESTED_EVENT, 0, 0, Point::default())
        };
        mac_abi::Event { kind: mac_abi::EventKind::OsEvent,
                         message,
                         payload: u64::from(id),
                         where_: at,
                         when,
                         modifiers,
                         window }
    }
}

impl Drop for TakeState<'_>
{
    fn drop(&mut self)
    {
        if self.registered
        {
            super::wake::remove(self.route.id, self.item.0, self.flavor);
        }
    }
}

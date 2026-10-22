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
    Content
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Response
{
    Reject,
    Accept
    {
        target:    Target,
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
/// Tracking carries only metadata and a response capability: no take methods.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackingOffer<'a>
{
    route:     Route<'a>,
    window:    Option<WindowId>,
    at:        Point,
    summary:   (usize, u32),
    pub phase: TrackPhase
}
impl<'a> TrackingOffer<'a>
{
    pub const fn flavors(self) -> Flavors<'a>
    {
        Flavors { route:   self.route,
                  summary: Some(self.summary) }
    }

    pub const fn window(self) -> Option<WindowId>
    {
        self.window
    }

    pub const fn position(self) -> Point
    {
        self.at
    }

    pub fn respond(self, response: Response) -> Result<()>
    {
        let (flags, volume, parent, bounds) = match response
        {
            Response::Reject => (0, 0, 0, Rect::default()),
            Response::Accept { target,
                               highlight } =>
            {
                match target
                {
                    Target::Folder { volume,
                                     id } => (1, volume.0, id.0, highlight),
                    Target::Trash { volume,
                                    id } => (1 | wire::TARGET_TRASH, volume.0, id.0, highlight),
                    Target::Content => (1, 0, 0, highlight)
                }
            }
        };
        self.route
            .call(wire::Call { operation: wire::ACCEPT,
                               point: self.at,
                               window: self.window.map_or(0, |w| w.0),
                               flags,
                               volume,
                               parent,
                               bounds,
                               ..Default::default() },
                  &[],
                  &mut [])
            .map(|_| ())
    }
}
/// A borrowed drop capability. Resident validation enforces take-once even if
/// an application copies this view, duplicates an event or polls a future again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DropPayload<'a>
{
    route:      Route<'a>,
    window:     Option<WindowId>,
    at:         Point,
    pub action: DropAction
}
impl<'a> DropPayload<'a>
{
    pub const fn flavors(self) -> Flavors<'a>
    {
        Flavors { route:   self.route,
                  summary: None }
    }

    pub const fn window(self) -> Option<WindowId>
    {
        self.window
    }

    pub const fn position(self) -> Point
    {
        self.at
    }

    pub fn origin(self) -> Result<Point>
    {
        self.route
            .call(wire::Call { operation: wire::INFO,
                               ..Default::default() },
                  &[],
                  &mut [])
            .map(|c| c.point)
    }

    pub fn custom<'b>(self, item: Item, flavor: FlavorType, output: &'b mut [u8])
                      -> TakeFuture<'a, 'b>
    {
        TakeFuture { drop: self,
                     item,
                     flavor,
                     output,
                     done: false,
                     requested: false,
                     registered: false }
    }

    pub async fn text<'b>(self, item: Item, output: &'b mut [u8]) -> Result<&'b str>
    {
        let n = self.custom(item, TEXT, output).await?;
        core::str::from_utf8(&output[..n]).map_err(|_| DragError::Unknown(mac_abi::BAD_ABI))
    }

    pub async fn file(self, item: Item) -> Result<FileItem>
    {
        let mut bytes = [0; wire::FILE_ITEM_BYTES];
        let n = self.custom(item, FILE_REFERENCE, &mut bytes).await?;
        FileItem::decode(&bytes[..n])
    }

    /// Complete receive work once. Rejection and dropped futures undo resident
    /// feedback; storage clients still own rollback of their staged outputs.
    pub async fn receive(self, work: impl Future<Output = Result<()>>) -> Result<()>
    {
        let mut guard = Completion { payload: self,
                                     done:    false };
        let result = work.await;
        self.acknowledge(result)?;
        guard.done = true;
        result
    }

    fn acknowledge(self, result: Result<()>) -> Result<()>
    {
        self.route
            .call(wire::Call { operation: wire::ACKNOWLEDGE,
                               window: self.window.map_or(0, |w| w.0),
                               flags: u32::from(result.is_ok()),
                               error: result.err().map_or(0, |e| {
                                                      match e
                                                      {
                                                          DragError::Unknown(s) => s,
                                                          _ => mac_abi::IO_ERROR
                                                      }
                                                  }),
                               ..Default::default() },
                  &[],
                  &mut [])
            .map(|_| ())
    }
}
struct Completion<'a>
{
    payload: DropPayload<'a>,
    done:    bool
}
impl Drop for Completion<'_>
{
    fn drop(&mut self)
    {
        if !self.done
        {
            let _ = self.payload.acknowledge(Err(DragError::Unknown(mac_abi::IO_ERROR)));
        }
    }
}
pub struct TakeFuture<'a, 'b>
{
    drop:       DropPayload<'a>,
    item:       Item,
    flavor:     FlavorType,
    output:     &'b mut [u8],
    done:       bool,
    requested:  bool,
    registered: bool
}
impl Future for TakeFuture<'_, '_>
{
    type Output = Result<usize>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output>
    {
        let this = self.get_mut();
        if this.done
        {
            return Poll::Ready(Err(DragError::FlavorUnavailable { flavor: this.flavor }));
        }
        let mut call = wire::Call { operation: wire::READ,
                                    flags: u32::from(this.requested),
                                    id: this.drop.route.id,
                                    window: this.drop.window.map_or(0, |w| w.0),
                                    item: this.item.0,
                                    kind: this.flavor.0,
                                    ..Default::default() };
        match this.drop.route.client.drag(&mut call, &[], this.output)
        {
            Err(ToolboxError::ServiceBusy) =>
            {
                this.requested = true;
                match super::wake::register(this.drop.route.id,
                                            this.item.0,
                                            this.flavor,
                                            context.waker())
                {
                    Ok(()) =>
                    {
                        this.registered = true;
                        Poll::Pending
                    }
                    Err(error) =>
                    {
                        this.done = true;
                        Poll::Ready(Err(error))
                    }
                }
            }
            result =>
            {
                if this.registered
                {
                    super::wake::remove(this.drop.route.id, this.item.0, this.flavor);
                    this.registered = false;
                }
                this.done = true;
                Poll::Ready(match result {
                    Ok(()) if call.transferred <= this.output.len() => Ok(call.transferred),
                    Ok(()) => Err(DragError::Unknown(mac_abi::BAD_ABI)),
                Err(error) if error.status() == wire::FLAVOR_UNAVAILABLE => Err(DragError::FlavorUnavailable { flavor: this.flavor }),
                Err(error) if error.status() == wire::PROMISE_FAILED => Err(DragError::PromiseFailed { flavor: this.flavor, source: ToolboxError::from_status(call.error) }),
                Err(error) => Err(error.into())
            })
            }
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    DataReady
}
impl<'a> DragEvent<'a>
{
    #[doc(hidden)]
    pub fn decode(client: Client<'a>, event: mac_abi::Event) -> Option<Self>
    {
        let route = Route { client,
                            id: event.payload as u32 };
        let window = WindowId((event.payload >> 32) as u32);
        let window = (window.0 != 0).then_some(window);
        let at = event.where_;
        Some(match event.message & 0xff00_0000
             {
                 wire::ENTERED_EVENT | wire::WITHIN_EVENT | wire::LEFT_EVENT =>
                 {
                     Self::Tracking(TrackingOffer { route,
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

    pub const fn window(self) -> Option<WindowId>
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
    pub fn into_record(self, when: u32, modifiers: u16) -> mac_abi::Event
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
            Self::DataReady => (wire::DATA_READY_EVENT, 0, 0, Point::default())
        };
        mac_abi::Event { kind: mac_abi::EventKind::OsEvent,
                         message,
                         payload: u64::from(id) | (u64::from(window) << 32),
                         where_: at,
                         when,
                         modifiers }
    }
}

impl Drop for TakeFuture<'_, '_>
{
    fn drop(&mut self)
    {
        if self.registered
        {
            super::wake::remove(self.drop.route.id, self.item.0, self.flavor);
        }
    }
}

//! System-owned dragging: one RAII source, borrowed offers and drop-only data.
#![no_std]
#![forbid(unsafe_code)]
use mac_abi::{Point, Rect, WindowId, drag as wire};
use mac_traps::{Client, Cnid, Error as ToolboxError, VolumeId};
mod promise;
mod wake;
pub use promise::{NoPromises, Promises, WithPromise};
mod offer;
pub use offer::{DragEvent, DragOutcome, DropAction, DropPayload, FlavorInfo, Flavors, Item,
                Response, TakeFuture, Target, TrackPhase, TrackingOffer};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlavorType([u8; 4]);
impl FlavorType
{
    pub const fn new(code: [u8; 4]) -> Self
    {
        Self(code)
    }

    pub const fn bytes(self) -> [u8; 4]
    {
        self.0
    }
}
pub const TEXT: FlavorType = FlavorType::new(*b"TEXT");
pub const FILE_REFERENCE: FlavorType = FlavorType::new(*b"fref");
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragError
{
    NoSession,
    FlavorUnavailable
    {
        flavor: FlavorType
    },
    PromiseFailed
    {
        flavor: FlavorType,
        source: ToolboxError
    },
    WrongThread,
    Unknown(mac_abi::Status)
}
pub type Result<T> = core::result::Result<T, DragError>;
impl From<ToolboxError> for DragError
{
    fn from(error: ToolboxError) -> Self
    {
        match error
        {
            ToolboxError::NotFound => Self::NoSession,
            ToolboxError::WrongThread => Self::WrongThread,
            other => Self::Unknown(other.status())
        }
    }
}
#[derive(Clone, Copy)]
pub struct Drags<'a>
{
    client: Client<'a>
}
impl<'a> Drags<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client }
    }

    pub fn builder(self, source: Option<WindowId>, anchor: Point, outline: Rect)
                   -> Result<Drag<'a>>
    {
        Drag::new(self.client, source, anchor, outline)
    }
}
/// Known data types keep their flavor contract next to their encoding.
pub trait FlavorData: Send + Sync
{
    const TYPE: FlavorType;
    fn bytes(&self) -> &[u8];
}
pub struct Text<'a>(&'a str);
impl<'a> Text<'a>
{
    pub const fn new(text: &'a str) -> Self
    {
        Self(text)
    }
}
impl FlavorData for Text<'_>
{
    const TYPE: FlavorType = TEXT;

    fn bytes(&self) -> &[u8]
    {
        self.0.as_bytes()
    }
}
pub struct Flavor<'a>
{
    kind:  FlavorType,
    bytes: &'a [u8]
}
impl<'a> Flavor<'a>
{
    pub const fn text(text: &'a str) -> Self
    {
        Self { kind:  TEXT,
               bytes: text.as_bytes() }
    }

    pub const fn custom(kind: FlavorType, bytes: &'a [u8]) -> Self
    {
        Self { kind,
               bytes }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileItem
{
    pub volume: VolumeId,
    pub id:     Cnid,
    /// Parent captured with the source snapshot; moves validate it atomically.
    pub parent: Cnid,
    pub origin: Point,
    pub bounds: Rect
}
pub const fn file_item(volume: VolumeId, id: Cnid, parent: Cnid, origin: Point, bounds: Rect)
                       -> FileItem
{
    FileItem { volume,
               id,
               parent,
               origin,
               bounds }
}
impl FileItem
{
    fn encode(self) -> [u8; wire::FILE_ITEM_BYTES]
    {
        let mut bytes = [0; wire::FILE_ITEM_BYTES];
        bytes[..4].copy_from_slice(&self.volume.0.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.id.0.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.parent.0.to_le_bytes());
        bytes[12..14].copy_from_slice(&self.origin.h.to_le_bytes());
        bytes[14..16].copy_from_slice(&self.origin.v.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.bounds.to_be_bytes());
        bytes
    }

    fn decode(bytes: &[u8]) -> Result<Self>
    {
        if bytes.len() != wire::FILE_ITEM_BYTES
        {
            return Err(DragError::Unknown(mac_abi::BAD_ABI));
        }
        Ok(Self { volume: VolumeId(u32::from_le_bytes(bytes[..4].try_into().unwrap())),
                  id:     Cnid(u32::from_le_bytes(bytes[4..8].try_into().unwrap())),
                  parent: Cnid(u32::from_le_bytes(bytes[8..12].try_into().unwrap())),
                  origin: Point { h: i16::from_le_bytes(bytes[12..14].try_into().unwrap()),
                                  v: i16::from_le_bytes(bytes[14..16].try_into().unwrap()) },
                  bounds: Rect::from_be_bytes(bytes[16..24].try_into().unwrap()) })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Route<'a>
{
    client: Client<'a>,
    id:     u32
}
impl Route<'_>
{
    fn call(self, mut call: wire::Call, input: &[u8], output: &mut [u8]) -> Result<wire::Call>
    {
        call.id = self.id;
        self.client.drag(&mut call, input, output).map_err(DragError::from)?;
        Ok(call)
    }
}
/// A draft owns resident offers, but cannot track until consumed by `begin`.
pub struct Drag<'a, P: Promises = NoPromises>
{
    route:    Route<'a>,
    item:     u32,
    anchor:   Point,
    outline:  Rect,
    promises: P,
    armed:    bool
}
impl<'a> Drag<'a>
{
    pub fn new(client: Client<'a>, source: Option<WindowId>, anchor: Point, outline: Rect)
               -> Result<Self>
    {
        let mut call = wire::Call { operation: wire::CREATE,
                                    window: source.map_or(0, |w| w.0),
                                    ..Default::default() };
        client.drag(&mut call, &[], &mut []).map_err(DragError::from)?;
        Ok(Self { route: Route { client,
                                 id: call.id },
                  item: 0,
                  anchor,
                  outline,
                  promises: NoPromises,
                  armed: true })
    }
}
impl<'a, P: Promises> Drag<'a, P>
{
    /// Begin a generic item; flavors below attach to this item.
    pub fn empty_item(&mut self, bounds: Rect) -> Result<()>
    {
        let item = self.item.checked_add(1).ok_or(DragError::Unknown(mac_abi::NO_MEMORY))?;
        self.route.call(wire::Call { operation: wire::ADD_ITEM,
                                      item,
                                      bounds,
                                      ..Default::default() },
                         &[],
                         &mut [])?;
        self.item = item;
        Ok(())
    }

    pub fn item(&mut self, file: FileItem) -> Result<()>
    {
        self.empty_item(file.bounds)?;
        self.flavor(Flavor::custom(FILE_REFERENCE, &file.encode()))
    }

    pub fn items(&mut self, items: impl IntoIterator<Item = FileItem>) -> Result<()>
    {
        let mut bytes = [0u8; wire::FILE_ITEM_BYTES * wire::MAX_ITEMS];
        let mut count = 0;
        for item in items
        {
            if count == wire::MAX_ITEMS
            {
                return Err(DragError::Unknown(mac_abi::NO_MEMORY));
            }
            bytes[count * wire::FILE_ITEM_BYTES..(count + 1) * wire::FILE_ITEM_BYTES].copy_from_slice(&item.encode());
            count += 1;
        }
        self.route.call(wire::Call { operation: wire::FILE_ITEMS,
                                      ..Default::default() },
                         &bytes[..count * wire::FILE_ITEM_BYTES],
                         &mut [])?;
        self.item = count as u32;
        Ok(())
    }

    pub fn flavor(&mut self, flavor: Flavor<'_>) -> Result<()>
    {
        self.route
            .call(wire::Call { operation: wire::OFFER,
                               item: self.item,
                               kind: flavor.kind.0,
                               ..Default::default() },
                  flavor.bytes,
                  &mut [])
            .map(|_| ())
    }

    /// Consuming chaining keeps closures owned and Send + Sync without requiring
    /// a global allocator, callback registry or leaked/static captures.
    pub fn promise<T: FlavorData,
                       F: FnOnce() -> core::result::Result<T, ToolboxError> + Send + Sync>(
        mut self, producer: F)
        -> Result<Drag<'a, WithPromise<P, F, T>>>
    {
        self.route.call(wire::Call { operation: wire::OFFER,
                                      item: self.item,
                                      kind: T::TYPE.0,
                                      flags: wire::PROMISED,
                                      ..Default::default() },
                         &[],
                         &mut [])?;
        self.armed = false;
        let promises = WithPromise::new(core::mem::replace(&mut self.promises, P::empty()),
                                        self.item,
                                        producer);
        Ok(Drag { route: self.route,
                  item: self.item,
                  anchor: self.anchor,
                  outline: self.outline,
                  promises,
                  armed: true })
    }

    pub fn begin(mut self) -> Result<DragSession<'a, P>>
    {
        self.route.call(wire::Call { operation: wire::BEGIN,
                                      point: self.anchor,
                                      bounds: self.outline,
                                      ..Default::default() },
                         &[],
                         &mut [])?;
        self.armed = false;
        Ok(DragSession { route:    self.route,
                         promises: core::mem::replace(&mut self.promises, P::empty()) })
    }
}
impl<P: Promises> Drop for Drag<'_, P>
{
    fn drop(&mut self)
    {
        if self.armed
        {
            let _ = self.route.call(wire::Call { operation: wire::DISPOSE,
                                                 ..Default::default() },
                                    &[],
                                    &mut []);
        }
    }
}
/// Sole source owner. Dropping a live session requests resident zoom-back and
/// exactly one terminal event; neither app callbacks nor UI pixels live here.
#[must_use = "dropping the source owner cancels the drag"]
pub struct DragSession<'a, P: Promises = NoPromises>
{
    route:    Route<'a>,
    promises: P
}
impl<P: Promises> DragSession<'_, P>
{
    pub fn was_click(&self) -> Result<bool>
    {
        self.route
            .call(wire::Call { operation: wire::INFO,
                               ..Default::default() },
                  &[],
                  &mut [])
            .map(|c| c.info.outcome == wire::CLICKED)
    }

    pub fn did_start_tracking(&self) -> Result<bool>
    {
        self.route
            .call(wire::Call { operation: wire::INFO,
                               ..Default::default() },
                  &[],
                  &mut [])
            .map(|c| c.index != 0)
    }

    pub fn cancel(&self) -> Result<()>
    {
        self.route
            .call(wire::Call { operation: wire::CANCEL,
                               ..Default::default() },
                  &[],
                  &mut [])
            .map(|_| ())
    }

    /// Called by the SDK event pump, on this source's owning OS thread.
    pub fn service_promises(&mut self) -> Result<bool>
    {
        let info = match self.route.call(wire::Call { operation: wire::INFO,
                                                      ..Default::default() },
                                         &[],
                                         &mut [])
        {
            Ok(c) => c.info,
            Err(DragError::NoSession) => return Ok(false),
            Err(e) => return Err(e)
        };
        if info.phase != wire::DROPPING
        {
            return Ok(false);
        }
        let call = match self.route.call(wire::Call { operation: wire::NEXT_PROMISE,
                                                      ..Default::default() },
                                         &[],
                                         &mut [])
        {
            Ok(call) => call,
            Err(DragError::NoSession) => return Ok(false),
            other => other?
        };
        let mut request = promise::PromiseRequest { route:    self.route,
                                                    item:     call.item,
                                                    kind:     FlavorType(call.kind),
                                                    supplied: false };
        self.promises.supply(&mut request)?;
        if !request.supplied
        {
            request.fail(ToolboxError::NotFound)?;
        }
        Ok(true)
    }
}
impl<P: Promises> Drop for DragSession<'_, P>
{
    fn drop(&mut self)
    {
        let _ = self.route.call(wire::Call { operation: wire::DISPOSE,
                                             ..Default::default() },
                                &[],
                                &mut []);
    }
}

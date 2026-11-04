//! Explicit family snapshots and checked owned drawing results. All padding is
//! encoded through the same production field codec as ordinary user requests.
use alloc::{sync::Arc, vec::Vec};
use core::mem::size_of;

use mac_abi::{BAD_ABI, BAD_ARGUMENT, Header, NO_MEMORY, SERVICE_BUSY, Status,
              controls::{ControlChange, ScrollbarState, reflow::*},
              definitions::*,
              geometry::{Point, Rect}};

use crate::{machine::traps::user::{OwnedBytes, wire},
            toolbox::quickdraw::Canvas};

wire::wire_struct!(CallbackRange { offset: u64,
                                   length: u64 });
wire::wire_struct!(CallbackPacket { header:         Header,
                                    entry:          u64,
                                    kind:           u32,
                                    id:             i16,
                                    variation:      i16,
                                    message:        i16,
                                    reserved:       u16,
                                    reflow:         u32,
                                    param:          i64,
                                    argument_kind:  u32,
                                    flags:          u32,
                                    object:         CallbackRange,
                                    arguments:      CallbackRange,
                                    auxiliary:      [CallbackRange; 2],
                                    pixels:         CallbackRange,
                                    surface_width:  u32,
                                    surface_height: u32,
                                    surface_origin: Point,
                                    surface_clip:   Rect,
                                    reserved2:      u32 });
wire::wire_struct!(WindowDefinitionState { content:               Rect,
                                           structure:             Rect,
                                           close_box:             Rect,
                                           grow_box:              Rect,
                                           active:                u32,
                                           close_pressed:         u32,
                                           title:                 [u8; mac_abi::WINDOW_TITLE_BYTES],
                                           title_length:          u32,
                                           zoomed:                u32,
                                           zoom_box:              Rect,
                                           zoom_pressed:          u32,
                                           structure_piece_count: u32,
                                           structure_pieces:      [Rect; 2] });
wire::wire_struct!(ControlChange { before: ControlDefinitionState,
                                   after:  ControlDefinitionState,
                                   damage: [Rect; 2] });
wire::wire_struct!(GridTrack { bounds:       SizeBounds,
                               gap:          i64,
                               assigned_gap: i64,
                               manual:       u32,
                               reserved:     u32,
                               requested:    i64,
                               minimum:      i64,
                               maximum:      i64,
                               preferred:    i64,
                               assigned:     i64,
                               position:     i64 });
wire::wire_struct!(GridItem { start:     u32,
                              count:     u32,
                              minimum:   i64,
                              preferred: i64,
                              anchors:   u32,
                              reserved:  u32,
                              position:  i64,
                              extent:    i64 });
wire::wire_struct!(GridAxisCall {
    constraint: AxisConstraint, percent_basis: i64, tracks: *mut GridTrack,
    track_count: usize, items: *mut GridItem, item_count: usize, minimum: i64, preferred: i64
});
wire::wire_struct!(GridPaint { cells: *const Rect, count: usize });

pub(crate) enum Object
{
    Opaque
    {
        owner:    mac_object_table::domain::DomainId,
        image_id: u64,
        object:   usize,
        surface:  usize
    },
    Window(WindowDefinitionState),
    Control(ControlDefinitionState),
    Scrollbar(ScrollbarState),
    Change(ControlChange)
}
#[derive(Clone, Copy)]
enum ObjectKind
{
    Opaque,
    Window,
    Control,
    Scrollbar,
    Change
}

pub(crate) enum Arguments
{
    Scalar(i64),
    Rect(Rect),
    Layout(Payload),
    GridAxis
    {
        constraint:    AxisConstraint,
        percent_basis: i64,
        tracks:        Vec<GridTrack>,
        items:         Vec<GridItem>
    },
    GridPaint(Vec<Rect>)
}

/// Geometry-only admission. The owning manager pins its real target/revision;
/// this value contains no framebuffer pointer or authority by itself.
#[derive(Clone, Copy)]
pub(crate) struct SurfacePlan
{
    width:         u32,
    height:        u32,
    origin:        Point,
    clip:          Rect,
    destination:   Rect,
    crop:          Point,
    parent_width:  usize,
    parent_height: usize,
    parent_origin: Point,
    parent_clip:   Rect
}
impl SurfacePlan
{
    /// Calculate a bounded native callback viewport without allocating. Its
    /// physical origin is eight-pixel aligned so pattern phase is unchanged.
    pub(crate) fn new(canvas: &Canvas<'_>, local: Rect) -> Result<Self, Status>
    {
        let origin = canvas.origin();
        let offset = |a: i16, b: i16| a.checked_add(b).ok_or(BAD_ARGUMENT);
        let target = Rect::new(offset(local.left, origin.h)?,
                               offset(local.top, origin.v)?,
                               offset(local.right, origin.h)?,
                               offset(local.bottom, origin.v)?);
        let clip = canvas.clip();
        let left = target.left.max(clip.left).max(0);
        let top = target.top.max(clip.top).max(0);
        let right = target.right
                          .min(clip.right)
                          .min(i16::try_from(canvas.width).map_err(|_| BAD_ARGUMENT)?);
        let bottom = target.bottom
                           .min(clip.bottom)
                           .min(i16::try_from(canvas.height).map_err(|_| BAD_ARGUMENT)?);
        if left >= right || top >= bottom
        {
            return Err(BAD_ARGUMENT);
        }
        let crop_left = left & !7;
        let crop_top = top & !7;
        let width = right - crop_left;
        let height = bottom - crop_top;
        Ok(Self { width:         width as u32,
                  height:        height as u32,
                  origin:        Point { h: origin.h.checked_sub(crop_left).ok_or(BAD_ARGUMENT)?,
                                         v: origin.v.checked_sub(crop_top).ok_or(BAD_ARGUMENT)? },
                  clip:          Rect::new(left - crop_left, top - crop_top, width, height),
                  destination:   Rect::new(left, top, right, bottom),
                  crop:          Point { h: crop_left,
                                         v: crop_top },
                  parent_width:  canvas.width,
                  parent_height: canvas.height,
                  parent_origin: origin,
                  parent_clip:   clip })
    }

    /// Initialize scratch storage outside Desktop/manager guards. Padding stays
    /// zero: no pixels outside the admitted target are exposed to the author.
    pub(crate) fn prepare(self) -> Result<PreparedSurface, (Status, Self)>
    {
        let count = match (self.width as usize).checked_mul(self.height as usize)
        {
            Some(count) => count,
            None => return Err((NO_MEMORY, self))
        };
        let mut pixels = Vec::new();
        if pixels.try_reserve_exact(count).is_err()
        {
            return Err((NO_MEMORY, self));
        }
        pixels.resize(count, 0);
        Ok(PreparedSurface { plan: self,
                             pixels })
    }
}

pub(crate) struct PreparedSurface
{
    plan:   SurfacePlan,
    pixels: Vec<u32>
}
impl PreparedSurface
{
    /// Capture the current original target without allocating. Refusal retains
    /// the same initialized scratch buffer for its owning staged operation.
    pub(crate) fn capture(mut self, canvas: &Canvas<'_>)
                          -> Result<SurfaceSnapshot, (Status, Self)>
    {
        let p = self.plan;
        if canvas.width != p.parent_width
           || canvas.height != p.parent_height
           || canvas.origin() != p.parent_origin
           || canvas.clip() != p.parent_clip
           || canvas.width
                    .checked_mul(canvas.height)
                    .is_none_or(|count| count > canvas.pixels.len())
        {
            return Err((SERVICE_BUSY, self));
        }
        let r = p.destination;
        let width = (r.right - r.left) as usize;
        for y in r.top as usize..r.bottom as usize
        {
            let source = y * canvas.width + r.left as usize;
            let target = (y - p.crop.v as usize) * p.width as usize + (r.left - p.crop.h) as usize;
            self.pixels[target..target + width].copy_from_slice(&canvas.pixels
                                                                    [source..source + width]);
        }
        Ok(SurfaceSnapshot { plan:   p,
                             pixels: self.pixels })
    }
}
pub(crate) struct SurfaceSnapshot
{
    plan:   SurfacePlan,
    pixels: Vec<u32>
}

pub(crate) struct Request
{
    pub(crate) kind:      u32,
    pub(crate) id:        i16,
    pub(crate) variation: i16,
    pub(crate) message:   i16,
    pub(crate) reflow:    u32,
    pub(crate) object:    Object,
    pub(crate) arguments: Arguments,
    pub(crate) surface:   Option<SurfaceSnapshot>
}
impl Request
{
    pub(crate) fn opaque_owner(&self) -> Option<(mac_object_table::domain::DomainId, u64)>
    {
        match self.object
        {
            Object::Opaque { owner,
                             image_id,
                             .. } => Some((owner, image_id)),
            _ => None
        }
    }

    pub(crate) fn kind(&self) -> u32
    {
        self.kind
    }

    pub(crate) fn id(&self) -> i16
    {
        self.id
    }

    pub(crate) fn prepare(self, entry: u64) -> Result<Packet, Status>
    {
        if let Object::Opaque { object,
                                surface,
                                .. } = &self.object
        {
            let Arguments::Scalar(param) = self.arguments
            else
            {
                return Err(BAD_ARGUMENT);
            };
            if self.surface.is_some()
            {
                return Err(BAD_ARGUMENT);
            }
            let layout = CallbackPacket { header: Header::for_type::<CallbackPacket>(),
                                          entry,
                                          kind: self.kind,
                                          id: self.id,
                                          variation: self.variation,
                                          message: self.message,
                                          reserved: 0,
                                          reflow: self.reflow,
                                          param,
                                          argument_kind: CALLBACK_OPAQUE,
                                          flags: 0,
                                          object: CallbackRange { offset: *object as u64,
                                                                  length: 0 },
                                          arguments: CallbackRange { offset: *surface as u64,
                                                                     length: 0 },
                                          auxiliary: [CallbackRange::default(); 2],
                                          pixels: CallbackRange::default(),
                                          surface_width: 0,
                                          surface_height: 0,
                                          surface_origin: Point::default(),
                                          surface_clip: Rect::ZERO,
                                          reserved2: 0 };
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(size_of::<CallbackPacket>()).map_err(|_| NO_MEMORY)?;
            bytes.resize(size_of::<CallbackPacket>(), 0);
            wire::encode_value(&layout, &mut bytes)?;
            return Ok(Packet { bytes,
                               layout,
                               object: ObjectKind::Opaque,
                               surface: None });
        }
        let kind = match &self.object
        {
            Object::Opaque { .. } => return Err(BAD_ARGUMENT),
            Object::Window(_) if self.kind == WDEF => ObjectKind::Window,
            Object::Control(_) if self.kind == CDEF => ObjectKind::Control,
            Object::Scrollbar(_) if self.kind == CDEF => ObjectKind::Scrollbar,
            Object::Change(_) if self.kind == CDEF => ObjectKind::Change,
            _ => return Err(BAD_ARGUMENT)
        };
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(size_of::<CallbackPacket>()).map_err(|_| NO_MEMORY)?;
        bytes.resize(size_of::<CallbackPacket>(), 0);
        let object = match &self.object
        {
            Object::Opaque { .. } => return Err(BAD_ARGUMENT),
            Object::Window(s) => append(&mut bytes, s)?,
            Object::Control(s) => append(&mut bytes, s)?,
            Object::Scrollbar(s) => append(&mut bytes, s)?,
            Object::Change(s) => append(&mut bytes, s)?
        };
        let mut packet = CallbackPacket { header: Header::for_type::<CallbackPacket>(),
                                          entry,
                                          kind: self.kind,
                                          id: self.id,
                                          variation: self.variation,
                                          message: self.message,
                                          reserved: 0,
                                          reflow: self.reflow,
                                          param: 0,
                                          argument_kind: CALLBACK_SCALAR,
                                          flags: 0,
                                          object,
                                          arguments: CallbackRange::default(),
                                          auxiliary: [CallbackRange::default(); 2],
                                          pixels: CallbackRange::default(),
                                          surface_width: 0,
                                          surface_height: 0,
                                          surface_origin: Point::default(),
                                          surface_clip: Rect::ZERO,
                                          reserved2: 0 };
        match self.arguments
        {
            Arguments::Scalar(value) => packet.param = value,
            Arguments::Rect(rect) =>
            {
                packet.argument_kind = CALLBACK_BYTES;
                packet.arguments = append(&mut bytes, &rect)?;
            }
            Arguments::Layout(payload) =>
            {
                if payload.message != self.message
                {
                    return Err(BAD_ARGUMENT);
                }
                packet.argument_kind = CALLBACK_BYTES;
                packet.arguments = append(&mut bytes, &payload)?;
            }
            Arguments::GridAxis { constraint,
                                  percent_basis,
                                  tracks,
                                  items } =>
            {
                packet.argument_kind = CALLBACK_GRID_AXIS;
                packet.arguments = append(&mut bytes,
                                          &GridAxisCall { constraint,
                                                          percent_basis,
                                                          tracks: core::ptr::null_mut(),
                                                          track_count: tracks.len(),
                                                          items: core::ptr::null_mut(),
                                                          item_count: items.len(),
                                                          minimum: 0,
                                                          preferred: 0 })?;
                packet.auxiliary[0] = append_slice(&mut bytes, &tracks)?;
                packet.auxiliary[1] = append_slice(&mut bytes, &items)?;
            }
            Arguments::GridPaint(cells) =>
            {
                packet.argument_kind = CALLBACK_GRID_PAINT;
                packet.arguments = append(&mut bytes,
                                          &GridPaint { cells: core::ptr::null(),
                                                       count: cells.len() })?;
                packet.auxiliary[0] = append_slice(&mut bytes, &cells)?;
            }
        }
        let surface = if let Some(surface) = self.surface
        {
            packet.flags = CALLBACK_SURFACE;
            packet.surface_width = surface.plan.width;
            packet.surface_height = surface.plan.height;
            packet.surface_origin = surface.plan.origin;
            packet.surface_clip = surface.plan.clip;
            packet.pixels = append_slice(&mut bytes, &surface.pixels)?;
            Some(SurfaceBinding { destination: surface.plan.destination,
                                  crop:        surface.plan.crop,
                                  width:       surface.plan.parent_width,
                                  height:      surface.plan.parent_height,
                                  origin:      surface.plan.parent_origin })
        }
        else
        {
            None
        };
        wire::encode_value(&packet, &mut bytes[..size_of::<CallbackPacket>()])?;
        Ok(Packet { bytes,
                    layout: packet,
                    object: kind,
                    surface })
    }
}

fn append<T: wire::Wire>(output: &mut Vec<u8>, value: &T) -> Result<CallbackRange, Status>
{
    let range = space(output, size_of::<T>())?;
    let checked = range.checked(output.len(), 8).ok_or(BAD_ABI)?;
    wire::encode_value(value, &mut output[checked])?;
    Ok(range)
}
fn append_slice<T: wire::Wire>(output: &mut Vec<u8>, values: &[T])
                               -> Result<CallbackRange, Status>
{
    let length = values.len().checked_mul(size_of::<T>()).ok_or(NO_MEMORY)?;
    let range = space(output, length)?;
    let start = usize::try_from(range.offset).map_err(|_| NO_MEMORY)?;
    for (i, value) in values.iter().enumerate()
    {
        let at = start + i * size_of::<T>();
        wire::encode_value(value, &mut output[at..at + size_of::<T>()])?;
    }
    Ok(range)
}
fn space(output: &mut Vec<u8>, length: usize) -> Result<CallbackRange, Status>
{
    let start = output.len().checked_add(7).map(|n| n & !7).ok_or(NO_MEMORY)?;
    let end = start.checked_add(length).ok_or(NO_MEMORY)?;
    if end > isize::MAX as usize
    {
        return Err(NO_MEMORY);
    }
    output.try_reserve_exact(end - output.len()).map_err(|_| NO_MEMORY)?;
    output.resize(end, 0);
    Ok(CallbackRange { offset: start as u64,
                       length: length as u64 })
}

#[derive(Clone, Copy)]
struct SurfaceBinding
{
    destination: Rect,
    crop:        Point,
    width:       usize,
    height:      usize,
    origin:      Point
}
pub(crate) struct Packet
{
    bytes:   Vec<u8>,
    layout:  CallbackPacket,
    object:  ObjectKind,
    surface: Option<SurfaceBinding>
}
impl Packet
{
    pub(crate) fn bytes(&self) -> &[u8]
    {
        &self.bytes
    }

    /// Seal a preadmitted scalar Window callback in its unique original carrier.
    /// Caller holds mutable Arc uniqueness; no Delivery or user reader exists.
    pub(super) fn replace_window(&mut self, state: &WindowDefinitionState) -> Result<(), Status>
    {
        if !matches!(self.object, ObjectKind::Window)
           || self.surface.is_some()
           || self.layout.flags != 0
           || self.layout.argument_kind != CALLBACK_SCALAR
           || self.layout.param != 0
           || self.layout.object.length != size_of::<WindowDefinitionState>() as u64
        {
            return Err(BAD_ARGUMENT);
        }
        text(&state.title, state.title_length)?;
        if state.structure_piece_count > state.structure_pieces.len() as u32
        {
            return Err(BAD_ARGUMENT);
        }
        let range = self.layout.object.checked(self.bytes.len(), 8).ok_or(BAD_ABI)?;
        // The fixed range length is validated before encoding any field.
        wire::encode_value(state, &mut self.bytes[range])
    }

    /// Refresh the actual fixed DISPOSE snapshot while the original Packet is
    /// uniquely held and has never been delivered. Every bound/shape check
    /// precedes mutation; no buffer or capacity changes here.
    pub(super) fn replace_control_disposal(&mut self, request: &Request) -> Result<(), Status>
    {
        if self.layout.kind != CDEF
           || self.layout.id != request.id
           || request.kind != CDEF
           || self.layout.message != DISPOSE
           || request.message != DISPOSE
           || self.surface.is_some()
           || request.surface.is_some()
           || self.layout.argument_kind != CALLBACK_SCALAR
           || self.layout.param != 0
           || !matches!(request.arguments, Arguments::Scalar(0))
           || request.reflow > 1
        {
            return Err(BAD_ARGUMENT);
        }
        let range = self.layout.object.checked(self.bytes.len(), 8).ok_or(BAD_ABI)?;
        let size = match (&request.object, self.object)
        {
            (Object::Control(state), ObjectKind::Control) =>
            {
                text(&state.title, state.title_length)?;
                size_of::<ControlDefinitionState>()
            }
            (Object::Scrollbar(_), ObjectKind::Scrollbar) => size_of::<ScrollbarState>(),
            _ => return Err(BAD_ARGUMENT)
        };
        if range.len() != size || self.bytes.len() < size_of::<CallbackPacket>()
        {
            return Err(BAD_ABI);
        }
        let mut header = self.layout;
        header.variation = request.variation;
        header.reflow = request.reflow;
        // The shared field encoder is fallible only on destination size, which
        // has been checked for both exact fixed ranges above.
        match &request.object
        {
            Object::Control(state) => wire::encode_value(state, &mut self.bytes[range])?,
            Object::Scrollbar(state) => wire::encode_value(state, &mut self.bytes[range])?,
            _ => return Err(BAD_ARGUMENT)
        }
        wire::encode_value(&header, &mut self.bytes[..size_of::<CallbackPacket>()])?;
        self.layout = header;
        Ok(())
    }

    pub(crate) fn reply(self: &Arc<Self>, bytes: OwnedBytes, result: i64)
                        -> Result<Reply, (Status, OwnedBytes)>
    {
        let admitted = (|| {
            if bytes.as_ref().len() != self.bytes.len()
               || bytes.as_ref()[..size_of::<CallbackPacket>()]
                  != self.bytes[..size_of::<CallbackPacket>()]
            {
                return Err(BAD_ABI);
            }
            self.validate_immutable_object(bytes.as_ref())?;
            let reply = Reply { bytes: &bytes,
                                layout: self.layout,
                                object: self.object,
                                surface: self.surface,
                                original: None,
                                custody: None,
                                result };
            reply.validate()?;
            Ok(())
        })();
        match admitted
        {
            Ok(()) =>
            {
                Ok(Reply { bytes,
                           layout: self.layout,
                           object: self.object,
                           surface: self.surface,
                           original: self.surface.map(|_| self.clone()),
                           custody: None,
                           result })
            }
            Err(status) => Err((status, bytes))
        }
    }

    fn validate_immutable_object(&self, bytes: &[u8]) -> Result<(), Status>
    {
        if !matches!(self.object, ObjectKind::Control | ObjectKind::Scrollbar | ObjectKind::Change)
        {
            return Ok(());
        }
        let range = self.layout.object.checked(self.bytes.len(), 8).ok_or(BAD_ABI)?;
        let original = &self.bytes[range.clone()];
        let returned = bytes.get(range).ok_or(BAD_ABI)?;
        if matches!(self.object, ObjectKind::Control | ObjectKind::Scrollbar)
        {
            // Family::decode lends immutable State views; no object fields are
            // outputs. Layout/grid argument buffers retain their declared role.
            if original != returned
            {
                return Err(BAD_ABI);
            }
        }
        else
        {
            let before: ControlChange = wire::decode_value(original)?;
            let after: ControlChange = wire::decode_value(returned)?;
            if before.before != after.before || before.after != after.after
            {
                return Err(BAD_ABI);
            }
            for damage in after.damage
            {
                if !damage.is_empty()
                   && (damage.left < before.after.bounds.left
                       || damage.top < before.after.bounds.top
                       || damage.right > before.after.bounds.right
                       || damage.bottom > before.after.bounds.bottom)
                {
                    return Err(BAD_ABI);
                }
            }
        }
        Ok(())
    }
}

/// Owned copied user result. No pointer in its payload is ever followed.
pub(crate) struct Reply<B = OwnedBytes>
{
    bytes:              B,
    layout:             CallbackPacket,
    object:             ObjectKind,
    surface:            Option<SurfaceBinding>,
    // Paint depends on the exact original pixels. Retain their existing packet
    // carrier after real RETURN, without retaining the execution gate pin.
    original:           Option<Arc<Packet>>,
    pub(crate) result:  i64,
    // LAST after owned bytes and original immutable paint/source packet.
    pub(super) custody: Option<Arc<dyn super::CallbackCustody>>
}
impl Reply<OwnedBytes>
{
    pub(crate) fn into_bytes(self) -> OwnedBytes
    {
        self.bytes
    }
}
impl<B: AsRef<[u8]>> Reply<B>
{
    fn range(&self, range: CallbackRange) -> Result<&[u8], Status>
    {
        let bytes = self.bytes.as_ref();
        Ok(&bytes[range.checked(bytes.len(), 8).ok_or(BAD_ABI)?])
    }

    pub(crate) fn window(&self) -> Result<WindowDefinitionState, Status>
    {
        if !matches!(self.object, ObjectKind::Window)
        {
            return Err(BAD_ARGUMENT);
        }
        wire::decode_value(self.range(self.layout.object)?)
    }

    pub(crate) fn control(&self) -> Result<ControlDefinitionState, Status>
    {
        if !matches!(self.object, ObjectKind::Control)
        {
            return Err(BAD_ARGUMENT);
        }
        wire::decode_value(self.range(self.layout.object)?)
    }

    pub(crate) fn scrollbar(&self) -> Result<ScrollbarState, Status>
    {
        if !matches!(self.object, ObjectKind::Scrollbar)
        {
            return Err(BAD_ARGUMENT);
        }
        wire::decode_value(self.range(self.layout.object)?)
    }

    pub(crate) fn change(&self) -> Result<ControlChange, Status>
    {
        if !matches!(self.object, ObjectKind::Change)
        {
            return Err(BAD_ARGUMENT);
        }
        wire::decode_value(self.range(self.layout.object)?)
    }

    pub(crate) fn layout(&self) -> Result<Payload, Status>
    {
        if self.layout.argument_kind != CALLBACK_BYTES
        {
            return Err(BAD_ARGUMENT);
        }
        wire::decode_value(self.range(self.layout.arguments)?)
    }

    /// Decode into the owning planner's private scratch arrays. No publication
    /// occurs here; the shared native grid validator admits geometry afterward.
    pub(crate) fn grid_axis(&self, tracks: &mut [GridTrack], items: &mut [GridItem])
                            -> Result<(i64, i64), Status>
    {
        if self.layout.argument_kind != CALLBACK_GRID_AXIS
        {
            return Err(BAD_ARGUMENT);
        }
        let axis: GridAxisCall = wire::decode_value(self.range(self.layout.arguments)?)?;
        if tracks.len() != axis.track_count || items.len() != axis.item_count
        {
            return Err(BAD_ABI);
        }
        let track_bytes = self.range(self.layout.auxiliary[0])?;
        let item_bytes = self.range(self.layout.auxiliary[1])?;
        for (target, bytes) in
            tracks.iter_mut().zip(track_bytes.chunks_exact(size_of::<GridTrack>()))
        {
            *target = wire::decode_value(bytes)?;
        }
        for (target, bytes) in items.iter_mut().zip(item_bytes.chunks_exact(size_of::<GridItem>()))
        {
            *target = wire::decode_value(bytes)?;
        }
        Ok((axis.minimum, axis.preferred))
    }

    fn validate(&self) -> Result<(), Status>
    {
        match self.object
        {
            ObjectKind::Opaque => (),
            ObjectKind::Window =>
            {
                let value = self.window()?;
                text(&value.title, value.title_length)?;
                if value.structure_piece_count > value.structure_pieces.len() as u32
                {
                    return Err(BAD_ABI);
                }
            }
            ObjectKind::Control =>
            {
                let value = self.control()?;
                text(&value.title, value.title_length)?;
            }
            ObjectKind::Scrollbar =>
            {
                self.scrollbar()?;
            }
            ObjectKind::Change =>
            {
                let value = self.change()?;
                text(&value.before.title, value.before.title_length)?;
                text(&value.after.title, value.after.title_length)?;
            }
        }
        if self.layout.argument_kind == CALLBACK_GRID_AXIS
        {
            let axis: GridAxisCall = wire::decode_value(self.range(self.layout.arguments)?)?;
            if axis.track_count != self.layout.auxiliary[0].length as usize / size_of::<GridTrack>()
               || axis.item_count
                  != self.layout.auxiliary[1].length as usize / size_of::<GridItem>()
            {
                return Err(BAD_ABI);
            }
        }
        if self.layout.argument_kind == CALLBACK_GRID_PAINT
        {
            let grid: GridPaint = wire::decode_value(self.range(self.layout.arguments)?)?;
            if grid.count != self.layout.auxiliary[0].length as usize / size_of::<Rect>()
            {
                return Err(BAD_ABI);
            }
        }
        Ok(())
    }

    /// Called only after the manager validates the original target generation,
    /// revision and pending claim. Copying changes only the captured clip.
    pub(crate) fn paint(&self, canvas: &mut Canvas<'_>) -> Result<Rect, Status>
    {
        let surface = self.surface.ok_or(BAD_ARGUMENT)?;
        if canvas.width != surface.width
           || canvas.height != surface.height
           || canvas.origin() != surface.origin
        {
            return Err(BAD_ARGUMENT);
        }
        let r = surface.destination;
        let clip = canvas.clip();
        if r.left < clip.left || r.top < clip.top || r.right > clip.right || r.bottom > clip.bottom
        {
            return Err(BAD_ARGUMENT);
        }
        let pixels = self.range(self.layout.pixels)?;
        let width = (r.right - r.left) as usize;
        let height = (r.bottom - r.top) as usize;
        let stride = self.layout.surface_width as usize;
        let required = stride.checked_mul(self.layout.surface_height as usize)
                             .and_then(|n| n.checked_mul(4))
                             .ok_or(BAD_ABI)?;
        let x_offset = usize::try_from(r.left - surface.crop.h).map_err(|_| BAD_ABI)?;
        let y_offset = usize::try_from(r.top - surface.crop.v).map_err(|_| BAD_ABI)?;
        if x_offset.checked_add(width).is_none_or(|end| end > stride)
           || y_offset.checked_add(height)
                      .is_none_or(|end| end > self.layout.surface_height as usize)
        {
            return Err(BAD_ABI);
        }
        if required != pixels.len()
           || canvas.width.checked_mul(canvas.height).ok_or(BAD_ABI)? > canvas.pixels.len()
        {
            return Err(BAD_ABI);
        }
        let original = self.original.as_ref().ok_or(BAD_ABI)?;
        let baseline = self.layout.pixels.checked(original.bytes.len(), 8).ok_or(BAD_ABI)?;
        let baseline = &original.bytes[baseline];
        if baseline.len() != required
        {
            return Err(BAD_ABI);
        }
        // A valid target/revision is insufficient for transparent/XOR author
        // drawing: a different committed underlying image invalidates this
        // result. Validate every original pixel before changing any target.
        for y in 0..height
        {
            let target = (r.top as usize + y) * canvas.width + r.left as usize;
            for x in 0..width
            {
                let at = ((y + y_offset) * stride + x + x_offset) * 4;
                if canvas.pixels[target + x]
                   != u32::from_ne_bytes([baseline[at],
                                          baseline[at + 1],
                                          baseline[at + 2],
                                          baseline[at + 3]])
                {
                    return Err(SERVICE_BUSY);
                }
            }
        }
        for y in 0..height
        {
            let target = (r.top as usize + y) * canvas.width + r.left as usize;
            for x in 0..width
            {
                let at = ((y + y_offset) * stride + x + x_offset) * 4;
                canvas.pixels[target + x] = u32::from_ne_bytes([pixels[at],
                                                                pixels[at + 1],
                                                                pixels[at + 2],
                                                                pixels[at + 3]]);
            }
        }
        Ok(r)
    }
}
fn text(bytes: &[u8], length: u32) -> Result<(), Status>
{
    let bytes = bytes.get(..length as usize).ok_or(BAD_ABI)?;
    core::str::from_utf8(bytes).map(|_| ()).map_err(|_| BAD_ABI)
}

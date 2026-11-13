//! Resident scroll boxes. Object identities, viewport pixels, dirty rows and
//! tracking policy stay in System; CDEF1 owns the scrollbar's pixels and geometry.
#![forbid(unsafe_code)]
use alloc::{sync::Arc, vec::Vec};

use mac_abi::{controls::*,
              geometry::{Point, Rect},
              scrollbox::{BUTTON_DOWN, CHANGED, TRACKING, *},
              *};

mod paint;
pub(crate) mod callbacks;
pub(crate) use paint::{ScrollBoxPaintClaim, ScrollBoxPaintOrigin, ScrollBoxPaintSnapshot};

use super::tracking::{Action, Tracking};
use crate::toolbox::{handles::{CapacityPolicy, KindCode, KindLease, OwnerRef, Store},
                     quickdraw::{Canvas, fonts::FontManager, fonts_service},
                     resources::definitions::{BoundDefinition, DefinitionRef}};
struct Row
{
    pixels: Vec<u32>
}
struct Raster
{
    width:      usize,
    height:     usize,
    row_height: usize,
    background: u32,
    rows:       Vec<Option<Arc<Row>>>,
    valid:      Vec<bool>
}
impl Raster
{
    fn pixels(&self) -> usize
    {
        self.width * self.height
    }
}
#[derive(Clone)]
struct Viewport
{
    spec:      Spec,
    top:       u32,
    raster:    Arc<Raster>,
    tracking:  Option<Tracking>,
    highlight: i16
}
struct Object
{
    window:           u64,
    recipient:        u64,
    recipient_owner:  mac_object_table::domain::DomainId,
    recovery:         bool,
    view:             Viewport,
    // Retain executable source through all control payload destruction; the
    // recovery Activity below remains the final modal-session acknowledgment.
    definition:       BoundDefinition,
    #[cfg(systemrisc)]
    _recovery_origin: Option<crate::recovery::ScrollBoxOrigin>
}
pub struct Manager
{
    objects:      Store<Object>,
    raster_bytes: usize,
    reserved_raster_bytes: usize,
    publication: Option<mac_object_table::store::PublicationReservation<crate::toolbox::handles::InvocationGate>>
}
pub(crate) fn specification(state: bool) -> mac_object_table::registry::Specification<'static>
{
    mac_object_table::registry::Specification { kind:     KindCode(if state
                                                                   {
                                                                       *b"SBST"
                                                                   }
                                                                   else
                                                                   {
                                                                       *b"SBOX"
                                                                   }),
                                                label:    if state
                                                {
                                                    "Scroll Box Manager state"
                                                }
                                                else
                                                {
                                                    "Managed scroll boxes"
                                                },
                                                provider: "System Control Manager",
                                                capacity: if state
                                                {
                                                    CapacityPolicy::HardBound { slots:  1,
                                                                                reason:
                                                                                    "One Scroll \
                                                                                     Box Manager \
                                                                                     backing state \
                                                                                     per domain" }
                                                }
                                                else
                                                {
                                                    CapacityPolicy::GrowableUntilMemory
                                                },
                                                source:
                                                    mac_object_table::registry::Source::Resident }
}
pub(crate) trait ScrollBoxHost
{
    fn managed_scroll_boxes(&mut self) -> &mut Manager;
}
pub(crate) struct ReleaseScrollBoxes;
pub(crate) struct ReleaseStorage;
impl<C: ScrollBoxHost> mac_object_table::registry::Release<C> for ReleaseScrollBoxes
{
    fn release(&self, context: &mut C, ticket: mac_object_table::domain::ReleasedResource)
               -> Result<(), mac_object_table::domain::CompletionError>
    {
        let manager = context.managed_scroll_boxes();
        let object = manager.objects.release(ticket)?;
        manager.raster_bytes -= object.view.raster.pixels() * 8;
        drop(object);
        Ok(())
    }
}
impl<C: ScrollBoxHost> mac_object_table::registry::Release<C> for ReleaseStorage
{
    fn release(&self, context: &mut C, ticket: mac_object_table::domain::ReleasedResource)
               -> Result<(), mac_object_table::domain::CompletionError>
    {
        context.managed_scroll_boxes().objects.release_storage(ticket)
    }
}
impl Manager
{
    pub(crate) fn new((objects, storage): (KindLease, KindLease)) -> Result<Self, Status>
    {
        Ok(Self { objects:               Store::owned(objects, storage).map_err(|_| BAD_ARGUMENT)?,
                  raster_bytes:          0,
                  reserved_raster_bytes: 0,
                  publication:           None })
    }
}
pub struct Drawing
{
    pub id:           u64,
    origin:           ScrollBoxPaintOrigin,
    bounds:           Rect,
    raster:           Arc<Raster>,
    bar:              super::Drawing,
    // Actual immutable readers release raster and executable source BEFORE
    // this original Session pin wakes closing cleanup.
    #[cfg(systemrisc)]
    _recovery_origin: Option<crate::recovery::ScrollBoxOrigin>
}
pub(crate) struct PaintCursor
{
    raster_done: bool,
    bar:         super::paint::Cursor
}
impl Default for PaintCursor
{
    fn default() -> Self
    {
        Self { raster_done: false,
               bar:         super::paint::Cursor::default() }
    }
}
impl PaintCursor
{
    pub(crate) fn advance_user(&mut self, resume: super::paint::Resume) -> Result<(), Status>
    {
        self.bar.advance_user(resume)
    }
}
impl Drawing
{
    pub fn cancel_tracking(&mut self) -> Option<Rect>
    {
        self.bar.cancel_tracking().then(|| self.bar.bounds())
    }

    pub fn bounds(&self) -> [Rect; 2]
    {
        [self.bounds, self.bar.bounds()]
    }

    pub fn storage(&self) -> usize
    {
        // This is retained-scene admission, distinct from physical allocation
        // receipts and raster quota. bar.storage already counts its header.
        (core::mem::size_of::<Self>() - core::mem::size_of::<super::Drawing>())
        + self.raster.pixels() * 4
        + self.raster.valid.capacity()
        + self.bar.storage()
    }

    pub(crate) fn has_user_sources(&self) -> bool
    {
        self.bar.has_user_sources()
    }

    pub fn paint(&self, canvas: &mut Canvas<'_>) -> Result<(), Status>
    {
        match self.paint_step_mode(&mut PaintCursor::default(), canvas, false)?
        {
            super::paint::Progress::Complete => Ok(()),
            super::paint::Progress::User(_) => Err(SERVICE_BUSY)
        }
    }

    pub(crate) fn paint_step(&self, cursor: &mut PaintCursor, canvas: &mut Canvas<'_>)
                             -> Result<super::paint::Progress, Status>
    {
        self.paint_step_mode(cursor, canvas, true)
    }

    pub(crate) fn paint_step_mode(&self, cursor: &mut PaintCursor, canvas: &mut Canvas<'_>,
                                  staged: bool)
                                  -> Result<super::paint::Progress, Status>
    {
        if !cursor.raster_done
        {
            self.paint_raster(canvas);
            cursor.raster_done = true;
        }
        let mut progress = super::paint::run(&self.bar, &mut cursor.bar, canvas, staged)?;
        if let super::paint::Progress::User(plan) = &mut progress
        {
            plan.scroll_box_subject(&mut cursor.bar, self)?;
        }
        Ok(progress)
    }

    fn paint_raster(&self, canvas: &mut Canvas<'_>)
    {
        let o = canvas.origin();
        let c = canvas.clip();
        let b = self.bounds;
        let left = (i32::from(b.left) + i32::from(o.h)).max(i32::from(c.left)).max(0);
        let top = (i32::from(b.top) + i32::from(o.v)).max(i32::from(c.top)).max(0);
        let right =
            (i32::from(b.right) + i32::from(o.h)).min(i32::from(c.right)).min(canvas.width as i32);
        let bottom = (i32::from(b.bottom) + i32::from(o.v)).min(i32::from(c.bottom))
                                                           .min(canvas.height as i32);
        if left < right && top < bottom
        {
            let width = (b.right - b.left) as usize;
            let n = (right - left) as usize;
            for y in top..bottom
            {
                let local_y = (y - i32::from(o.v) - i32::from(b.top)) as usize;
                let local_x = (left - i32::from(o.h) - i32::from(b.left)) as usize;
                let dest = y as usize * canvas.width + left as usize;
                let output = &mut canvas.pixels[dest..dest + n];
                if let Some(Some(row)) = self.raster.rows.get(local_y / self.raster.row_height)
                {
                    let src = (local_y % self.raster.row_height) * width + local_x;
                    output.copy_from_slice(&row.pixels[src..src + n]);
                }
                else
                {
                    output.fill(self.raster.background);
                }
            }
        }
    }
}
fn dimensions(spec: Spec) -> Result<(usize, usize, usize), Status>
{
    let w = i32::from(spec.viewport.right) - i32::from(spec.viewport.left);
    let h = i32::from(spec.viewport.bottom) - i32::from(spec.viewport.top);
    if w <= 0
       || h <= 0
       || w > 2048
       || h > 2048
       || spec.row_height <= 0
       || spec.row_height as i32 > h
       || spec.padding.h < 0
       || spec.padding.v < 0
       || spec.padding.v > spec.row_height
       || spec.reserved != 0
       || spec.scrollbar.right as i32 - spec.scrollbar.left as i32 != 16
       || spec.scrollbar.bottom as i32 - (spec.scrollbar.top as i32) < 32
    {
        return Err(BAD_ARGUMENT);
    }
    Ok((w as usize, h as usize, h as usize / spec.row_height as usize))
}
/// Says which of a scroll box's own allocations came back short, and how
/// many elements it had asked for, then gives the status the caller returns.
fn short(stage: &'static str, amount: usize) -> Status
{
    serial!("SCROLL_BOX_REFUSED stage={stage} amount={amount}");
    NO_MEMORY
}
fn raster(spec: Spec) -> Result<Arc<Raster>, Status>
{
    let (width, height, count) = dimensions(spec)?;
    let mut rows = Vec::new();
    rows.try_reserve_exact(count).map_err(|_| short("row-table", count))?;
    rows.resize_with(count, || None);
    let mut valid = Vec::new();
    valid.try_reserve_exact(count).map_err(|_| short("valid-table", count))?;
    valid.resize(count, false);
    Arc::try_new(Raster { width,
                          height,
                          row_height: spec.row_height as usize,
                          background: spec.background,
                          rows,
                          valid }).map_err(|_| short("raster", count))
}
fn writable(object: &mut Viewport) -> Result<&mut Raster, Status>
{
    if Arc::get_mut(&mut object.raster).is_none()
    {
        let source = &object.raster;
        let mut rows = Vec::new();
        rows.try_reserve_exact(source.rows.len()).map_err(|_| short("row-table-copy", source.rows.len()))?;
        rows.extend(source.rows.iter().cloned());
        let mut valid = Vec::new();
        valid.try_reserve_exact(source.valid.len()).map_err(|_| short("valid-table-copy", source.valid.len()))?;
        valid.extend_from_slice(&source.valid);
        // A snapshot shares immutable row tiles. Copy only the small row table;
        // filling a row later replaces that row, never the entire viewport.
        object.raster = Arc::try_new(Raster { width: source.width,
                                              height: source.height,
                                              row_height: source.row_height,
                                              background: source.background,
                                              rows,
                                              valid }).map_err(|_| short("raster-copy", 0))?;
    }
    Arc::get_mut(&mut object.raster).ok_or(SERVICE_BUSY)
}
fn row_pixels(raster: &mut Raster, index: usize) -> Result<&mut [u32], Status>
{
    let slot = raster.rows.get_mut(index).ok_or(BAD_ARGUMENT)?;
    if slot.as_mut().and_then(Arc::get_mut).is_none()
    {
        let size = raster.width * raster.row_height;
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(size).map_err(|_| short("row-pixels", size))?;
        pixels.resize(size, raster.background);
        *slot = Some(Arc::try_new(Row { pixels }).map_err(|_| short("row", size))?);
    }
    let row = slot.as_mut().ok_or(BAD_ABI)?;
    let row = Arc::get_mut(row).ok_or(SERVICE_BUSY)?;
    Ok(&mut row.pixels)
}
/// Prepare an immutable-row replacement before publishing a new scroll-box
/// snapshot. Resident compound views use this same primitive for icons + text;
/// the borrowed drawing closure cannot retain the canvas or call an app.
fn paint_row(object: &mut Viewport, row: u32,
             draw: impl FnOnce(&mut Canvas<'_>) -> Result<(), Status>)
             -> Result<(), Status>
{
    let index = row.checked_sub(object.top).ok_or(BAD_ARGUMENT)? as usize;
    if index >= object.raster.valid.len() || row >= object.spec.count
    {
        return Err(BAD_ARGUMENT);
    }
    let spec = object.spec;
    let (width, _, _) = dimensions(spec)?;
    let data = writable(object)?;
    data.valid[index] = false;
    let pixels = row_pixels(data, index)?;
    let mut canvas = Canvas::new(pixels, width, spec.row_height as usize);
    canvas.paint_rect(Rect::new(0, 0, width as i16, spec.row_height), spec.background);
    draw(&mut canvas)?;
    data.valid[index] = true;
    Ok(())
}

impl Viewport
{
    fn rows(&self) -> u32
    {
        self.raster.valid.len() as u32
    }

    fn maximum(&self) -> u32
    {
        self.spec.count.saturating_sub(self.rows())
    }

    fn state(&self) -> ScrollbarState
    {
        let maximum = self.maximum().min(i16::MAX as u32) as i16;
        ScrollbarState { bounds: self.spec.scrollbar,
                         value: if maximum == 0
                         {
                             0
                         }
                         else
                         {
                             (u64::from(self.top) * maximum as u64 / u64::from(self.maximum()))
                             as i16
                         },
                         maximum,
                         active: u32::from(maximum > 0),
                         highlighted: self.highlight,
                         reserved: 0 }
    }

    fn missing(&self) -> core::ops::Range<u32>
    {
        let end = self.rows().min(self.spec.count.saturating_sub(self.top)) as usize;
        let first = self.raster.valid[..end].iter().position(|v| !*v).unwrap_or(end);
        let last = self.raster.valid[..end].iter().rposition(|v| !*v).map_or(first, |i| i + 1);
        self.top + first as u32..self.top + last as u32
    }

    fn scroll(&mut self, top: u32) -> Result<(), Status>
    {
        let top = top.min(self.maximum());
        let delta = i64::from(top) - i64::from(self.top);
        if delta == 0
        {
            return Ok(());
        }
        let spec = self.spec;
        let (_, _, rows) = dimensions(spec)?;
        let data = writable(self)?;
        let distance = delta.unsigned_abs().min(rows as u64) as usize;
        if distance == rows
        {
            data.rows.fill(None);
            data.valid.fill(false);
        }
        else if delta > 0
        {
            data.rows.rotate_left(distance);
            data.rows[rows - distance..].fill(None);
            data.valid.copy_within(distance..rows, 0);
            data.valid[rows - distance..].fill(false);
        }
        else
        {
            data.rows.rotate_right(distance);
            data.rows[..distance].fill(None);
            data.valid.copy_within(0..rows - distance, distance);
            data.valid[..distance].fill(false);
        }
        self.top = top;
        Ok(())
    }
}
impl Manager
{
    fn get(&self, id: u64, owner: u64) -> Result<&Object, Status>
    {
        let object = self.objects.get(id).ok_or(NOT_FOUND)?;
        if object.recipient != owner
        {
            return Err(ACCESS_DENIED);
        }
        let storage = self.objects.owner(id).ok_or(NOT_FOUND)?;
        #[cfg(systemrisc)]
        {
            if object.recovery != object._recovery_origin.is_some()
            {
                return Err(BAD_ABI);
            }
            if let Some(origin) = &object._recovery_origin
            {
                let current = crate::context::recovery_activity().ok_or(ACCESS_DENIED)?;
                current.validate_scroll_box_origin(origin,
                                                   origin.recipient(),
                                                   storage,
                                                   mac_abi::WindowId(object.window),
                                                   crate::recovery::ScrollBoxAccess::Cleanup)?;
            }
            else if storage.incarnation() != owner
            {
                return Err(ACCESS_DENIED);
            }
        }
        #[cfg(not(systemrisc))]
        if storage.incarnation() != owner
        {
            return Err(ACCESS_DENIED);
        }
        Ok(object)
    }

    fn get_mut(&mut self, id: u64, owner: u64) -> Result<&mut Object, Status>
    {
        self.get(id, owner)?;
        self.objects.get_mut(id).ok_or(NOT_FOUND)
    }

    /// Direct resident row providers require the same live entry admission as
    /// request SET_ROW. Cleanup/cancel keep their separate get_mut path.
    fn get_mut_live(&mut self, id: u64, owner: u64) -> Result<&mut Object, Status>
    {
        let object = self.get(id, owner)?;
        #[cfg(systemrisc)]
        if let Some(origin) = &object._recovery_origin
        {
            let current = crate::context::recovery_activity().ok_or(ACCESS_DENIED)?;
            let storage = self.objects.owner(id).ok_or(NOT_FOUND)?;
            current.validate_scroll_box_origin(origin,
                                               origin.recipient(),
                                               storage,
                                               mac_abi::WindowId(object.window),
                                               crate::recovery::ScrollBoxAccess::Live)?;
        }
        self.objects.get_mut(id).ok_or(NOT_FOUND)
    }

    pub fn window(&self, id: u64, owner: u64) -> Result<u64, Status>
    {
        Ok(self.get(id, owner)?.window)
    }

    /// Render one already-prepared resident row through the managed viewport.
    /// No provider, application callback or trap may run inside `draw`.
    pub(crate) fn set_row_with(&mut self, id: u64, owner: u64, row: u32,
                               draw: impl FnOnce(&mut Canvas<'_>) -> Result<(), Status>)
                               -> Result<(), Status>
    {
        paint_row(&mut self.get_mut_live(id, owner)?.view, row, draw)
    }

    /// Retain the viewport allowance derived from the admitted recovery UI.
    /// Its protected allocation stock is independent of ordinary row tiles,
    /// whose actual allocations retain their original account/budget receipts.
    pub(crate) fn prepare_recovery(&mut self, storage: &OwnerRef, bytes: usize)
                                   -> Result<(), Status>
    {
        if storage.owner().kind() != mac_object_table::domain::DomainKind::System || bytes == 0
        {
            return Err(BAD_ARGUMENT);
        }
        if self.publication.is_none()
        {
            self.publication =
                Some(self.objects.reserve_publication(storage).map_err(pool_status)?);
        }
        self.reserved_raster_bytes = bytes;
        Ok(())
    }

    pub(crate) fn cancel(&mut self, call: &mut Call, owner: u64) -> Result<(), Status>
    {
        if !call.surface.is_null() || call.reserved != [0; 3]
        {
            return Err(BAD_ARGUMENT);
        }
        let object = self.get_mut(call.id, owner)?;
        object.view.tracking = None;
        let changed = core::mem::replace(&mut object.view.highlight, 0) != 0;
        call.window = object.window;
        call.spec = object.view.spec;
        call.top = object.view.top;
        let missing = object.view.missing();
        call.first = missing.start;
        call.end = missing.end;
        call.part = 0;
        call.flags = u32::from(changed) * CHANGED;
        Ok(())
    }

    pub fn raster_storage(&self) -> usize
    {
        // Conservative compositor cache-pressure estimate only. Row tables and
        // tiles allocate lazily; this is not a memory reservation or admission
        // limit. The allocator accounts actual backing, including shared rows.
        self.raster_bytes
    }

    pub fn remove_window(&mut self, window: u64)
    {
        let bytes = &mut self.raster_bytes;
        self.objects.remove_where_with(|_, object| object.window == window,
                                       |object| {
                                           *bytes -= object.view.raster.pixels() * 8;
                                           drop(object);
                                       });
    }

    /// Fixed CDEF1 binding retained by the actual owned scroll box.
    pub(crate) fn bound_definition(&self, call: &Call, owner: u64)
                                   -> Result<Option<DefinitionRef>, Status>
    {
        if call.operation == CREATE
        {
            Ok(None)
        }
        else
        {
            Ok(Some(self.get(call.id, owner)?.definition.reference()))
        }
    }

    #[cfg(systemrisc)]
    fn creation_origin(recipient: &OwnerRef, storage: &OwnerRef, window: u64)
                       -> Result<Option<crate::recovery::ScrollBoxOrigin>, Status>
    {
        match crate::context::recovery_activity()
        {
            Some(activity) =>
            {
                activity.scroll_box_origin(recipient, storage, mac_abi::WindowId(window)).map(Some)
            }
            None if recipient.owner() == storage.owner() => Ok(None),
            None => Err(ACCESS_DENIED)
        }
    }

    /// Admission only: no definition callbacks, raster allocation or mutation.
    pub(crate) fn preflight_definition(&self, recipient: &OwnerRef, storage: &OwnerRef,
                                       call: &Call, fonts: &FontManager)
                                       -> Result<Option<DefinitionRef>, Status>
    {
        let owner = recipient.owner().incarnation();
        #[cfg(systemrisc)]
        let creation_origin = if call.operation == CREATE
        {
            Self::creation_origin(recipient, storage, call.window)?
        }
        else
        {
            None
        };
        #[cfg(not(systemrisc))]
        if storage.owner() != recipient.owner()
        {
            return Err(ACCESS_DENIED);
        }
        if call.operation == CREATE && !storage.accepting()
        {
            return Err(pool_status(mac_object_table::domain::PoolError::Stopping {
                owner: storage.owner()
            }));
        }
        if call.reserved != [0; 3] || call.text_length as usize > call.text.len()
        {
            return Err(BAD_ARGUMENT);
        }
        core::str::from_utf8(&call.text[..call.text_length as usize]).map_err(|_| BAD_ARGUMENT)?;
        if !matches!(call.operation,
                     CREATE
                     | CONFIGURE
                     | INFO
                     | ROW_AT
                     | INVALIDATE
                     | SCROLL_TO
                     | SET_ROW
                     | TRACK_BEGIN
                     | TRACK_POLL
                     | DRAW
                     | DISPOSE)
        {
            return Err(NOT_SUPPORTED);
        }
        if call.operation != CREATE
        {
            let object = self.get(call.id, owner)?;
            #[cfg(systemrisc)]
            if let Some(origin) = &object._recovery_origin
            {
                let current = crate::context::recovery_activity().ok_or(ACCESS_DENIED)?;
                let access = if call.operation == DISPOSE
                {
                    crate::recovery::ScrollBoxAccess::Cleanup
                }
                else
                {
                    crate::recovery::ScrollBoxAccess::Live
                };
                current.validate_scroll_box_origin(origin,
                                                   recipient.owner(),
                                                   storage.owner(),
                                                   mac_abi::WindowId(object.window),
                                                   access)?;
            }
            else if storage.owner() != recipient.owner()
            {
                return Err(ACCESS_DENIED);
            }
        }
        if call.operation == SET_ROW && call.flags & !SELECTED != 0
        {
            return Err(BAD_ARGUMENT);
        }
        if matches!(call.operation, CREATE | CONFIGURE)
        {
            let (w, h, _) = dimensions(call.spec)?;
            fonts.metrics(fonts_service::selection(call.spec.font))?;
            let recovery = if call.operation == CREATE
            {
                #[cfg(systemrisc)]
                {
                    creation_origin.is_some()
                }
                #[cfg(not(systemrisc))]
                {
                    false
                }
            }
            else
            {
                self.get(call.id, owner)?.recovery
            };
            if recovery
            {
                if w * h * 8 > self.reserved_raster_bytes
                {
                    return Err(short("recovery-allowance", w * h * 8));
                }
            }
        }
        Ok(if call.operation == CREATE
           {
               None
           }
           else
           {
               Some(self.get(call.id, owner)?.definition.reference())
           })
    }

    pub(crate) fn request(&mut self, recipient: &OwnerRef, storage: &OwnerRef, call: &mut Call,
                          fonts: &FontManager, entry: DefinitionRef, gate: TrapGate)
                          -> Result<Option<Drawing>, Status>
    {
        let bound = self.preflight_definition(recipient, storage, call, fonts)?;
        let entry = bound.unwrap_or(entry);
        let owner = recipient.owner().incarnation();
        let text = core::str::from_utf8(&call.text[..call.text_length as usize]).map_err(|_| {
                                                                                    BAD_ARGUMENT
                                                                                })?;
        if matches!(call.operation, CREATE | CONFIGURE)
        {
            let (w, h, _) = dimensions(call.spec)?;
            let previous = if call.operation == CONFIGURE
            {
                self.get(call.id, owner)?.view.raster.pixels() * 8
            }
            else
            {
                0
            };
            let used = self.raster_bytes;
            #[cfg(systemrisc)]
            let creation_origin = if call.operation == CREATE
            {
                Self::creation_origin(recipient, storage, call.window)?
            }
            else
            {
                None
            };
            let recovery = if call.operation == CREATE
            {
                #[cfg(systemrisc)]
                {
                    creation_origin.is_some()
                }
                #[cfg(not(systemrisc))]
                {
                    false
                }
            }
            else
            {
                self.get(call.id, owner)?.recovery
            };
            if call.operation == CREATE
            {
                let binding = crate::toolbox::resources::definitions::bind(entry)?;
                let data = raster(call.spec)?;
                let object = Object { window: call.window,
                                      recipient: owner,
                                      recipient_owner: recipient.owner(),
                                      recovery,
                                      view: Viewport { spec:      call.spec,
                                                       top:       0,
                                                       raster:    data,
                                                       tracking:  None,
                                                       highlight: 0 },
                                      definition: binding,
                                      #[cfg(systemrisc)]
                                      _recovery_origin: creation_origin.clone() };
                #[cfg(systemrisc)]
                {
                    call.id = if let Some(origin) = creation_origin
                    {
                        let activity = origin.activity();
                        let subject = activity.root_subject();
                        self.objects.insert_reserved_in_scope_with_acknowledgement(
                        self.publication.as_mut().ok_or(NOT_SUPPORTED)?, subject.scope(),
                        Some(activity.acknowledgement_pin()), |_| object)
                        .map_err(publication_status)?.id()
                    }
                    else
                    {
                        self.objects.insert(storage, object).map_err(pool_status)?
                    };
                }
                #[cfg(not(systemrisc))]
                {
                    call.id = self.objects.insert(storage, object).map_err(pool_status)?;
                }
                self.raster_bytes = used + w * h * 8;
            }
            else
            {
                let object = self.get_mut(call.id, owner)?;
                if object.view.spec != call.spec
                {
                    let mut same_layout = object.view.spec;
                    same_layout.count = call.spec.count;
                    if same_layout != call.spec
                    {
                        object.view.raster = raster(call.spec)?;
                    }
                    else
                    {
                        // Admit the row-table COW before publishing count/top.
                        // A refusal leaves the previous scroll range intact.
                        writable(&mut object.view)?;
                    }
                    object.view.spec = call.spec;
                    object.view.scroll(object.view.top.min(object.view.maximum()))?;
                    let top = object.view.top;
                    let count = object.view.spec.count;
                    let data = writable(&mut object.view)?;
                    for (index, (row, valid)) in
                        data.rows.iter_mut().zip(&mut data.valid).enumerate()
                    {
                        if top.saturating_add(index as u32) >= count
                        {
                            *row = None;
                            *valid = false;
                        }
                    }
                    object.view.tracking = None;
                    object.view.highlight = 0;
                    self.raster_bytes = used - previous + w * h * 8;
                }
            }
        }
        if call.operation == DISPOSE
        {
            self.get(call.id, owner)?;
            if let Some(object) = self.objects.remove(call.id)
            {
                self.raster_bytes -= object.view.raster.pixels() * 8;
            }
            return Ok(None);
        }
        let paint_key = if call.operation == DRAW
        {
            Some(self.objects.completion_key(call.id).ok_or(NOT_FOUND)?)
        }
        else
        {
            None
        };
        let object = self.get_mut(call.id, owner)?;
        let old_top = object.view.top;
        let old_highlight = object.view.highlight;
        match call.operation
        {
            CREATE | CONFIGURE | INFO => (),
            ROW_AT =>
            {
                call.row = u32::MAX;
                if object.view.spec.viewport.contains(call.point)
                {
                    let row = ((i32::from(call.point.v) - i32::from(object.view.spec.viewport.top))
                               / i32::from(object.view.spec.row_height))
                              as u32;
                    if row < object.view.rows() && object.view.top + row < object.view.spec.count
                    {
                        call.row = object.view.top + row;
                    }
                }
            }
            INVALIDATE =>
            {
                let data = writable(&mut object.view)?;
                data.rows.fill(None);
                data.valid.fill(false);
            }
            SCROLL_TO => object.view.scroll(call.top)?,
            SET_ROW =>
            {
                let spec = object.view.spec;
                let selected = call.flags & SELECTED != 0;
                paint_row(&mut object.view, call.row, |canvas| {
                    if selected
                    {
                        canvas.paint_rect(Rect::new(1,
                                                    0,
                                                    spec.viewport.right - spec.viewport.left,
                                                    spec.row_height - 1),
                                          spec.selected_background);
                    }
                    fonts.draw(fonts_service::selection(spec.font),
                               canvas,
                               Point { h: spec.padding.h,
                                       v: spec.padding.v },
                               text,
                               if selected { spec.selected_foreground } else { spec.foreground })?;
                    Ok(())
                })?;
            }
            TRACK_BEGIN =>
            {
                if object.view.tracking.is_some()
                {
                    return Err(SERVICE_BUSY);
                }
                let mut c = ControlCall { operation: HIT_SCROLLBAR,
                                          scrollbar: object.view.state(),
                                          point: call.point,
                                          ..Default::default() };
                super::prepare(&mut c, entry, gate)?;
                let part = c.result as i16;
                call.part = part;
                if matches!(part,
                            SCROLL_UP
                            | SCROLL_DOWN
                            | SCROLL_PAGE_UP
                            | SCROLL_PAGE_DOWN
                            | SCROLL_THUMB)
                {
                    if part != SCROLL_THUMB
                    {
                        step(&mut object.view, part)?;
                    }
                    object.view.tracking =
                        Some(Tracking::new(part, call.point, c.scrollbar.value, call.ticks));
                    object.view.highlight = part;
                }
            }
            TRACK_POLL =>
            {
                if let Some(tracking) = object.view.tracking
                {
                    let state = object.view.state();
                    let transition =
                        tracking.poll(call.point,
                                      call.flags & BUTTON_DOWN != 0,
                                      call.ticks,
                                      Some(state),
                                      0,
                                      entry,
                                      gate,
                                      |point| {
                                          let mut hit = ControlCall { operation: HIT_SCROLLBAR,
                                                                      scrollbar: state,
                                                                      point,
                                                                      ..Default::default() };
                                          super::prepare(&mut hit, entry, gate)?;
                                          Ok(hit.result as i16)
                                      })?;
                    apply_transition(&mut object.view, transition)?;
                }
            }
            DRAW => (),
            _ => return Err(NOT_SUPPORTED)
        }
        call.window = object.window;
        call.spec = object.view.spec;
        call.top = object.view.top;
        let missing = object.view.missing();
        call.first = missing.start;
        call.end = missing.end;
        call.flags = (u32::from(object.view.tracking.is_some()) * TRACKING)
                     | (u32::from(object.view.top != old_top
                                  || object.view.highlight != old_highlight)
                        * CHANGED);
        if call.operation == DRAW
        {
            if !missing.is_empty()
            {
                return Err(SERVICE_BUSY);
            }
            let mut c = ControlCall { operation: DRAW_SCROLLBAR,
                                      scrollbar: object.view.state(),
                                      ..Default::default() };
            let bar = super::prepare(&mut c, entry, gate)?.ok_or(BAD_ARGUMENT)?;
            if !bar.source.matches(&object.definition)
            {
                return Err(BAD_ABI);
            }
            let origin = ScrollBoxPaintOrigin::publication(paint_key.ok_or(BAD_ABI)?, object);
            return Ok(Some(Drawing { id: call.id,
                                     origin,
                                     bounds: object.view.spec.viewport,
                                     raster: object.view.raster.clone(),
                                     bar,
                                     #[cfg(systemrisc)]
                                     _recovery_origin: object._recovery_origin.clone() }));
        }
        Ok(None)
    }
}
fn apply_transition(view: &mut Viewport, transition: super::tracking::Transition)
                    -> Result<(), Status>
{
    match transition.action
    {
        Action::None => (),
        Action::Step(part) => step(view, part)?,
        Action::Value(value) =>
        {
            let maximum = view.maximum();
            let scale = maximum.min(i16::MAX as u32);
            let top = if scale == 0
            {
                0
            }
            else
            {
                ((value.max(0) as u64 * u64::from(maximum) + u64::from(scale) / 2)
                 / u64::from(scale)).min(u64::from(maximum)) as u32
            };
            view.scroll(top)?;
        }
        Action::Activate => return Err(BAD_ABI)
    }
    view.highlight = transition.highlight;
    view.tracking = transition.tracking;
    Ok(())
}

fn step(object: &mut Viewport, part: i16) -> Result<(), Status>
{
    let delta = match part
    {
        SCROLL_UP => -1,
        SCROLL_DOWN => 1,
        SCROLL_PAGE_UP => -i64::from(object.rows()),
        SCROLL_PAGE_DOWN => i64::from(object.rows()),
        _ => 0
    };
    object.scroll((i64::from(object.top) + delta).clamp(0, i64::from(object.maximum())) as u32)
}

fn pool_status(error: mac_object_table::domain::PoolError) -> Status
{
    crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(error))
}

#[cfg(systemrisc)]
fn publication_status(error: mac_object_table::store::PublicationError) -> Status
{
    match error
    {
        mac_object_table::store::PublicationError::Busy => SERVICE_BUSY,
        mac_object_table::store::PublicationError::Pool(error) => pool_status(error)
    }
}

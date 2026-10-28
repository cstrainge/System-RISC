//! Owned resident controls. CDEFs own geometry; this manager owns identity and
//! tracking policy. No caller memory or callback survives a request.
#![forbid(unsafe_code)]

use alloc::{sync::Arc, vec::Vec};

use mac_abi::{controls::*, *};

use crate::toolbox::handles::{CapacityPolicy, KindCode, KindLease, OwnerRef, Store};

pub(crate) fn specification() -> mac_object_table::registry::Specification<'static>
{
    mac_object_table::registry::Specification { kind:     KindCode(*b"CTRL"),
                                                label:    "Managed controls",
                                                provider: "System Control Manager",
                                                capacity: CapacityPolicy::GrowableUntilMemory,
                                                source:
                                                    mac_object_table::registry::Source::Resident }
}
pub(crate) fn storage_specification() -> mac_object_table::registry::Specification<'static>
{
    mac_object_table::registry::Specification {
        kind: KindCode(*b"CSTA"), label: "Control Manager state",
        provider: "System Control Manager",
        capacity: CapacityPolicy::HardBound { slots: 1, reason: "One Control Manager backing state per domain" },
        source: mac_object_table::registry::Source::Resident }
}
pub(crate) trait ControlHost
{
    fn managed_controls(&mut self) -> &mut Manager;
}
pub(crate) struct ReleaseControls;
pub(crate) struct ReleaseStorage;
impl<C: ControlHost> mac_object_table::registry::Release<C> for ReleaseStorage
{
    fn release(&self, context: &mut C, resource: mac_object_table::domain::ReleasedResource)
               -> Result<(), mac_object_table::domain::CompletionError>
    {
        context.managed_controls().objects.release_storage(resource)
    }
}
impl<C: ControlHost> mac_object_table::registry::Release<C> for ReleaseControls
{
    fn release(&self, context: &mut C, resource: mac_object_table::domain::ReleasedResource)
               -> Result<(), mac_object_table::domain::CompletionError>
    {
        drop(context.managed_controls().objects.release(resource)?);
        Ok(())
    }
}

struct Object
{
    window:             u64,
    recipient:          u64,
    spec:               Spec,
    scrollbar:          bool,
    title:              Option<Arc<super::snapshot::Title>>,
    tracking:           Option<Tracking>,
    // Last: registered removal must finish payload destruction before the
    // protected modal service can become available to another recipient.
    #[cfg(target_arch = "riscv64")]
    _recovery_activity: Option<crate::recovery::Activity>
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Spec
{
    standard:  mac_abi::definitions::ControlDefinitionState,
    scrollbar: ScrollbarState,
    procedure: i16,
    step:      i16,
    page:      i16
}
impl Spec
{
    fn from_call(c: &ControlCall) -> Self
    {
        Self { standard:  c.standard,
               scrollbar: c.scrollbar,
               procedure: c.procedure,
               step:      c.step,
               page:      c.page }
    }

    fn call(self, operation: u32) -> ControlCall
    {
        ControlCall { operation,
                      standard: self.standard,
                      scrollbar: self.scrollbar,
                      procedure: self.procedure,
                      step: self.step,
                      page: self.page,
                      ..Default::default() }
    }
}
#[derive(Clone, Copy)]
struct Tracking
{
    part:    i16,
    anchor:  Point,
    initial: i16,
    next:    u64,
    point:   Point,
    hot:     bool
}
pub struct Manager
{
    objects: Store<Object>,
    publications: Vec<mac_object_table::store::PublicationReservation<crate::toolbox::handles::InvocationGate>>
}
impl Manager
{
    pub(crate) fn new((kind, storage): (KindLease, KindLease)) -> Result<Self, Status>
    {
        Ok(Self { objects:      Store::owned(kind, storage).map_err(|_| BAD_ARGUMENT)?,
                  publications: Vec::new() })
    }
}
impl Manager
{
    fn get(&self, id: u64, owner: u64) -> Result<&Object, Status>
    {
        let o = self.objects.get(id).ok_or(NOT_FOUND)?;
        if o.recipient != owner
        {
            return Err(ACCESS_DENIED);
        }
        let storage = self.objects.owner(id).ok_or(NOT_FOUND)?;
        if storage.incarnation() != owner
        {
            #[cfg(target_arch = "riscv64")]
            {
                let activity = crate::context::recovery_activity().ok_or(ACCESS_DENIED)?;
                if activity.owner() != owner || activity.storage_owner().owner() != storage
                {
                    return Err(ACCESS_DENIED);
                }
            }
            #[cfg(not(target_arch = "riscv64"))]
            return Err(ACCESS_DENIED);
        }
        Ok(o)
    }

    /// Held canonical slots and cleanup records cannot be consumed by ordinary
    /// callers. The required count is computed from the guaranteed UI resources.
    pub(crate) fn prepare_recovery(&mut self, storage: &OwnerRef, count: usize)
                                   -> Result<(), Status>
    {
        if storage.owner().kind() != mac_object_table::domain::DomainKind::System || count == 0
        {
            return Err(BAD_ARGUMENT);
        }
        self.publications
            .try_reserve_exact(count.saturating_sub(self.publications.len()))
            .map_err(|_| NO_MEMORY)?;
        while self.publications.len() < count
        {
            self.publications
                .push(self.objects.reserve_publication(storage).map_err(pool_status)?);
        }
        Ok(())
    }

    pub fn window(&self, id: u64, owner: u64) -> Result<u64, Status>
    {
        Ok(self.get(id, owner)?.window)
    }

    pub fn remove_window(&mut self, window: u64)
    {
        self.objects.remove_where(|_, object| object.window == window);
    }

    pub fn remove_owner(&mut self, owner: u64)
    {
        self.objects.remove_owner(owner);
    }

    pub fn definition(&self, call: &ControlCall, owner: u64) -> Result<i16, Status>
    {
        Ok(if call.operation == CREATE_SCROLLBAR
           {
               1
           }
           else if call.operation == CREATE_STANDARD
           {
               super::definition_id(&Spec::from_call(call).call(DRAW_STANDARD))?
           }
           else
           {
               let o = self.get(call.id, owner)?;
               super::definition_id(&o.spec.call(if o.scrollbar
                                                 {
                                                     DRAW_SCROLLBAR
                                                 }
                                                 else
                                                 {
                                                     DRAW_STANDARD
                                                 }))?
           })
    }

    pub(crate) fn request(&mut self, recipient: &OwnerRef, storage: &OwnerRef,
                          call: &mut ControlCall, entry: DefinitionEntry, gate: TrapGate)
                          -> Result<Option<super::Drawing>, Status>
    {
        let owner = recipient.owner().incarnation();
        let recovery = storage.owner() != recipient.owner();
        if recovery
        {
            #[cfg(target_arch = "riscv64")]
            {
                let activity = crate::context::recovery_activity().ok_or(ACCESS_DENIED)?;
                if activity.owner() != owner
                   || activity.storage_owner().owner() != storage.owner()
                   || !crate::context::io_subject().is_some_and(|subject| {
                                                       subject.scope()
                                                              .same_scope(activity.root_subject()
                                                                                  .scope())
                                                   })
                   || (matches!(call.operation, CREATE_STANDARD | CREATE_SCROLLBAR)
                       && !recipient.accepting())
                {
                    return Err(ACCESS_DENIED);
                }
            }
            #[cfg(not(target_arch = "riscv64"))]
            return Err(ACCESS_DENIED);
        }
        let operation = call.operation;
        if !call.surface.is_null() || call.reserved != 0
        {
            return Err(BAD_ARGUMENT);
        }
        let mut admitted_title = None;
        if matches!(operation, CREATE_STANDARD | CREATE_SCROLLBAR | CONFIGURE_OBJECT)
        {
            let scrollbar = if operation == CONFIGURE_OBJECT
            {
                self.get(call.id, owner)?.scrollbar
            }
            else
            {
                operation == CREATE_SCROLLBAR
            };
            if scrollbar && (call.step <= 0 || call.page <= 0)
            {
                return Err(BAD_ARGUMENT);
            }
            let mut spec = *call;
            spec.operation = if scrollbar { DRAW_SCROLLBAR } else { DRAW_STANDARD };
            if operation == CONFIGURE_OBJECT
            {
                let old = self.get(call.id, owner)?;
                spec.standard.highlighted = old.spec.standard.highlighted;
                spec.scrollbar.highlighted = old.spec.scrollbar.highlighted;
            }
            if operation != CONFIGURE_OBJECT
               || self.get(call.id, owner)?.spec != Spec::from_call(&spec)
            {
                // Validate through the real resource before publishing or mutating.
                super::validate(&mut spec, entry, gate)?;
            }
            if !scrollbar
               && !matches!(spec.procedure,
                            PROGRESS_BAR_PROCEDURE | MONOCHROME_PROGRESS_BAR_PROCEDURE)
            {
                let previous = if operation == CONFIGURE_OBJECT
                {
                    self.get(call.id, owner)?.title.as_ref()
                }
                else
                {
                    None
                };
                admitted_title = Some(super::snapshot::Title::admit(&spec.standard, previous)?);
            }
            if matches!(operation, CREATE_STANDARD | CREATE_SCROLLBAR)
            {
                let object = Object { window: call.window,
                                      recipient: owner,
                                      spec: Spec::from_call(&spec),
                                      scrollbar,
                                      title: admitted_title.clone(),
                                      tracking: None,
                                      #[cfg(target_arch = "riscv64")]
                                      _recovery_activity: if recovery
                                      {
                                          crate::context::recovery_activity()
                                      }
                                      else
                                      {
                                          None
                                      } };
                call.id = if recovery
                {
                    let reservation = self.publications
                                          .iter_mut()
                                          .find(|reservation| reservation.ready())
                                          .ok_or(SERVICE_BUSY)?;
                    #[cfg(target_arch = "riscv64")]
                    {
                        let activity = crate::context::recovery_activity().ok_or(ACCESS_DENIED)?;
                        let subject = activity.root_subject();
                        self.objects.insert_reserved_in_scope_with_acknowledgement(
                            reservation, subject.scope(), Some(activity.acknowledgement_pin()),
                            |_| object).map_err(publication_status)?.id()
                    }
                    #[cfg(not(target_arch = "riscv64"))]
                    {
                        let _ = (reservation, object);
                        return Err(NOT_SUPPORTED);
                    }
                }
                else
                {
                    self.objects.insert(storage, object).map_err(pool_status)?
                };
            }
        }
        let object = self.get(call.id, owner)?;
        if operation == BEGIN_TRACKING && call.window != 0 && call.window != object.window
        {
            return Err(BAD_ARGUMENT);
        }
        if operation == DISPOSE_OBJECT
        {
            self.objects.remove(call.id);
            return Ok(None);
        }
        if operation == BEGIN_TRACKING
           && self.objects
                  .iter()
                  .any(|(id, o)| id != call.id && o.recipient == owner && o.tracking.is_some())
        {
            return Err(SERVICE_BUSY);
        }
        let o = self.objects.get_mut(call.id).ok_or(NOT_FOUND)?;
        let previous = (o.spec.standard, o.spec.scrollbar, o.spec.procedure);
        let mut action = false;
        let mut handled = false;
        let mut part = 0;
        match operation
        {
            CREATE_STANDARD | CREATE_SCROLLBAR | INFO_OBJECT | DRAW_OBJECT => (),
            CONFIGURE_OBJECT =>
            {
                // New title backing was admitted before any state/capture mutation.
                o.title = admitted_title;
                let cancel = o.spec.standard.bounds != call.standard.bounds
                             || o.spec.standard.enabled != call.standard.enabled
                             || o.spec.scrollbar.bounds != call.scrollbar.bounds
                             || o.spec.scrollbar.active != call.scrollbar.active
                             || o.spec.procedure != call.procedure;
                let highlight = if cancel
                {
                    0
                }
                else if o.scrollbar
                {
                    o.spec.scrollbar.highlighted as u32
                }
                else
                {
                    o.spec.standard.highlighted
                };
                o.spec.standard = call.standard;
                o.spec.scrollbar = call.scrollbar;
                o.spec.procedure = call.procedure;
                o.spec.step = call.step;
                o.spec.page = call.page;
                if cancel
                {
                    o.tracking = None;
                }
                o.highlight(highlight as i16);
            }
            BEGIN_TRACKING =>
            {
                if o.tracking.is_some()
                {
                    return Err(SERVICE_BUSY);
                }
                part = o.hit(call.point, entry, gate)?;
                if matches!(part,
                            BUTTON_PART
                            | CHECKBOX_PART
                            | SCROLL_UP
                            | SCROLL_DOWN
                            | SCROLL_PAGE_UP
                            | SCROLL_PAGE_DOWN
                            | SCROLL_THUMB)
                {
                    handled = true;
                    o.tracking = Some(Tracking { part,
                                                 anchor: call.point,
                                                 initial: o.spec.scrollbar.value,
                                                 next: call.ticks.saturating_add(20),
                                                 point: call.point,
                                                 hot: true });
                    o.highlight(part);
                    if o.scrollbar && part != SCROLL_THUMB
                    {
                        action = o.step(part);
                    }
                }
            }
            POLL_TRACKING =>
            {
                if let Some(mut t) = o.tracking.take()
                {
                    handled = true;
                    part = t.part;
                    let down = call.flags & BUTTON_DOWN != 0;
                    if o.scrollbar && t.part == SCROLL_THUMB
                    {
                        if call.point != t.point
                        {
                            let mut c = o.spec.call(TRACK_SCROLLBAR);
                            c.scrollbar.value = t.initial;
                            c.anchor = t.anchor;
                            c.point = call.point;
                            super::prepare(&mut c, entry, gate)?;
                            let value = c.result as i16;
                            action = value != o.spec.scrollbar.value;
                            o.spec.scrollbar.value = value;
                        }
                        o.highlight(if down { t.part } else { 0 });
                    }
                    else
                    {
                        let inside = if call.point == t.point
                        {
                            t.hot
                        }
                        else
                        {
                            o.hit(call.point, entry, gate)? == t.part
                        };
                        t.hot = inside;
                        o.highlight(if down && inside { t.part } else { 0 });
                        if o.scrollbar
                        {
                            if down && inside && call.ticks >= t.next
                            {
                                action = o.step(t.part);
                                t.next = call.ticks.saturating_add(3);
                            }
                        }
                        else
                        {
                            action = !down && inside;
                        }
                    }
                    t.point = call.point;
                    if down
                    {
                        o.tracking = Some(t);
                    }
                }
            }
            CANCEL_TRACKING =>
            {
                handled = o.tracking.take().is_some();
                o.highlight(0);
            }
            _ => return Err(NOT_SUPPORTED)
        }
        let changed = previous != (o.spec.standard, o.spec.scrollbar, o.spec.procedure);
        call.window = o.window;
        call.standard = o.spec.standard;
        call.scrollbar = o.spec.scrollbar;
        call.procedure = o.spec.procedure;
        call.step = o.spec.step;
        call.page = o.spec.page;
        call.result = i32::from(part);
        call.flags = (u32::from(o.tracking.is_some()) * TRACKING)
                     | (u32::from(handled) * HANDLED)
                     | (u32::from(action) * ACTION)
                     | (u32::from(changed) * CHANGED);
        if changed || operation == DRAW_OBJECT
        {
            let mut state = o.spec.call(if o.scrollbar { DRAW_SCROLLBAR } else { DRAW_STANDARD });
            let mut drawing = super::prepare_with_title(&mut state, entry, gate, o.title.as_ref())?.ok_or(BAD_ARGUMENT)?;
            drawing.id = call.id;
            if changed
               && operation == CONFIGURE_OBJECT
               && previous.0.bounds == o.spec.standard.bounds
               && previous.2 == o.spec.procedure
            {
                drawing.change_from(previous.0)?;
            }
            Ok(Some(drawing))
        }
        else
        {
            Ok(None)
        }
    }
}
impl Object
{
    fn highlight(&mut self, part: i16)
    {
        if self.scrollbar
        {
            self.spec.scrollbar.highlighted = part;
        }
        else
        {
            self.spec.standard.highlighted = part as u32;
        }
    }

    fn hit(&self, point: Point, entry: DefinitionEntry, gate: TrapGate) -> Result<i16, Status>
    {
        let mut c = self.spec.call(if self.scrollbar { HIT_SCROLLBAR } else { HIT_STANDARD });
        c.point = point;
        super::prepare(&mut c, entry, gate)?;
        Ok(c.result as i16)
    }

    fn step(&mut self, part: i16) -> bool
    {
        let amount = match part
        {
            SCROLL_UP => -i32::from(self.spec.step),
            SCROLL_DOWN => i32::from(self.spec.step),
            SCROLL_PAGE_UP => -i32::from(self.spec.page),
            SCROLL_PAGE_DOWN => i32::from(self.spec.page),
            _ => 0
        };
        let old = self.spec.scrollbar.value;
        self.spec.scrollbar.value =
            (i32::from(old) + amount).clamp(0, i32::from(self.spec.scrollbar.maximum)) as i16;
        old != self.spec.scrollbar.value
    }
}

fn pool_status(error: mac_object_table::domain::PoolError) -> Status
{
    crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(error))
}

#[cfg(target_arch = "riscv64")]
fn publication_status(error: mac_object_table::store::PublicationError) -> Status
{
    match error
    {
        mac_object_table::store::PublicationError::Busy => SERVICE_BUSY,
        mac_object_table::store::PublicationError::Pool(error) => pool_status(error)
    }
}

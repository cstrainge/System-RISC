//! Owned resident controls. CDEFs own geometry; this manager owns identity and
//! tracking policy. No caller memory or callback survives a request.
#![forbid(unsafe_code)]

use alloc::{sync::Arc, vec::Vec};

use mac_abi::{controls::*, definitions::ControlDefinitionState, *};
use sr_control_reflow_handling as layout;

use super::tracking::{Action, Tracking};
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
    state:              State,
    tree:               tree::Node,
    // Last: registered removal must finish payload destruction before the
    // protected modal service can become available to another recipient.
    #[cfg(target_arch = "riscv64")]
    _recovery_activity: Option<crate::recovery::Activity>
}
// Only mutable control state is staged; ownership and recovery pins stay live.
#[derive(Clone)]
struct State
{
    spec:     Spec,
    title:    Option<Arc<super::snapshot::Title>>,
    tracking: Option<Tracking>,
    layout:   Option<Layout>
}
#[derive(Clone)]
struct Layout
{
    sizing:              layout::SizingState,
    visible:             Option<Rect>,
    measured:            Option<(layout::Constraints, layout::Measurement)>,
    arrangement_pending: bool
}
impl Layout
{
    fn new(bounds: Rect) -> Result<Self, Status>
    {
        let bounds = layout::LayoutRect::try_from(bounds).map_err(|e| e.status())?;
        Ok(Self { sizing:              layout::SizingState::new(bounds.size()),
                  visible:             None,
                  measured:            None,
                  arrangement_pending: true })
    }

    fn pending(&self) -> u32
    {
        u32::from(self.measured.is_none()) * mac_abi::controls::reflow::INVALIDATE_MEASUREMENT
        | u32::from(self.arrangement_pending) * mac_abi::controls::reflow::INVALIDATE_ARRANGEMENT
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Spec
{
    Standard
    {
        state: ControlDefinitionState, procedure: i16
    },
    Scrollbar
    {
        state: ScrollbarState, step: i16, page: i16
    }
}
impl Spec
{
    fn from_call(call: &ControlCall, scrollbar: bool) -> Self
    {
        if scrollbar
        {
            Self::Scrollbar { state: call.scrollbar,
                              step:  call.step,
                              page:  call.page }
        }
        else
        {
            Self::Standard { state:     call.standard,
                             procedure: call.procedure }
        }
    }

    fn write(self, call: &mut ControlCall)
    {
        match self
        {
            Self::Standard { state,
                             procedure } =>
            {
                call.standard = state;
                call.procedure = procedure;
                call.scrollbar = Default::default();
                call.step = 0;
                call.page = 0;
            }
            Self::Scrollbar { state,
                              step,
                              page } =>
            {
                call.scrollbar = state;
                call.step = step;
                call.page = page;
                call.standard = Default::default();
                call.procedure = 0;
            }
        }
    }

    fn call(self, operation: u32) -> ControlCall
    {
        let mut call = ControlCall { operation,
                                     ..Default::default() };
        self.write(&mut call);
        call
    }

    fn bounds(self) -> Rect
    {
        match self
        {
            Self::Standard { state,
                             .. } => state.bounds,
            Self::Scrollbar { state,
                              .. } => state.bounds
        }
    }

    fn set_bounds(&mut self, bounds: Rect)
    {
        match self
        {
            Self::Standard { state,
                             .. } => state.bounds = bounds,
            Self::Scrollbar { state,
                              .. } => state.bounds = bounds
        }
    }

    fn scrollbar(self) -> Option<ScrollbarState>
    {
        match self
        {
            Self::Scrollbar { state,
                              .. } => Some(state),
            _ => None
        }
    }

    fn draw(self) -> ControlCall
    {
        self.call(if self.scrollbar().is_some() { DRAW_SCROLLBAR } else { DRAW_STANDARD })
    }

    fn highlight(self) -> u32
    {
        match self
        {
            Self::Standard { state,
                             .. } => state.highlighted,
            Self::Scrollbar { state,
                              .. } => state.highlighted as u32
        }
    }

    fn set_highlight(&mut self, highlight: u32)
    {
        match self
        {
            Self::Standard { state,
                             .. } => state.highlighted = highlight,
            Self::Scrollbar { state,
                              .. } => state.highlighted = highlight as i16
        }
    }

    fn appearance_matches(self, next: Self) -> bool
    {
        match (self, next)
        {
            (Self::Standard { state: a,
                              procedure: p },
             Self::Standard { state: b,
                              procedure: q }) => a == b && p == q,
            (Self::Scrollbar { state: a,
                               .. },
             Self::Scrollbar { state: b,
                               .. }) => a == b,
            _ => false
        }
    }

    fn cancels_capture(self, next: Self, tracking: Option<Tracking>) -> bool
    {
        match (self, next)
        {
            (Self::Standard { state: a,
                              procedure: p },
             Self::Standard { state: b,
                              procedure: q }) =>
            {
                a.bounds != b.bounds || a.enabled != b.enabled || p != q
            }
            (Self::Scrollbar { state: a,
                               .. },
             Self::Scrollbar { state: b,
                               .. }) =>
            {
                a.bounds != b.bounds
                || a.active != b.active
                || a.maximum != b.maximum
                || (tracking.is_some_and(|t| t.part == SCROLL_THUMB) && a.value != b.value)
            }
            _ => true
        }
    }

    fn set_value(&mut self, value: i16) -> bool
    {
        if let Self::Scrollbar { state,
                                 .. } = self
        {
            core::mem::replace(&mut state.value, value) != value
        }
        else
        {
            false
        }
    }

    fn step(&mut self, part: i16) -> bool
    {
        let Self::Scrollbar { state,
                              step,
                              page } = self
        else
        {
            return false;
        };
        let amount = match part
        {
            SCROLL_UP => -i32::from(*step),
            SCROLL_DOWN => i32::from(*step),
            SCROLL_PAGE_UP => -i32::from(*page),
            SCROLL_PAGE_DOWN => i32::from(*page),
            _ => 0
        };
        let value = (i32::from(state.value) + amount).clamp(0, i32::from(state.maximum)) as i16;
        core::mem::replace(&mut state.value, value) != value
    }
}

pub struct Manager
{
    objects: Store<Object>,
    next_order: u64,
    publications: Vec<mac_object_table::store::PublicationReservation<crate::toolbox::handles::InvocationGate>>
}
impl Manager
{
    pub(crate) fn new((kind, storage): (KindLease, KindLease)) -> Result<Self, Status>
    {
        Ok(Self { objects:      Store::owned(kind, storage).map_err(|_| BAD_ARGUMENT)?,
                  publications: Vec::new(),
                  next_order:   1 })
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

    /// Cancellation cannot depend on loading/validating or allocating a drawing.
    pub(crate) fn cancel(&mut self, call: &mut ControlCall, owner: u64) -> Result<(), Status>
    {
        if !call.surface.is_null() || call.reserved != 0
        {
            return Err(BAD_ARGUMENT);
        }
        self.get(call.id, owner)?;
        let object = self.objects.get_mut(call.id).ok_or(NOT_FOUND)?;
        let state = &mut object.state;
        let handled = state.tracking.take().is_some();
        let old = state.spec.highlight();
        state.highlight(0);
        call.window = object.window;
        state.spec.write(call);
        call.reflow = u32::from(state.layout.is_some());
        call.layout.invalidation = state.layout.as_ref().map_or(0, Layout::pending);
        call.result = 0;
        call.flags =
            u32::from(handled) * HANDLED | u32::from(old != state.spec.highlight()) * CHANGED;
        Ok(())
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
               super::definition_id(&Spec::from_call(call, false).draw())?
           }
           else
           {
               let o = self.get(call.id, owner)?;
               super::definition_id(&o.state.spec.draw())?
           })
    }

    pub(crate) fn request(&mut self, recipient: &OwnerRef, storage: &OwnerRef,
                          call: &mut ControlCall, entry: DefinitionEntry, gate: TrapGate)
                          -> Result<Option<super::Drawing>, Status>
    {
        self.request_with(recipient, storage, call, entry, gate, Ok)
    }

    /// The publisher admits and installs retained drawing before state commits.
    /// It must return errors before replacing the old drawing and cannot call apps.
    pub(crate) fn request_with<R>(&mut self, recipient: &OwnerRef, storage: &OwnerRef,
                                  call: &mut ControlCall, entry: DefinitionEntry, gate: TrapGate,
                                  publish: impl FnOnce(super::Drawing) -> Result<R, Status>)
                                  -> Result<Option<R>, Status>
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
        if call.reflow > 1
           || (call.reflow != 0 && !matches!(operation, CREATE_STANDARD | CREATE_SCROLLBAR))
        {
            return Err(BAD_ARGUMENT);
        }
        if !matches!(operation, CREATE_STANDARD | CREATE_SCROLLBAR)
        {
            self.preflight_tree_configuration(call, owner)?;
        }
        let mut admitted_title = None;
        if matches!(operation, CREATE_STANDARD | CREATE_SCROLLBAR | CONFIGURE_OBJECT)
        {
            let scrollbar = if operation == CONFIGURE_OBJECT
            {
                self.get(call.id, owner)?.state.spec.scrollbar().is_some()
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
                spec.reflow = u32::from(old.state.layout.is_some());
                let highlight = old.state.spec.highlight();
                spec.standard.highlighted = highlight;
                spec.scrollbar.highlighted = highlight as i16;
            }
            if operation != CONFIGURE_OBJECT
               || self.get(call.id, owner)?.state.spec != Spec::from_call(&spec, scrollbar)
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
                    self.get(call.id, owner)?.state.title.as_ref()
                }
                else
                {
                    None
                };
                admitted_title = Some(super::snapshot::Title::admit(&spec.standard, previous)?);
            }
            if matches!(operation, CREATE_STANDARD | CREATE_SCROLLBAR)
            {
                let next_order = self.next_order.checked_add(1).ok_or(NO_MEMORY)?;
                let object = Object { tree:
                                          tree::Node::new(self.next_order),
                                      window:
                                          call.window,
                                      recipient:                                          owner,
                                      state:
                                          State { spec:     Spec::from_call(&spec, scrollbar),
                                                  title:    admitted_title.clone(),
                                                  tracking: None,
                                                  layout:   if call.reflow == 1
                                                  {
                                                      Some(Layout::new(Spec::from_call(&spec, scrollbar).bounds())?)
                                                  }
                                                  else
                                                  {
                                                      None
                                                  } },
                                      #[cfg(target_arch = "riscv64")]
                                      _recovery_activity:
                                          if recovery
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
                self.next_order = next_order;
            }
        }
        let object = self.get(call.id, owner)?;
        if operation == BEGIN_TRACKING && call.window != 0 && call.window != object.window
        {
            return Err(BAD_ARGUMENT);
        }
        if operation == DISPOSE_OBJECT
        {
            if object.tree.parent != 0
            {
                return Err(ACCESS_DENIED);
            }
            self.remove_tree(call.id)?;
            return Ok(None);
        }
        if operation == BEGIN_TRACKING
           && self.objects.iter().any(|(id, o)| {
                                     id != call.id
                                     && o.recipient == owner
                                     && o.state.tracking.is_some()
                                 })
        {
            return Err(SERVICE_BUSY);
        }
        let inherited_clip = self.inherited_clip(call.id);
        let object = self.objects.get_mut(call.id).ok_or(NOT_FOUND)?;
        let mut next = object.state.clone();
        let o = &mut next;
        let previous = o.spec;
        let previous_visible =
            o.layout
             .as_ref()
             .and_then(|layout| layout.visible)
             .map_or(previous.bounds(), |clip| super::intersection(previous.bounds(), clip));
        let mut action = false;
        let mut handled = false;
        let mut part = 0;
        let mut visibility_changed = false;
        match operation
        {
            CREATE_STANDARD | CREATE_SCROLLBAR | INFO_OBJECT | DRAW_OBJECT => (),
            REFLOW_OBJECT =>
            {
                visibility_changed = o.reflow(&mut call.layout, entry, gate)?;
            }
            CONFIGURE_OBJECT =>
            {
                // New title backing was admitted before any state/capture mutation.
                o.title = admitted_title;
                let next = Spec::from_call(call, o.spec.scrollbar().is_some());
                let cancel = o.spec.cancels_capture(next, o.tracking);
                let highlight = if cancel { 0 } else { o.spec.highlight() };
                o.spec = next;
                if cancel
                {
                    o.tracking = None;
                }
                o.spec.set_highlight(highlight);
                if previous != o.spec
                   && let Some(layout) = &mut o.layout
                {
                    layout.measured = None;
                    layout.arrangement_pending = true;
                }
            }
            BEGIN_TRACKING =>
            {
                if o.tracking.is_some()
                {
                    return Err(SERVICE_BUSY);
                }
                part = if inherited_clip.is_some_and(|clip| !clip.contains(call.point))
                {
                    0
                }
                else
                {
                    o.hit(call.point, entry, gate)?
                };
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
                    o.tracking =
                        Some(Tracking::new(part,
                                           call.point,
                                           o.spec.scrollbar().map_or(0, |state| state.value),
                                           call.ticks));
                    o.highlight(part);
                    if o.spec.scrollbar().is_some() && part != SCROLL_THUMB
                    {
                        action = o.spec.step(part);
                    }
                }
            }
            POLL_TRACKING =>
            {
                if let Some(tracking) = o.tracking
                {
                    handled = true;
                    part = tracking.part;
                    let transition = tracking.poll(call.point,
                                                   call.flags & BUTTON_DOWN != 0,
                                                   call.ticks,
                                                   o.spec.scrollbar(),
                                                   u32::from(o.layout.is_some()),
                                                   entry,
                                                   gate,
                                                   |point| {
                                                       if inherited_clip.is_some_and(|clip| {
                                                                            !clip.contains(point)
                                                                        })
                                                       {
                                                           Ok(0)
                                                       }
                                                       else
                                                       {
                                                           o.hit(point, entry, gate)
                                                       }
                                                   })?;
                    action = match transition.action
                    {
                        Action::None => false,
                        Action::Activate => true,
                        Action::Step(part) => o.spec.step(part),
                        Action::Value(value) => o.spec.set_value(value)
                    };
                    o.highlight(transition.highlight);
                    o.tracking = transition.tracking;
                }
            }
            CANCEL_TRACKING =>
            {
                handled = o.tracking.take().is_some();
                o.highlight(0);
            }
            _ => return Err(NOT_SUPPORTED)
        }
        let changed = visibility_changed || !previous.appearance_matches(o.spec);
        call.window = object.window;
        call.relation = object.tree.relation();
        o.spec.write(call);
        call.reflow = u32::from(o.layout.is_some());
        call.layout.invalidation = o.layout.as_ref().map_or(0, Layout::pending);
        call.result = i32::from(part);
        call.flags = (u32::from(o.tracking.is_some()) * TRACKING)
                     | (u32::from(handled) * HANDLED)
                     | (u32::from(action) * ACTION)
                     | (u32::from(changed) * CHANGED);
        let drawing = if changed || operation == DRAW_OBJECT
        {
            let mut state = o.spec.draw();
            state.reflow = u32::from(o.layout.is_some());
            let mut drawing = super::prepare_with_title(&mut state, entry, gate, o.title.as_ref())?.ok_or(BAD_ARGUMENT)?;
            drawing.id = call.id;
            if changed
               && operation == CONFIGURE_OBJECT
               && let (Spec::Standard { state: before,
                                        procedure: old },
                       Spec::Standard { state: after,
                                        procedure: new }) = (previous, o.spec)
               && before.bounds == after.bounds
               && old == new
            {
                drawing.change_from(before)?;
            }
            drawing.set_visible(tree::clip(o.layout.as_ref().and_then(|layout| layout.visible),
                                           inherited_clip));
            if visibility_changed
            {
                drawing.damage = [previous_visible, drawing.visible_bounds()];
            }
            Some(publish(drawing)?)
        }
        else
        {
            None
        };
        // No fallible work after publication. A refusal above drops only staging.
        let parent = object.tree.parent;
        let changed_requirements =
            object.state.layout.as_ref().map(|v| (v.sizing.requirements(), v.measured.is_none()))
            != next.layout.as_ref().map(|v| (v.sizing.requirements(), v.measured.is_none()));
        object.state = next;
        if changed_requirements
        {
            self.invalidate_ancestors(parent);
        }
        Ok(drawing)
    }
}
impl State
{
    fn highlight(&mut self, part: i16)
    {
        self.spec.set_highlight(part as u32);
    }

    fn hit(&self, point: Point, entry: DefinitionEntry, gate: TrapGate) -> Result<i16, Status>
    {
        let mut call = self.spec.call(if self.spec.scrollbar().is_some()
                                      {
                                          HIT_SCROLLBAR
                                      }
                                      else
                                      {
                                          HIT_STANDARD
                                      });
        if self.layout
               .as_ref()
               .and_then(|layout| layout.visible)
               .is_some_and(|clip| !clip.contains(point))
        {
            return Ok(0);
        }
        call.reflow = u32::from(self.layout.is_some());
        call.point = point;
        super::prepare(&mut call, entry, gate)?;
        Ok(call.result as i16)
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

mod reflow;

mod tree;

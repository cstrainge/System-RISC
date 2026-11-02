//! Owned resident controls. CDEFs own geometry; this manager owns identity and
//! tracking policy. No caller memory or callback survives a request.
#![forbid(unsafe_code)]

use alloc::{sync::Arc, vec::Vec};

use mac_abi::{controls::*,
              definitions::ControlDefinitionState,
              geometry::{Point, Rect},
              *};
use sr_control_reflow_handling as layout;

use super::tracking::{Action, Tracking};
use crate::toolbox::{handles::{CapacityPolicy, KindCode, KindLease, OwnerRef, Store},
                     resources::definitions::{BoundDefinition, DefinitionRef}};
#[cfg(target_arch = "riscv64")]
#[path = "admission.rs"]
mod admission;
#[cfg(target_arch = "riscv64")]
pub(crate) use admission::{ControlCommit, ControlPlan, CreationError};

/// Recovery placement follows a genuine active Session and its original I/O
/// root, not inequality of recipient/storage labels. A System report has the
/// SAME genuine System owner in both roles and still uses admitted endpoints.
fn recovery_mode(recipient: &OwnerRef, storage: &OwnerRef) -> Result<bool, Status>
{
    #[cfg(target_arch = "riscv64")]
    if let Some(activity) = crate::context::recovery_activity()
    {
        if activity.recipient().owner() != recipient.owner()
           || activity.storage_owner().owner() != storage.owner()
           || !crate::context::io_subject().is_some_and(|subject| {
                                               subject.scope()
                                                      .same_scope(activity.root_subject().scope())
                                           })
        {
            return Err(ACCESS_DENIED);
        }
        return Ok(true);
    }
    if storage.owner() != recipient.owner()
    {
        return Err(ACCESS_DENIED);
    }
    Ok(false)
}


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
impl<C: ControlHost>
    mac_object_table::registry::Release<C, crate::toolbox::handles::cleanup::Failure>
    for ReleaseStorage
{
    fn release(&self, context: &mut C, ticket: mac_object_table::domain::ReleasedResource)
               -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        use crate::toolbox::handles::cleanup::Failure;
        let objects = &mut context.managed_controls().objects;
        let preparing = objects.publication_preparations(ticket.key().owner());
        if preparing != 0
        {
            return Err(Failure::publication_preparations(ticket, preparing));
        }
        objects.release_storage(ticket).map_err(Failure::from)
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
    automatic:          automatic::Automatic,
    // Keep the genuine callable pinned through state/tree payload destruction.
    definition:         BoundDefinition,
    // Last: registered removal must finish payload destruction before the
    // protected modal service can become available to another recipient.
    #[cfg(target_arch = "riscv64")]
    _recovery_activity: Option<crate::recovery::Activity>
}
// Only mutable control state is staged; ownership and recovery pins stay live.
#[derive(Clone)]
struct State
{
    spec:             Spec,
    title:            Option<Arc<super::snapshot::Title>>,
    tracking:         Option<Tracking>,
    splitter:         Option<splitter::Capture>,
    scroll_axis:      Option<usize>,
    scroll_highlight: i16,
    layout:           Option<Layout>
}
#[derive(Clone)]
struct Layout
{
    sizing:              layout::SizingState,
    anchors:             layout::Anchors,
    window_border:       layout::BorderOffsets,
    window_extent:       Option<(i16, i16)>,
    visible:             Option<Rect>,
    measured:            Option<(layout::Constraints, layout::Measurement)>,
    intrinsic:           Option<layout::Measurement>,
    grid:                Option<Arc<grid::Configuration>>,
    outlines:            Option<Arc<super::snapshot::Grid>>,
    scroll:              Option<Arc<scrolling::Scroll>>,
    logical:             mac_abi::controls::reflow::LogicalRect,
    hidden:              bool,
    arrangement_pending: bool
}
impl Layout
{
    fn new(bounds: Rect) -> Result<Self, Status>
    {
        let bounds = layout::LayoutRect::try_from(bounds).map_err(|e| e.status())?;
        Ok(Self { window_border:       layout::BorderOffsets::ZERO,
                  window_extent:       None,
                  anchors:             layout::Anchors::NONE,
                  sizing:              layout::SizingState::new(bounds.size()),
                  visible:             None,
                  measured:            None,
                  intrinsic:           None,
                  grid:                None,
                  outlines:            None,
                  scroll:              None,
                  logical:             Default::default(),
                  hidden:              false,
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
                a.bounds != b.bounds
                || a.enabled != b.enabled
                || p != q
                || (p == SPLITTER_PROCEDURE
                    && (a.flags != b.flags
                        || a.minimum != b.minimum
                        || a.maximum != b.maximum
                        || a.value != b.value
                        || a.splitter_parent != b.splitter_parent))
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
    objects: Store<Object, automatic::Work>,
    next_order: u64,
    publications: Vec<mac_object_table::store::PublicationReservation<crate::toolbox::handles::InvocationGate>>
}
impl Manager
{
    pub(crate) fn new((kind, storage): (KindLease, KindLease)) -> Result<Self, Status>
    {
        Ok(Self { objects:      Store::owned_state_prepared(kind, storage, |_| {
                                    Ok(automatic::Work::default())
                                }).map_err(pool_status)?,
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
        let layout_status = self.layout_status(call.id);
        let object = self.objects.get_mut(call.id).ok_or(NOT_FOUND)?;
        let state = &mut object.state;
        let handled = state.tracking.take().is_some();
        let old_scroll_highlight = state.scroll_highlight;
        state.scroll_highlight = 0;
        state.scroll_axis = None;
        state.splitter = None;
        let old = state.spec.highlight();
        state.highlight(0);
        call.window = object.window;
        state.spec.write(call);
        call.reflow = u32::from(state.layout.is_some());
        call.layout.anchors = state.layout.as_ref().map_or(0, |l| l.anchors.into());
        call.layout.window_border =
            state.layout.as_ref().map_or(Default::default(), |l| l.window_border.into());
        call.layout.invalidation = state.layout.as_ref().map_or(0, Layout::pending);
        call.layout.status = layout_status;
        call.result = 0;
        call.flags = u32::from(handled) * HANDLED
                     | u32::from(old != state.spec.highlight() || old_scroll_highlight != 0)
                       * CHANGED;
        self.wake_capture(call.id);
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
        if call.operation == CREATE_SCROLLBAR
        {
            return Ok(1);
        }
        if call.operation == CREATE_STANDARD
        {
            return super::definition_id(&Spec::from_call(call, false).draw());
        }
        super::definition_id(&self.get(call.id, owner)?.state.spec.draw())
    }

    /// Validate the real retained object's auxiliary CDEF need before the
    /// native coordinator releases Desktop to load a cold dependency.
    pub(crate) fn auxiliary_definition(&self, call: &ControlCall, owner: u64)
                                       -> Result<Option<i16>, Status>
    {
        use mac_abi::controls::reflow as wire;
        if call.operation != REFLOW_OBJECT
        {
            return Ok(None);
        }
        let object = self.get(call.id, owner)?;
        if !call.surface.is_null()
           || call.reserved != 0
           || call.reflow != 0
           || (call.window != 0 && call.window != object.window)
        {
            return Err(BAD_ARGUMENT);
        }
        let layout = object.state.layout.as_ref().ok_or(NOT_SUPPORTED)?;
        if call.layout.reserved != 0
        {
            return Err(BAD_ARGUMENT);
        }
        match call.layout.message
        {
            wire::SET_SCROLLING =>
            {
                self.table(call.id, owner)?;
                let axes = call.layout.scroll.axes;
                if axes & !3 != 0
                {
                    return Err(BAD_ARGUMENT);
                }
                Ok((axes != 0 && layout.scroll.is_none()).then_some(1))
            }
            wire::SET_SPLITTER =>
            {
                self.table(call.id, owner)?;
                let setting = call.layout.splitter;
                if setting.axis > 1
                   || setting.index >= u32::MAX - 1
                   || setting.enabled > 1
                   || setting.reserved != 0
                {
                    return Err(BAD_ARGUMENT);
                }
                Ok((setting.enabled != 0
                    && layout.grid.as_ref().is_none_or(|grid| !grid.has_splitters()))
                   .then_some(CDEF_SPLITTER))
            }
            _ => Ok(None)
        }
    }

    pub(crate) fn bound_definition(&self, call: &ControlCall, owner: u64)
                                   -> Result<Option<DefinitionRef>, Status>
    {
        if matches!(call.operation, CREATE_STANDARD | CREATE_SCROLLBAR)
        {
            return Ok(None);
        }
        let object = self.get(call.id, owner)?;
        if call.operation == CONFIGURE_OBJECT
        {
            let previous = super::definition_id(&object.state.spec.draw())?;
            let mut proposed = *call;
            proposed.operation = if object.state.spec.scrollbar().is_some()
            {
                DRAW_SCROLLBAR
            }
            else
            {
                DRAW_STANDARD
            };
            if super::definition_id(&proposed)? != previous
            {
                return Ok(None);
            }
        }
        Ok(Some(object.definition.reference()))
    }

    /// Pure validation before loading a cold definition or detached creation.
    pub(crate) fn preflight_definition(&self, recipient: &OwnerRef, storage: &OwnerRef,
                                       call: &ControlCall)
                                       -> Result<(i16, Option<DefinitionRef>), Status>
    {
        let owner = recipient.owner().incarnation();
        let recovery = recovery_mode(recipient, storage)?;
        let creation = matches!(call.operation, CREATE_STANDARD | CREATE_SCROLLBAR);
        if creation && (!recipient.accepting() || !storage.accepting())
        {
            return Err(ACCESS_DENIED);
        }
        if !call.surface.is_null()
           || call.reserved != 0
           || call.reflow > 1
           || (!creation && call.reflow != 0)
        {
            return Err(BAD_ARGUMENT);
        }
        let _ = recovery;
        if matches!(call.operation, CREATE_STANDARD | CREATE_SCROLLBAR | CONFIGURE_OBJECT)
        {
            let old = if call.operation == CONFIGURE_OBJECT
            {
                Some(self.get(call.id, owner)?)
            }
            else
            {
                None
            };
            let scrollbar = old.map_or(call.operation == CREATE_SCROLLBAR, |o| {
                                   o.state.spec.scrollbar().is_some()
                               });
            if scrollbar && (call.step <= 0 || call.page <= 0)
            {
                return Err(BAD_ARGUMENT);
            }
            let mut spec = *call;
            spec.operation = if scrollbar { DRAW_SCROLLBAR } else { DRAW_STANDARD };
            if let Some(old) = old
            {
                spec.reflow = u32::from(old.state.layout.is_some());
                spec.standard.highlighted = old.state.spec.highlight();
                spec.scrollbar.highlighted = old.state.spec.highlight() as i16;
            }
            let id = super::preflight(&spec)?;
            let bound = match old
            {
                Some(old) if super::definition_id(&old.state.spec.draw())? == id =>
                {
                    Some(old.definition.reference())
                }
                _ => None
            };
            return Ok((id, bound));
        }
        self.preflight_tree_configuration(call, owner)?;
        let object = self.get(call.id, owner)?;
        if call.operation == BEGIN_TRACKING
           && (call.window != 0 && call.window != object.window
               || object.state.tracking.is_some()
               || self.objects.iter().any(|(id, o)| {
                                         id != call.id
                                         && o.recipient == owner
                                         && o.state.tracking.is_some()
                                     }))
        {
            return Err(SERVICE_BUSY);
        }
        Ok((super::definition_id(&object.state.spec.draw())?, Some(object.definition.reference())))
    }

    pub(crate) fn request(&mut self, recipient: &OwnerRef, storage: &OwnerRef,
                          call: &mut ControlCall, entry: DefinitionRef, gate: TrapGate)
                          -> Result<Option<super::Drawing>, Status>
    {
        self.request_with(recipient, storage, call, entry, gate, Ok)
    }

    /// The publisher admits and installs retained drawing before state commits.
    /// It must return errors before replacing the old drawing and cannot call apps.
    pub(crate) fn request_with<R>(&mut self, recipient: &OwnerRef, storage: &OwnerRef,
                                  call: &mut ControlCall, entry: DefinitionRef, gate: TrapGate,
                                  publish: impl FnOnce(super::Drawing) -> Result<R, Status>)
                                  -> Result<Option<R>, Status>
    {
        let owner = recipient.owner().incarnation();
        let recovery = recovery_mode(recipient, storage)?;
        if recovery
           && matches!(call.operation, CREATE_STANDARD | CREATE_SCROLLBAR)
           && !recipient.accepting()
        {
            return Err(ACCESS_DENIED);
        }
        if call.operation == SET_STATIC_TEXT_OBJECT
        {
            if call.flags == 0 || call.flags & !TEXT_PROPERTIES != 0 || call.reflow != 0
            {
                return Err(BAD_ARGUMENT);
            }
            let Spec::Standard { mut state,
                                 procedure: STATIC_TEXT_PROCEDURE } =
                self.get(call.id, owner)?.state.spec
            else
            {
                return Err(NOT_SUPPORTED);
            };
            if call.flags & TEXT_CONTENT != 0
            {
                state.title = call.standard.title;
                state.title_length = call.standard.title_length;
            }
            if call.flags & TEXT_FONT != 0
            {
                state.font = call.standard.font;
            }
            if call.flags & TEXT_ALIGNMENT != 0
            {
                state.value = call.standard.value;
            }
            if call.flags & TEXT_LAYOUT != 0
            {
                state.flags = call.standard.flags;
            }
            if call.flags & TEXT_BOUNDS != 0
            {
                state.bounds = call.standard.bounds;
            }
            call.standard = state;
            call.procedure = STATIC_TEXT_PROCEDURE;
            call.operation = CONFIGURE_OBJECT;
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
        let (_, bound) = self.preflight_definition(recipient, storage, call)?;
        let entry = bound.unwrap_or(entry);
        let replacement = if operation == CONFIGURE_OBJECT && bound.is_none()
        {
            Some(crate::toolbox::resources::definitions::bind(entry)?)
        }
        else
        {
            None
        };
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
                let definition = crate::toolbox::resources::definitions::bind(entry)?;
                let object = Object::new(call.window,
                                         owner,
                                         Spec::from_call(&spec, scrollbar),
                                         admitted_title.clone(),
                                         definition,
                                         call.reflow,
                                         self.next_order,
                                         recovery)?;
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
                self.schedule_layout(call.id);
            }
        }
        self.finish_request_with(call, owner, entry, gate, admitted_title, replacement, publish)
    }

    fn validate_spec(&self, owner: u64, call: &ControlCall, entry: DefinitionRef, gate: TrapGate)
                     -> Result<(Spec, bool), Status>
    {
        let scrollbar = if call.operation == CONFIGURE_OBJECT
        {
            self.get(call.id, owner)?.state.spec.scrollbar().is_some()
        }
        else
        {
            call.operation == CREATE_SCROLLBAR
        };
        let mut spec = *call;
        spec.operation = if scrollbar { DRAW_SCROLLBAR } else { DRAW_STANDARD };
        if call.operation == CONFIGURE_OBJECT
        {
            let old = self.get(call.id, owner)?;
            spec.reflow = u32::from(old.state.layout.is_some());
            spec.standard.highlighted = old.state.spec.highlight();
            spec.scrollbar.highlighted = old.state.spec.highlight() as i16;
        }
        if call.operation != CONFIGURE_OBJECT
           || self.get(call.id, owner)?.state.spec != Spec::from_call(&spec, scrollbar)
        {
            super::validate(&mut spec, entry, gate)?;
        }
        Ok((Spec::from_call(&spec, scrollbar), scrollbar))
    }

    fn finish_request(&mut self, call: &mut ControlCall, owner: u64, entry: DefinitionRef,
                      gate: TrapGate, admitted_title: Option<Arc<super::snapshot::Title>>,
                      replacement: Option<BoundDefinition>)
                      -> Result<Option<super::Drawing>, Status>
    {
        self.finish_request_with(call, owner, entry, gate, admitted_title, replacement, Ok)
    }

    fn finish_request_with<R>(&mut self, call: &mut ControlCall, owner: u64, entry: DefinitionRef,
                              gate: TrapGate, admitted_title: Option<Arc<super::snapshot::Title>>,
                              mut replacement: Option<BoundDefinition>,
                              publish: impl FnOnce(super::Drawing) -> Result<R, Status>)
                              -> Result<Option<R>, Status>
    {
        let operation = call.operation;
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
        let layout_status = self.layout_status(call.id);
        let object = self.objects.get_mut(call.id).ok_or(NOT_FOUND)?;
        let mut next = object.state.clone();
        let o = &mut next;
        let previous = o.spec;
        let previous_visible =
            o.layout
             .as_ref()
             .and_then(Layout::clip)
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
                let assigned_hidden = call.layout.message == mac_abi::controls::reflow::ASSIGN_RECT
                                      && o.layout.as_ref().is_some_and(|l| l.hidden);
                if assigned_hidden && let Some(l) = &mut o.layout
                {
                    l.hidden = false;
                }
                visibility_changed = o.reflow(&mut call.layout, entry, gate)? || assigned_hidden;
            }
            SET_TABLE_LINES_OBJECT =>
            {
                if call.standard.flags & !TABLE_LINE_FLAGS != 0
                {
                    return Err(BAD_ARGUMENT);
                }
                let Spec::Standard { state,
                                     procedure: TABLE_PROCEDURE } = &mut o.spec
                else
                {
                    return Err(NOT_SUPPORTED);
                };
                state.flags = call.standard.flags;
            }
            CONFIGURE_OBJECT =>
            {
                // New title backing was admitted before any state/capture mutation.
                o.title = admitted_title;
                let next = Spec::from_call(call, o.spec.scrollbar().is_some());
                let cancel = o.spec.cancels_capture(next, o.tracking);
                let highlight = if cancel { 0 } else { o.spec.highlight() };
                o.spec = next;
                if previous.bounds() != o.spec.bounds()
                   && let Some(l) = &mut o.layout
                {
                    l.hidden = false;
                }
                if cancel
                {
                    o.tracking = None;
                    o.scroll_axis = None;
                    o.splitter = None;
                    o.scroll_highlight = 0;
                }
                o.spec.set_highlight(highlight);
                if previous != o.spec
                   && let Some(layout) = &mut o.layout
                {
                    layout.measured = None;
                    layout.intrinsic = None;
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
        call.layout.anchors = o.layout.as_ref().map_or(0, |l| l.anchors.into());
        call.layout.window_border =
            o.layout.as_ref().map_or(Default::default(), |l| l.window_border.into());
        call.layout.invalidation = o.layout.as_ref().map_or(0, Layout::pending);
        call.layout.status = layout_status;
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
            drawing.grid = o.layout.as_ref().and_then(|l| l.outlines.clone());
            Self::scroll_drawing(o, &mut drawing)?;
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
            drawing.set_visible(tree::clip(o.layout.as_ref().and_then(Layout::clip),
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
            object.state.layout.as_ref().map(|v| {
                                            (v.sizing.requirements(),
                                             v.measured.is_none(),
                                             v.anchors)
                                        })
            != next.layout
                   .as_ref()
                   .map(|v| (v.sizing.requirements(), v.measured.is_none(), v.anchors));
        let changed_geometry = object.state.spec.bounds() != next.spec.bounds();
        let newly_pending = next.layout.as_ref().is_some_and(|l| l.arrangement_pending)
                            && object.state.layout.as_ref().is_some_and(|l| !l.arrangement_pending);
        if changed_geometry && let Some(l) = &mut next.layout
        {
            l.window_extent = None;
        }
        let released_capture = object.state.tracking.is_some() && next.tracking.is_none();
        let configured = operation == CONFIGURE_OBJECT && previous != next.spec;
        let retired_binding =
            replacement.take().map(|next| core::mem::replace(&mut object.definition, next));
        object.state = next;
        if changed_requirements
        {
            self.invalidate_ancestors(parent);
        }
        else if changed_geometry || newly_pending
        {
            self.invalidate_arrangement_ancestors(parent);
        }
        if changed_requirements
           || changed_geometry
           || newly_pending
           || configured
           || (operation == REFLOW_OBJECT
               && call.layout.message == mac_abi::controls::reflow::INVALIDATE_LAYOUT)
        {
            self.schedule_layout(call.id);
        }
        if released_capture
        {
            self.wake_capture(call.id);
        }
        self.layout_committed(call.id);
        drop(retired_binding);
        Ok(drawing)
    }
}
impl Object
{
    fn new(window: u64, recipient: u64, spec: Spec, title: Option<Arc<super::snapshot::Title>>,
           definition: BoundDefinition, reflow: u32, order: u64, recovery: bool)
           -> Result<Self, Status>
    {
        #[cfg(not(target_arch = "riscv64"))]
        let _ = recovery;
        Ok(Self { window,
                  recipient,
                  definition,
                  tree: tree::Node::new(order),
                  automatic: automatic::Automatic::default(),
                  state: State { spec,
                                 title,
                                 tracking: None,
                                 splitter: None,
                                 scroll_axis: None,
                                 scroll_highlight: 0,
                                 layout: if reflow == 1
                                 {
                                     Some(Layout::new(spec.bounds())?)
                                 }
                                 else
                                 {
                                     None
                                 } },
                  #[cfg(target_arch = "riscv64")]
                  _recovery_activity: if recovery
                  {
                      crate::context::recovery_activity()
                  }
                  else
                  {
                      None
                  } })
    }
}
impl State
{
    fn highlight(&mut self, part: i16)
    {
        self.spec.set_highlight(part as u32);
    }

    fn hit(&self, point: Point, entry: DefinitionRef, gate: TrapGate) -> Result<i16, Status>
    {
        let mut call = self.spec.call(if self.spec.scrollbar().is_some()
                                      {
                                          HIT_SCROLLBAR
                                      }
                                      else
                                      {
                                          HIT_STANDARD
                                      });
        if self.layout.as_ref().and_then(Layout::clip).is_some_and(|clip| !clip.contains(point))
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

mod grid;
pub(crate) use grid::Prepared;
mod window;
mod automatic;
mod scrolling;

mod splitter;

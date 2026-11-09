//! Ordinary Process window admission into the existing canonical WIND Store.
//! The reserved System-modal contract remains separate; both births use the
//! same Window payload and WDEF initialization/rollback implementation.
#![forbid(unsafe_code)]
use mac_object_table::{domain::DomainKind,
                       store::{PreparedPublication, PublicationPermit, PublicationPlan,
                               RetiredCapacity, RetiredPublication}};

use super::*;
use crate::machine::scheduler::accounting::AllocationAuthority;

#[derive(Clone, Copy)]
struct Intent(mac_abi::WindowRequest);
impl Intent
{
    fn minimum(&self) -> (i16, i16)
    {
        (self.0.minimum_width, self.0.minimum_height)
    }

    fn matches(&self, request: &mac_abi::WindowRequest) -> bool
    {
        self.0.bounds == request.bounds
        && self.0.procedure == request.procedure
        && self.0.minimum_width == request.minimum_width
        && self.0.minimum_height == request.minimum_height
        && self.0.reserved == request.reserved
        && self.0.title_length == request.title_length
        && request.title_length as usize <= request.title.len()
        && self.0.title[..self.0.title_length as usize]
           == request.title[..request.title_length as usize]
    }
}

pub(crate) struct WindowPlan
{
    owner:       OwnerRef,
    intent:      Intent,
    entry:       DefinitionRef,
    gate:        mac_abi::TrapGate,
    publication: PublicationPlan<OwnedWindow, InvocationGate>,
    capacity:    WindowCapacityPlan
}
/// Fresh metadata and a bound original WDEF, before the Store admits a slot.
#[must_use = "dispose partial window preparation outside Desktop"]
pub(crate) struct WindowPreparation
{
    window:      OwnedWindow,
    capacity:    WindowCapacity,
    owner:       OwnerRef,
    intent:      Intent,
    publication: PreparedPublication<OwnedWindow, InvocationGate>,
    allocation:  AllocationAuthority
}
/// A real unpublished slot/credit and its exact bound payload. Capacity refresh
/// replaces empty WM arrays only, never this permit or the original account.
#[must_use = "retain or dispose the actual window permit outside Desktop"]
pub(crate) struct PreparedWindow
{
    window:     OwnedWindow,
    capacity:   WindowCapacity,
    owner:      OwnerRef,
    intent:     Intent,
    admission:  RetiredCapacity<OwnedWindow, InvocationGate>,
    permit:     PublicationPermit<InvocationGate>,
    allocation: AllocationAuthority
}
pub(crate) struct WindowCapacityPlan
{
    visibility:     super::super::visibility::CapacityPlan,
    order:          usize,
    order_scratch:  usize,
    geometry_order: usize,
    failures:       usize,
    removals:       usize
}
/// On return from installation, moved-out arrays own no live windows/tickets.
#[must_use = "dispose fresh or displaced WM arrays outside Desktop"]
pub(crate) struct WindowCapacity
{
    visibility:     super::super::visibility::PreparedCapacity,
    order:          Vec<WindowId>,
    order_scratch:  Vec<WindowId>,
    geometry_order: Vec<WindowId>,
    failures:       VecDeque<mac_object_table::domain::CompletionError>,
    removals:       VecDeque<super::operations::RemovalDebt>
}
/// Returned on both successful WDEF initialization and postbirth rollback.
/// Actual rollback still uses InitializingWindow's existing Dispose/quarantine
/// path under Desktop; it is not claimed to be a guard-free cleanup migration.
#[must_use = "dispose window publication metadata outside Desktop"]
pub(crate) struct WindowRetired
{
    capacity:    WindowCapacity,
    admission:   RetiredCapacity<OwnedWindow, InvocationGate>,
    publication: RetiredPublication<InvocationGate>,
    owner:       OwnerRef,
    allocation:  AllocationAuthority
}
#[must_use = "inspect WDEF result and dispose retired metadata outside Desktop"]
pub(crate) struct WindowCommit
{
    pub(crate) result:  Result<WindowId, CreationError>,
    pub(crate) retired: WindowRetired
}
impl WindowCommit
{
    pub(crate) fn dispose(self)
    {
        self.retired.dispose();
    }
}
impl WindowPlan
{
    /// Run with the original caller's recovery placement selection, outside
    /// Desktop. Only shared arrays/indexes select System allocation internally.
    pub(crate) fn prepare(self, allocation: &AllocationAuthority)
                          -> Result<WindowPreparation, CreationError>
    {
        if allocation.owner() != self.owner.owner()
        {
            return Err(CreationError::Facility(mac_object_table::domain::PoolError::ForeignOwner {
                expected: self.owner.owner(), actual: allocation.owner()
            }));
        }
        if !self.owner.accepting()
        {
            return Err(CreationError::Stopping);
        }
        crate::allocator::with_target(crate::allocator::Target::Owner {
            account: allocation.clone(), class: mac_kernel_model::policy::MemoryClass::Kernel
        }, || {
            let binding = crate::toolbox::resources::definitions::bind(self.entry)
                .map_err(CreationError::Binding)?;
            let publication = self.publication.prepare().map_err(CreationError::Facility)?;
            let capacity = self.capacity.prepare()?;
            let window = OwnedWindow::new(self.intent.0.bounds, self.intent.0.title(),
                                           self.owner.owner().incarnation(), self.intent.minimum(),
                                           self.intent.0.procedure, self.gate, binding, self.intent.0.reserved == mac_abi::WINDOW_STAY_IN_BACK,
                                           WindowSource::Application);
            Ok(WindowPreparation { window, capacity, owner: self.owner,
                                   allocation: allocation.clone(), intent: self.intent, publication })
        })
    }
}
impl WindowPreparation
{
    pub(crate) fn owner(&self) -> &OwnerRef
    {
        &self.owner
    }

    pub(crate) fn matches_request(&self, request: &mac_abi::WindowRequest) -> bool
    {
        self.intent.matches(request)
    }

    pub(crate) fn dispose(self)
    {
        drop(self);
    }
}
impl PreparedWindow
{
    pub(crate) fn prepare_trusted_cleanup(&mut self,
                                          notice: alloc::sync::Arc<crate::toolbox::resources::definitions::callbacks::Notice>)
                                          -> Result<(), Status>
    {
        if self.window.trusted_close.is_some()
        {
            return Ok(());
        }
        if self.user_definition().map_err(|_| mac_abi::BAD_ABI)?.is_some()
        {
            return Ok(());
        }
        let mut window = self.window.window;
        window.id = WindowId(self.permit.id());
        let plan = super::trusted::TrustedClosePlan::from_birth(window,
                                                                self.owner.owner(),
                                                                self.window
                                                                    .revision
                                                                    .ok_or(mac_abi::BAD_ABI)?);
        self.window.trusted_close =
            Some(plan.prepare(&self.owner, &self.allocation, notice, None)?);
        Ok(())
    }

    /// Guard-free, fallible cleanup stock before any author NEW side effects.
    pub(crate) fn prepare_user_cleanup(&mut self,
                                       notice: alloc::sync::Arc<crate::toolbox::resources::definitions::callbacks::Notice>)
                                       -> Result<(), Status>
    {
        if self.user_definition()
               .map_err(|cause| {
                   match cause
                   {
                       CreationError::Binding(status) => status,
                       _ => mac_abi::BAD_ABI
                   }
               })?
               .is_none()
        {
            return Ok(());
        }
        if self.window.cleanup_job.is_none()
        {
            let job = crate::allocator::with_target(crate::allocator::Target::Owner {
                account: self.allocation.clone(), class: mac_kernel_model::policy::MemoryClass::Kernel
            }, || super::operations::WindowJob::prepare_cleanup(self.window.window,
                WindowId(self.permit.id()), &self.owner, notice.clone()))?;
            self.window.cleanup_job = Some(job);
        }
        if self.window.paint.is_none()
        {
            let guardian = self.window.canvas_guardian()?;
            self.window.paint = Some(crate::allocator::with_target(crate::allocator::Target::Owner {
                account: self.allocation.clone(), class: mac_kernel_model::policy::MemoryClass::Kernel
            }, || super::paint::PaintOwner::prepare(WindowId(self.permit.id()),
                self.owner.owner(), self.allocation.clone(), notice, guardian))?);
        }
        Ok(())
    }

    pub(super) fn birth_allocation(&self) -> AllocationAuthority
    {
        self.allocation.clone()
    }

    pub(super) fn birth_window(&self) -> Window
    {
        let mut window = self.window.window;
        window.id = WindowId(self.permit.id());
        window
    }

    pub(super) fn birth_cleanup(&self)
                                -> Result<alloc::sync::Arc<super::operations::WindowJob>, Status>
    {
        self.window.cleanup_job.as_ref().cloned().ok_or(mac_abi::BAD_ABI)
    }

    pub(super) fn birth_quiescence(&self)
                                   -> crate::toolbox::resources::definitions::QuiescencePlan
    {
        self.window.binding.quiescence_plan()
    }

    pub(super) fn birth_matches(&self,
                                proof: &crate::toolbox::resources::definitions::QuiescedDefinition)
                                -> bool
    {
        proof.matches(&self.window.binding)
    }

    pub(super) fn retain_author_state(&mut self,
                                      reply: &crate::toolbox::resources::definitions::callbacks::packet::Reply)
                                      -> Result<(), Status>
    {
        self.window.state = reply.window()?;
        Ok(())
    }

    pub(crate) fn user_definition(&self) -> Result<Option<DefinitionRef>, CreationError>
    {
        let entry = self.window.binding.reference();
        if crate::toolbox::resources::definitions::is_user(entry).map_err(CreationError::Binding)?
        {
            Ok(Some(entry))
        }
        else
        {
            Ok(None)
        }
    }

    pub(crate) fn user_request(
        &self, operation: i16)
        -> Result<crate::toolbox::resources::definitions::callbacks::packet::Request, CreationError>
    {
        use crate::toolbox::resources::definitions::callbacks::packet::{Arguments, Object, Request};
        self.user_definition()?.ok_or(CreationError::InvalidRequest)?;
        let expected = match self.window.user_initialization
        {
            UserInitialization::None => definition::NEW,
            UserInitialization::New => definition::CALCULATE,
            UserInitialization::Ready => return Err(CreationError::InvalidRequest)
        };
        if operation != expected
        {
            return Err(CreationError::InvalidRequest);
        }
        Ok(Request { properties: None,
                     kind:       definition::WDEF,
                     id:         self.window.procedure >> 4,
                     variation:  self.window.procedure & 15,
                     message:    operation,
                     reflow:     0,
                     object:     Object::Window(self.window.state),
                     arguments:  Arguments::Scalar(0),
                     surface:    None })
    }

    pub(crate) fn apply_user_reply(&mut self, operation: i16,
                                   reply: &crate::toolbox::resources::definitions::callbacks::packet::Reply)
                                   -> Result<(), CreationError>
    {
        self.user_request(operation)?;
        if reply.result < 0
        {
            return Err(CreationError::Definition { procedure: self.window.procedure,
                                                   operation,
                                                   reply: reply.result });
        }
        let state = reply.window().map_err(CreationError::Binding)?;
        if operation == definition::CALCULATE
           && (state.structure.is_empty()
               || state.structure.left < 0
               || state.structure.top < if self.window.stay_in_back { MENU_BAR_BOTTOM } else { 25 }
               || state.structure.right > crate::display::width() as i16
               || state.structure.bottom > crate::display::height() as i16)
        {
            return Err(CreationError::Geometry { procedure: self.window.procedure });
        }
        self.window.state = state;
        self.window.user_initialization = if operation == definition::NEW
        {
            UserInitialization::New
        }
        else
        {
            UserInitialization::Ready
        };
        Ok(())
    }

    pub(crate) fn owner(&self) -> &OwnerRef
    {
        &self.owner
    }

    pub(crate) fn matches_request(&self, request: &mac_abi::WindowRequest) -> bool
    {
        self.intent.matches(request)
    }

    pub(crate) fn plan_capacity(&self, manager: &WindowManager)
                                -> Result<WindowCapacityPlan, CreationError>
    {
        manager.validate_process_intent(&self.owner, &self.intent)?;
        manager.plan_process_capacity()
    }

    /// Return obsolete owners instead of dropping even an empty buffer here.
    pub(crate) fn replace_capacity(&mut self, mut capacity: WindowCapacity) -> WindowCapacity
    {
        core::mem::swap(&mut self.capacity, &mut capacity);
        capacity
    }
}
impl WindowCapacityPlan
{
    pub(crate) fn prepare(self) -> Result<WindowCapacity, CreationError>
    {
        InvocationGate::with_system_allocation(|| {
            fn array<T>(count: usize) -> Result<Vec<T>, CreationError>
            {
                let mut values = Vec::new();
                values.try_reserve_exact(count).map_err(|_| CreationError::NoMemory)?;
                Ok(values)
            }
            let mut failures = VecDeque::new();
            failures.try_reserve_exact(self.failures).map_err(|_| CreationError::NoMemory)?;
            let mut removals = VecDeque::new();
            removals.try_reserve_exact(self.removals).map_err(|_| CreationError::NoMemory)?;
            Ok(WindowCapacity { visibility: self.visibility
                                                .prepare()
                                                .map_err(|_| CreationError::NoMemory)?,
                                order: array(self.order)?,
                                order_scratch: array(self.order_scratch)?,
                                geometry_order: array(self.geometry_order)?,
                                failures,
                                removals })
        })
    }
}
impl WindowCapacity
{
    pub(crate) fn dispose(self)
    {
        drop(self);
    }
}
impl WindowRetired
{
    pub(crate) fn dispose(self)
    {
        self.capacity.dispose();
        self.admission.dispose();
        self.publication.dispose();
        drop(self.owner);
        drop(self.allocation);
    }
}
impl WindowManager
{
    fn validate_process_intent(&self, owner: &OwnerRef, intent: &Intent)
                               -> Result<(), CreationError>
    {
        if owner.owner().kind() != DomainKind::Process
           || crate::context::recovery_activity().is_some()
        {
            return Err(CreationError::RecoveryAuthority);
        }
        self.preflight_creation(intent.0.bounds,
                                owner,
                                intent.minimum(),
                                intent.0.reserved == mac_abi::WINDOW_STAY_IN_BACK)?;
        Ok(())
    }

    pub(crate) fn plan_process_window(&self, owner: &OwnerRef, request: &mac_abi::WindowRequest,
                                      entry: DefinitionRef)
                                      -> Result<WindowPlan, CreationError>
    {
        if request.title_length as usize > request.title.len()
           || request.procedure < 0
           || !matches!(request.reserved, 0 | mac_abi::WINDOW_STAY_IN_BACK)
           || core::str::from_utf8(&request.title[..request.title_length as usize]).is_err()
        {
            return Err(CreationError::InvalidRequest);
        }
        let intent = Intent(*request);
        self.validate_process_intent(owner, &intent)?;
        let capacity = self.plan_process_capacity()?;
        let publication = self.windows.plan_publication(owner).map_err(CreationError::Facility)?;
        Ok(WindowPlan { owner: owner.clone(),
                        intent,
                        entry,
                        gate: self.gate,
                        publication,
                        capacity })
    }

    fn capacity_requirements(&self) -> Result<(usize, usize), CreationError>
    {
        let required = self.len
                           .checked_add(1)
                           .and_then(|n| n.checked_add(self.recovery_spare()))
                           .ok_or(CreationError::NoMemory)?;
        let failures =
            self.retained_failures.len().checked_add(required).ok_or(CreationError::NoMemory)?;
        Ok((required, failures))
    }

    fn plan_process_capacity(&self) -> Result<WindowCapacityPlan, CreationError>
    {
        let (required, failures) = self.capacity_requirements()?;
        let removals = self.removal_capacity_required(1 + self.recovery_spare())?;
        let missing = |current, wanted| if current < wanted { wanted } else { 0 };
        Ok(WindowCapacityPlan { visibility:     self.visibility
                                                    .borrow()
                                                    .plan_capacity(required)
                                                    .map_err(|_| CreationError::NoMemory)?,
                                order:          missing(self.order.capacity(), required),
                                order_scratch:  missing(self.order_scratch.capacity(), required),
                                geometry_order: missing(self.geometry_order.capacity(), required),
                                failures:       missing(self.retained_failures.capacity(),
                                                        failures),
                                removals:       missing(self.removals.capacity(), removals) })
    }

    fn install_process_capacity(&mut self, capacity: &mut WindowCapacity)
                                -> Result<(), CreationError>
    {
        let (required, failures) = self.capacity_requirements()?;
        let removals = self.removal_capacity_required(1 + self.recovery_spare())?;
        if (self.removals.capacity() < removals && capacity.removals.capacity() < removals)
           || (self.order.capacity() < required && capacity.order.capacity() < required)
           || (self.order_scratch.capacity() < required
               && capacity.order_scratch.capacity() < required)
           || (self.geometry_order.capacity() < required
               && capacity.geometry_order.capacity() < required)
           || (self.retained_failures.capacity() < failures
               && capacity.failures.capacity() < failures)
           || !self.visibility
                   .get_mut()
                   .can_install(required, &capacity.visibility)
                   .map_err(|_| CreationError::NoMemory)?
        {
            return Err(CreationError::CapacityChanged);
        }
        // Every capacity was checked before moving any shared metadata.
        self.visibility
            .get_mut()
            .install_capacity(required, &mut capacity.visibility)
            .map_err(|_| CreationError::CapacityChanged)?;
        if self.removals.capacity() < removals
        {
            capacity.removals.extend(self.removals.drain(..));
            core::mem::swap(&mut self.removals, &mut capacity.removals);
        }
        if self.order.capacity() < required
        {
            capacity.order.extend(self.order.drain(..));
            core::mem::swap(&mut self.order, &mut capacity.order);
        }
        if self.order_scratch.capacity() < required
        {
            core::mem::swap(&mut self.order_scratch, &mut capacity.order_scratch);
        }
        if self.geometry_order.capacity() < required
        {
            core::mem::swap(&mut self.geometry_order, &mut capacity.geometry_order);
        }
        if self.retained_failures.capacity() < failures
        {
            while let Some(ticket) = self.retained_failures.pop_front()
            {
                capacity.failures.push_back(ticket);
            }
            core::mem::swap(&mut self.retained_failures, &mut capacity.failures);
        }
        Ok(())
    }

    /// This consumes no payload on refusal, including Store-side capacity change.
    pub(crate) fn admit_process_window(&mut self, input: &mut Option<WindowPreparation>)
                                       -> Result<PreparedWindow, CreationError>
    {
        let preparation = input.as_ref().ok_or(CreationError::InvalidRequest)?;
        self.validate_process_intent(&preparation.owner, &preparation.intent)?;
        let Some(WindowPreparation { window,
                                     capacity,
                                     owner,
                                     allocation,
                                     intent,
                                     publication }) = input.take()
        else
        {
            return Err(CreationError::InvalidRequest);
        };
        match self.windows.admit_prepared(publication)
        {
            Ok((permit, admission)) =>
            {
                Ok(PreparedWindow { window,
                                    capacity,
                                    owner,
                                    allocation,
                                    intent,
                                    admission,
                                    permit })
            }
            Err(failure) =>
            {
                *input = Some(WindowPreparation { window,
                                                  capacity,
                                                  owner,
                                                  allocation,
                                                  intent,
                                                  publication: failure.preparation });
                Err(CreationError::Publication(failure.cause))
            }
        }
    }

    /// Canonical unpublished custody installed before any author NEW.
    pub(crate) fn admit_birth_owner(&mut self, owner: &alloc::sync::Arc<super::birth::BirthOwner>,
                                    request: &mac_abi::WindowRequest)
                                    -> Result<(), CreationError>
    {
        if !owner.same_notice(&self.work_notice) || owner.registered()
        {
            return Err(CreationError::InvalidRequest);
        }
        let mut loan = owner.checkout().map_err(CreationError::Binding)?;
        let prepared = loan.prepared.as_mut().ok_or(CreationError::InvalidRequest)?;
        if !prepared.matches_request(request)
           || prepared.owner.owner() != owner.storage_owner()
           || prepared.birth_window().id != owner.window_id()
        {
            return Err(CreationError::InvalidRequest);
        }
        self.validate_process_intent(&prepared.owner, &prepared.intent)?;
        self.install_process_capacity(&mut prepared.capacity)?;
        if self.removals.len() == self.removals.capacity()
        {
            return Err(CreationError::CapacityChanged);
        }
        self.removals.push_back(super::operations::RemovalDebt::Birth(owner.clone()));
        owner.register();
        Ok(())
    }

    /// Outer refusal is prebirth and retains input. Finished always returns
    /// retired metadata, including when genuine WDEF initialization rolls back.
    pub(crate) fn commit_process_window(&mut self, input: &mut Option<PreparedWindow>,
                                        events: &mut EventManager, when: u32, activate: bool)
                                        -> Result<WindowCommit, CreationError>
    {
        let prepared = input.as_ref().ok_or(CreationError::InvalidRequest)?;
        self.validate_process_intent(&prepared.owner, &prepared.intent)?;
        if prepared.user_definition()?.is_some()
           && prepared.window.user_initialization != UserInitialization::Ready
        {
            return Err(CreationError::Binding(mac_abi::SERVICE_BUSY));
        }
        self.retry_retained_quarantines();
        let prepared = input.as_mut().ok_or(CreationError::InvalidRequest)?;
        self.install_process_capacity(&mut prepared.capacity)?;
        let Some(PreparedWindow { mut window,
                                  capacity,
                                  owner,
                                  allocation,
                                  intent,
                                  admission,
                                  permit }) = input.take()
        else
        {
            return Err(CreationError::InvalidRequest);
        };
        window.window.id = WindowId(permit.id());
        match self.windows.publish_prepared(permit, window)
        {
            Err(failure) =>
            {
                *input = Some(PreparedWindow { window: failure.value,
                                               capacity,
                                               owner,
                                               allocation,
                                               intent,
                                               admission,
                                               permit: failure.permit });
                Err(CreationError::Publication(failure.cause))
            }
            Ok((publication, retired)) =>
            {
                let result = self.initialize_window(WindowId(publication.id()),
                                                    intent.0.procedure,
                                                    events,
                                                    when,
                                                    activate,
                                                    None,
                                                    None,
                                                    None);
                Ok(WindowCommit { result,
                                  retired: WindowRetired { capacity,
                                                           admission,
                                                           publication: retired,
                                                           owner,
                                                           allocation } })
            }
        }
    }
}

//! Canonical SCHD registration and SCTL lifecycle. No foreign callback runs here.
#![forbid(unsafe_code)]
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};

use mac_abi::{ACCESS_DENIED, BAD_ABI, BAD_ARGUMENT, CORRUPT_DATA, NO_MEMORY, NOT_FOUND,
              NOT_SUPPORTED, SERVICE_BUSY, Status};
use mac_module::{Offer, abi};
use mac_object_table::{domain::{CapacityPolicy, KindCode, ReleasedResource},
                       registry::{CleanupFailure as _, Release, Source, Specification},
                       shared::InvocationGate as _,
                       store::CompletionKey};
use mac_scheduler_interface::{self as wire,
                              control::{self, Limits, Registration as Reply, Requirements, State}};
use spinning_top::Spinlock;

use crate::{images::ModuleExecution,
            machine::scheduler::policy::{self, ActivationProof, Plan},
            state::TryState,
            toolbox::handles::{InvocationGate, OwnerRef, Store,
                               cleanup::{Failure, context::Context}}};

static CATALOG: TryState<Catalog> = TryState::new();
static INSTALLED: AtomicBool = AtomicBool::new(false);
static RETIRING: AtomicBool = AtomicBool::new(false);
static WORK: AtomicBool = AtomicBool::new(false);
struct Catalog
{
    entries: Store<Arc<Registration>>
}
struct Registration
{
    owner:      OwnerRef,
    image:      crate::images::Binding,
    offer:      Offer,
    limits:     Limits,
    generation: AtomicU64,
    refusal:    AtomicI32,
    receipt:    spin::Once<CompletionKey>,
    state:      Spinlock<Phase>
}
struct Phase
{
    kind:                     State,
    transition:               u64,
    status:                   Status,
    callbacks:                u32,
    callback_count_uncertain: bool,
    withdrawn:                Option<crate::drivers::registry::SchedulerWithdrawalAck>,
    withdrawal_refusal:       Option<WithdrawalRefusal>,
    retiring:                 bool,
    committing:               bool,
    requested_harts:          u32,
    plan:                     Option<Arc<Plan>>,
    binding:                  Option<Arc<BoundPolicy>>
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WithdrawalRefusal
{
    Registry(super::super::Error),
    Runtime(crate::state::AccessError),
    Acknowledgement
}
impl WithdrawalRefusal
{
    fn status(self) -> Status
    {
        match self
        {
            Self::Registry(cause) => super::status(cause),
            Self::Runtime(cause) => cause.status(),
            Self::Acknowledgement => CORRUPT_DATA
        }
    }

    fn retryable(self) -> bool
    {
        matches!(self,
                 Self::Registry(super::super::Error::Busy)
                 | Self::Runtime(crate::state::AccessError::Busy))
    }
}
fn retain_withdrawal_refusal(row: &Registration, cause: WithdrawalRefusal)
{
    let distinct = {
        let mut state = row.state.lock();
        let distinct = state.withdrawal_refusal != Some(cause);
        state.withdrawal_refusal = Some(cause);
        if !cause.retryable()
        {
            state.kind = State::Quarantined;
        }
        distinct
    };
    if distinct
    {
        serial!("SCHEDULER_WITHDRAWAL_RETAINED generation={} cause={cause:?}; original \
                 registration, code and plan remain owned",
                row.generation.load(Ordering::Acquire));
    }
    if cause.retryable()
    {
        changed();
    }
}
/// Only the canonical adapter pump constructs this genuine withdrawal intent.
pub(crate) struct Withdrawal
{
    owner:        OwnerRef,
    registration: CompletionKey,
    status:       Status
}
impl Withdrawal
{
    pub(crate) fn owner(&self) -> &OwnerRef
    {
        &self.owner
    }

    pub(crate) fn registration(&self) -> CompletionKey
    {
        self.registration
    }

    pub(crate) fn status(&self) -> Status
    {
        self.status
    }

    pub(crate) fn cause(&self) -> &'static str
    {
        match self.status
        {
            0 =>
            {
                "Scheduler policy withdrawn at its owner's request; bootstrap scheduling remains \
                 active"
            }
            CORRUPT_DATA =>
            {
                "Scheduler callback accounting is uncertain; bootstrap active, original \
                 registration and code retained"
            }
            NOT_SUPPORTED =>
            {
                "Scheduler policy transition or complete eligible set is unsupported; bootstrap \
                 active, original cleanup retained"
            }
            BAD_ABI =>
            {
                "Scheduler policy reply or immutable round is invalid; bootstrap active, original \
                 cleanup retained"
            }
            _ =>
            {
                "Scheduler policy callback or transition refused with the recorded status; \
                 bootstrap active, original cleanup retained"
            }
        }
    }
}
struct BoundPolicy
{
    execution: ModuleExecution,
    instance:  spin::Once<abi::Instance>
}
/// Stale pins retain metadata, never permission to enter the provider again.
#[derive(Clone)]
pub(crate) struct PolicyPin
{
    registration: Arc<Registration>,
    plan:         Arc<Plan>,
    _released:    Released
}
#[derive(Clone)]
struct Released;
impl Drop for Released
{
    fn drop(&mut self)
    {
        if RETIRING.load(Ordering::Acquire)
        {
            changed();
            crate::machine::scheduler::wake::request();
        }
    }
}
pub(crate) enum Candidate
{
    Shadow(PolicyPin),
    Active(PolicyPin)
}
pub(crate) struct CallLease
{
    plan:      Arc<Plan>,
    binding:   Arc<BoundPolicy>,
    pin:       PolicyPin,
    instance:  abi::Instance,
    _released: Released
}
impl CallLease
{
    pub(crate) fn plan(&self) -> &Plan
    {
        &self.plan
    }

    pub(crate) fn execution(&self) -> &ModuleExecution
    {
        &self.binding.execution
    }

    pub(crate) fn instance(&self) -> abi::Instance
    {
        self.instance
    }
}
impl Drop for CallLease
{
    fn drop(&mut self)
    {
        InvocationGate::run(|| {
            let mut state = self.pin.registration.state.lock();
            // Only begin_call constructs this linear lease; one matching Drop.
            let Some(remaining) = state.callbacks.checked_sub(1)
            else
            {
                // Never manufacture a quiescence ACK or kill the System from
                // a guard destructor. The original row/code/plan stay retained.
                state.callback_count_uncertain = true;
                state.withdrawn = None;
                INSTALLED.store(false, Ordering::Release);
                state.kind = State::Quarantined;
                state.status = CORRUPT_DATA;
                state.retiring = true;
                RETIRING.store(true, Ordering::Release);
                changed();
                crate::machine::scheduler::wake::request();
                return;
            };
            state.callbacks = remaining;
        });
        // _released runs after both payload Arcs and pin have actually dropped.
    }
}
impl PolicyPin
{
    pub(crate) fn plan(&self) -> &Plan
    {
        &self.plan
    }

    pub(crate) fn generation(&self) -> u64
    {
        self.registration.generation.load(Ordering::Acquire)
    }

    fn owner(&self) -> &OwnerRef
    {
        &self.registration.owner
    }
}
fn changed()
{
    WORK.store(true, Ordering::Release);
    super::recovery::changed();
}
pub(crate) fn report_pending()
{
    changed();
    crate::machine::scheduler::wake::request();
}
/// Canonical lower ACKs already enter drivers::recovery_changed. Subscribe
/// without polling or adding a new authority/registry.
pub(crate) fn cleanup_progress()
{
    if RETIRING.load(Ordering::Acquire)
    {
        WORK.store(true, Ordering::Release);
        crate::machine::scheduler::wake::request();
    }
}
fn advance(state: &mut Phase) -> Result<(), Status>
{
    let Some(next) = state.transition.checked_add(1)
    else
    {
        state.kind = State::Quarantined;
        state.status = NOT_SUPPORTED;
        state.withdrawn = None;
        INSTALLED.store(false, Ordering::Release);
        state.retiring = true;
        RETIRING.store(true, Ordering::Release);
        changed();
        crate::machine::scheduler::wake::request();
        return Err(NOT_SUPPORTED);
    };
    state.transition = next;
    Ok(())
}
fn snapshot(row: &Registration, state: &Phase) -> Reply
{
    Reply { generation:   row.generation.load(Ordering::Acquire),
            state:        state.kind,
            last_status:  state.withdrawal_refusal.map_or(state.status, WithdrawalRefusal::status),
            active_harts: crate::machine::scheduler::active_harts(),
            callbacks:    state.callbacks,
            transition:   state.transition }
}
fn with_catalog<R>(op: impl FnOnce(&mut Catalog) -> R) -> Result<R, Status>
{
    InvocationGate::run(|| CATALOG.with(op).map_err(|e| e.status()))
}
fn pin(row: &Arc<Registration>, plan: Arc<Plan>) -> PolicyPin
{
    PolicyPin { registration: row.clone(),
                plan,
                _released: Released }
}
fn owner_row(catalog: &Catalog, owner: &OwnerRef) -> Option<Arc<Registration>>
{
    catalog.entries.retained_values().find(|row| row.owner.owner() == owner.owner()).cloned()
}
fn authentic(pin: &PolicyPin) -> Result<(), Status>
{
    with_catalog(|catalog| {
        let receipt = pin.registration.receipt.get().ok_or(BAD_ABI)?;
        let row = catalog.entries.completing(receipt).ok_or(NOT_FOUND)?;
        if Arc::ptr_eq(row, &pin.registration) { Ok(()) } else { Err(ACCESS_DENIED) }
    })?
}
/// Work hint only. Canonical phase/admission is checked again by candidate.
pub(crate) fn configured() -> bool
{
    INSTALLED.load(Ordering::Acquire)
}
pub(crate) fn candidate() -> Result<Option<Candidate>, Status>
{
    if !INSTALLED.load(Ordering::Acquire)
    {
        return Ok(None);
    }
    with_catalog(|catalog| {
        let Some(row) = catalog.entries.retained_values().next()
        else
        {
            return Ok(None);
        };
        let mut state = row.state.try_lock().ok_or(SERVICE_BUSY)?;
        if !row.owner.accepting() && !state.retiring
        {
            state.retiring = true;
            state.kind = State::Retiring;
            RETIRING.store(true, Ordering::Release);
            changed();
            crate::machine::scheduler::wake::request();
        }
        if state.retiring || row.refusal.load(Ordering::Acquire) != 0 || !row.owner.accepting()
        {
            INSTALLED.store(false, Ordering::Release);
            return Ok(None);
        }
        if state.binding.as_ref().and_then(|b| b.instance.get()).is_none()
        {
            return Ok(None);
        }
        let plan = state.plan.as_ref().ok_or(BAD_ABI)?.clone();
        Ok(match state.kind
        {
            State::Armed => Some(Candidate::Shadow(pin(row, plan))),
            State::Active => Some(Candidate::Active(pin(row, plan))),
            _ => None
        })
    })?
}
pub(crate) fn begin_call(pin: &PolicyPin) -> Result<CallLease, Status>
{
    InvocationGate::run(|| {
        authentic(pin)?;
        if pin.registration.offer.class != wire::POLICY_CLASS
        {
            return Err(BAD_ABI);
        }
        let mut state = pin.registration.state.try_lock().ok_or(SERVICE_BUSY)?;
        if state.retiring
           || pin.registration.refusal.load(Ordering::Acquire) != 0
           || !pin.registration.owner.accepting()
           || !matches!(state.kind, State::Armed | State::Active)
        {
            return Err(SERVICE_BUSY);
        }
        let plan = state.plan.as_ref().ok_or(BAD_ABI)?.clone();
        let binding = state.binding.as_ref().ok_or(BAD_ABI)?.clone();
        let instance = *binding.instance.get().ok_or(SERVICE_BUSY)?;
        // Actual per-hart scratch loans provide local exclusion. The total
        // remains bounded by the prepaid discovered topology, never a guess
        // that a module's Instance can be called through an unpinned copy.
        let callbacks = state.callbacks.checked_add(1).ok_or(CORRUPT_DATA)?;
        if callbacks > plan.limits().physical_harts
        {
            return Err(SERVICE_BUSY);
        }
        state.callbacks = callbacks;
        Ok(CallLease { plan,
                       binding,
                       pin: pin.clone(),
                       instance,
                       _released: Released })
    })
}
/// Final metadata exclusion for a canonical THRD claim. The caller holds
/// SCHEDULER; this turn must not enter Runtime/catalog, allocate, call foreign
/// code or dispose owning values. A retained pin cannot authorize a withdrawn
/// or replaced generation. No registry lock is acquired in reverse order.
pub(crate) fn with_active_policy<R>(pin: &PolicyPin, generation: u64,
                                    operation: impl FnOnce() -> R)
                                    -> Result<R, Status>
{
    InvocationGate::run(|| {
        let state = pin.registration.state.try_lock().ok_or(SERVICE_BUSY)?;
        if generation == 0
           || pin.generation() != generation
           || pin.registration.receipt.get().is_none()
           || state.kind != State::Active
           || state.retiring
           || pin.registration.refusal.load(Ordering::Acquire) != 0
           || !pin.owner().accepting()
        {
            return Err(SERVICE_BUSY);
        }
        Ok(operation())
    })
}
pub(crate) fn commit_shadow(proof: ActivationProof) -> Result<(), Status>
{
    InvocationGate::run(|| {
        let pin = proof.into_policy();
        authentic(&pin)?;
        {
            let mut state = pin.registration.state.try_lock().ok_or(SERVICE_BUSY)?;
            if state.kind != State::Armed
               || state.retiring
               || state.callbacks != 0
               || !pin.owner().accepting()
            {
                return Err(SERVICE_BUSY);
            }
            advance(&mut state)?;
            // This private publication turn is reachable only by consuming the
            // core's sealed genuine shadow proof. It is never a public caller flag.
            state.committing = true;
        }
        let published = super::RUNTIME.with(|runtime| {
                                          let mut output = [0; mac_module::lifecycle::REPLY_BYTES];
                                          runtime.registry
                                                 .startup_call(pin.owner().owner().incarnation(),
                                                               mac_module::lifecycle::CONFIRM,
                                                               &[],
                                                               &mut output)
                                                 .map_err(super::status)
                                      })
                                      .map_err(|e| e.status())
                                      .and_then(core::convert::identity);
        let mut state = pin.registration.state.lock();
        state.committing = false;
        match published
        {
            Ok(_) if !state.retiring && state.kind == State::Armed =>
            {
                state.kind = State::Active;
                changed();
                crate::machine::scheduler::wake::request();
                Ok(())
            }
            Ok(_) => Err(SERVICE_BUSY),
            Err(cause) =>
            {
                state.status = cause;
                Err(cause)
            }
        }
    })
}
pub(crate) fn refuse(pin: &PolicyPin, cause: Status) -> Result<(), Status>
{
    InvocationGate::run(|| {
        if cause >= 0
        {
            return Err(BAD_ARGUMENT);
        }
        // PolicyPin is minted only from the original stored registration. A
        // first refusal revokes new callbacks and claims before a potentially
        // contended metadata turn, retaining its exact cause for the pump.
        if pin.generation() == 0 || pin.registration.receipt.get().is_none()
        {
            return Err(BAD_ABI);
        }
        let _ = pin.registration
                   .refusal
                   .compare_exchange(0, cause, Ordering::AcqRel, Ordering::Acquire);
        INSTALLED.store(false, Ordering::Release);
        RETIRING.store(true, Ordering::Release);
        changed();
        crate::machine::scheduler::wake::request();
        let mut state = pin.registration.state.try_lock().ok_or(SERVICE_BUSY)?;
        advance(&mut state)?;
        state.status = pin.registration.refusal.load(Ordering::Acquire);
        state.withdrawn = None;
        INSTALLED.store(false, Ordering::Release);
        state.kind = State::Quarantined;
        state.retiring = true;
        RETIRING.store(true, Ordering::Release);
        changed();
        crate::machine::scheduler::wake::request();
        Ok(())
    })
}
pub(crate) fn publication_receipt(owner: &OwnerRef) -> Result<Option<CompletionKey>, Status>
{
    InvocationGate::run(|| {
        with_catalog(|catalog| {
            let Some(row) = owner_row(catalog, owner)
            else
            {
                return Ok(None);
            };
            let state = row.state.try_lock().ok_or(SERVICE_BUSY)?;
            if (state.kind == State::Active || state.committing)
               && !state.retiring
               && owner.accepting()
               && state.binding.as_ref().and_then(|b| b.instance.get()).is_some()
            {
                Ok(row.receipt.get().copied())
            }
            else
            {
                Ok(None)
            }
        })?
    })
}
pub(crate) fn bind_returned(module: u64, image: crate::images::Binding, instance: abi::Instance)
                            -> Result<(), Status>
{
    InvocationGate::run(|| {
        let row = with_catalog(|catalog| {
            catalog.entries
                   .retained_values()
                   .find(|r| r.owner.owner().incarnation() == module)
                   .cloned()
        })?;
        let Some(row) = row
        else
        {
            return Ok(());
        };
        if row.image.id() != image.id()
           || row.image.owner() != image.owner()
           || !row.owner.accepting()
        {
            return Err(ACCESS_DENIED);
        }
        let state = row.state.try_lock().ok_or(SERVICE_BUSY)?;
        if state.retiring || !matches!(state.kind, State::Prepared | State::Armed)
        {
            return Err(SERVICE_BUSY);
        }
        let binding = state.binding.as_ref().ok_or(BAD_ABI)?;
        if binding.instance.get().is_some()
        {
            return Err(BAD_ABI);
        }
        binding.instance.call_once(|| instance);
        if state.kind == State::Armed
        {
            INSTALLED.store(true, Ordering::Release);
            crate::machine::scheduler::wake::request();
        }
        Ok(())
    })
}
fn prepare(module: u64, requirements: Requirements) -> Result<Reply, Status>
{
    let (owner, allocation, image, offer) =
        super::RUNTIME.with(|runtime| {
                          let (owner, allocation, offer) = runtime.registry
                                                                  .scheduler_preparation(module)
                                                                  .map_err(super::status)?;
                          let binding = runtime.bindings
                                               .iter()
                                               .find(|b| b.module == module)
                                               .ok_or(NOT_FOUND)?;
                          if binding.instance.is_some()
                          {
                              return Err(SERVICE_BUSY);
                          }
                          Ok::<_, Status>((owner, allocation, binding.image, offer))
                      })
                      .map_err(|e| e.status())??;
    requirements.validate(policy::limits()?).map_err(|_| BAD_ARGUMENT)?;
    let execution = crate::kernel::module_execution(image)?;
    crate::allocation::with_kernel_allocation(allocation, || {
        let limits = policy::limits()?;
        let plan = Arc::try_new(Plan::prepare(&owner, requirements)?).map_err(|_| NO_MEMORY)?;
        let binding =
            Arc::try_new(BoundPolicy { execution,
                                       instance: spin::Once::new() }).map_err(|_| NO_MEMORY)?;
        let state = Phase { kind:                     State::Prepared,
                            transition:               1,
                            status:                   0,
                            callbacks:                0,
                            callback_count_uncertain: false,
                            withdrawn:                None,
                            withdrawal_refusal:       None,
                            retiring:                 false,
                            committing:               false,
                            requested_harts:          1,
                            plan:                     Some(plan),
                            binding:                  Some(binding) };
        let row =
            Arc::try_new(Registration { owner: owner.clone(),
                                        image,
                                        offer,
                                        limits,
                                        generation: AtomicU64::new(0),
                                        refusal: AtomicI32::new(0),
                                        receipt: spin::Once::new(),
                                        state: Spinlock::new(state) }).map_err(|_| NO_MEMORY)?;
        with_catalog(|catalog| {
            if catalog.entries.retained_values().next().is_some() || !owner.accepting()
            {
                return Err(SERVICE_BUSY);
            }
            let (_, receipt) = catalog.entries
                                      .insert_with_receipt(&owner, |id| {
                                          row.generation.store(id, Ordering::Release);
                                          row.clone()
                                      })
                                      .map_err(|_| NO_MEMORY)?
                                      .into_parts();
            row.receipt.call_once(|| receipt);
            // Prepared is not callable and creates no periodic scheduler work.
            let state = row.state.lock();
            Ok(snapshot(&row, &state))
        })?
    })?
}
fn stop(row: &Registration) -> Result<(), Status>
{
    InvocationGate::run(|| {
        let mut state = row.state.try_lock().ok_or(SERVICE_BUSY)?;
        if !state.retiring
        {
            advance(&mut state)?;
            state.retiring = true;
            state.kind = State::Retiring;
            crate::machine::scheduler::wake::request();
        }
        INSTALLED.store(false, Ordering::Release);
        RETIRING.store(true, Ordering::Release);
        changed();
        Ok(())
    })
}
fn control_operation(owner: &OwnerRef, operation: u32, input: &[u8]) -> Result<Reply, Status>
{
    InvocationGate::run(|| {
        let generation = if operation == control::HARTS
        {
            control::HartRequest::decode(input).map_err(|_| BAD_ARGUMENT)?.generation
        }
        else
        {
            u64::from_le_bytes(input.try_into().map_err(|_| BAD_ARGUMENT)?)
        };
        let row = with_catalog(|catalog| owner_row(catalog, owner))?;
        let Some(row) = row
        else
        {
            return if operation == control::RETIRE && generation == 0
            {
                Ok(Reply { generation:   0,
                           state:        State::Retired,
                           last_status:  0,
                           active_harts: 1,
                           callbacks:    0,
                           transition:   0 })
            }
            else
            {
                Err(NOT_FOUND)
            };
        };
        if generation != row.generation.load(Ordering::Acquire)
           && !(operation == control::RETIRE && generation == 0)
        {
            return Err(ACCESS_DENIED);
        }
        if operation == control::RETIRE
        {
            stop(&row)?;
        }
        let mut state = row.state.try_lock().ok_or(SERVICE_BUSY)?;
        match operation
        {
            control::STATUS | control::RETIRE => (),
            control::ARM
                if owner.accepting() && !state.retiring && state.kind == State::Prepared =>
            {
                advance(&mut state)?;
                state.kind = State::Armed;
                let callable = state.binding.as_ref().and_then(|b| b.instance.get()).is_some();
                INSTALLED.store(callable, Ordering::Release);
                if callable
                {
                    crate::machine::scheduler::wake::request();
                }
            }
            control::HARTS
                if owner.accepting()
                   && !state.retiring
                   && matches!(state.kind, State::Prepared | State::Armed | State::Active) =>
            {
                let request = control::HartRequest::decode(input).map_err(|_| BAD_ARGUMENT)?;
                if request.count == 0 || request.count > row.limits.maximum_active_harts
                {
                    return Err(NOT_SUPPORTED);
                }
                advance(&mut state)?;
                state.requested_harts = request.count;
                changed();
                crate::machine::scheduler::wake::request();
            }
            _ => return Err(SERVICE_BUSY)
        }
        let reply = snapshot(&row, &state);
        drop(state);
        if operation == control::RETIRE && reply.state == State::Retired
        {
            if Arc::strong_count(&row) != 2
            {
                return Err(SERVICE_BUSY);
            }
            let removed = with_catalog(|catalog| {
                let removed = catalog.entries.remove(reply.generation);
                if catalog.entries.retained_values().next().is_none()
                {
                    INSTALLED.store(false, Ordering::Release);
                    RETIRING.store(false, Ordering::Release);
                }
                removed
            })?;
            // A stopped owner has invalidated the ID already: its actual walk
            // owns release, so retain the row/ticket until that real path ACKs it.
            let missing = removed.is_none() && row.owner.accepting();
            // Return our temporary metadata borrow before Removed destroys
            // the sole stored payload and performs its canonical ACK.
            drop(row);
            if missing
            {
                return Err(NOT_FOUND);
            }
            drop(removed);
        }
        Ok(reply)
    })
}
fn request(module: u64, operation: u32, input: &[u8], output: &mut [u8]) -> Result<usize, Status>
{
    if !(control::MIN_REPLY..=control::MAX_REPLY).contains(&output.len())
    {
        return Err(BAD_ARGUMENT);
    }
    crate::context::authenticate_module(module)?;
    let result = (|| {
        if operation == control::LIMITS
        {
            if !input.is_empty()
            {
                return Err(BAD_ARGUMENT);
            }
            let payload = policy::limits()?.encode().map_err(|_| BAD_ABI)?;
            return Ok(control::reply(control::Reply::Success(&payload), output).map_err(|_| {
                                                                                   BAD_ABI
                                                                               })?
                                                                               .len());
        }
        let reply = if operation == control::PREPARE
        {
            let requirements =
                Requirements::decode(input, policy::limits()?).map_err(|_| BAD_ARGUMENT)?;
            prepare(module, requirements)?
        }
        else
        {
            let owner = crate::context::module_owner(module)?;
            control_operation(&owner, operation, input)?
        };
        let payload = reply.encode().map_err(|_| BAD_ABI)?;
        Ok(control::reply(control::Reply::Success(&payload), output).map_err(|_| BAD_ABI)?.len())
    })();
    match result
    {
        Ok(length) => Ok(length),
        Err(status) =>
        {
            let cause = match status
            {
                ACCESS_DENIED =>
                {
                    "SCTL caller, declaration or registration does not match its genuine module \
                     owner"
                }
                NOT_FOUND =>
                {
                    "No matching retained scheduler registration exists for this owner and \
                     generation"
                }
                BAD_ARGUMENT =>
                {
                    "Invalid SCTL payload, generation, reply capacity or requested hart count"
                }
                NO_MEMORY =>
                {
                    "Scheduler preparation could not admit complete original-account metadata and \
                     snapshot storage; no takeover"
                }
                NOT_SUPPORTED =>
                {
                    "Requested scheduler mechanism, topology or transition is unsupported; no \
                     partial activation"
                }
                BAD_ABI =>
                {
                    "Scheduler binding or wire is invalid; original cleanup obligation remains \
                     retained"
                }
                _ =>
                {
                    "Scheduler transition is busy or not in its required lifecycle phase; retain \
                     the original grant and retry after actual progress"
                }
            };
            Ok(control::reply(control::Reply::Failure { status,
                                                        cause },
                              output).map_err(|_| BAD_ABI)?
                                     .len())
        }
    }
}
/// Guard-free checkpoint. Callback/lease releases and actual cleanup signals wake it.
pub(crate) fn pump()
{
    if !WORK.swap(false, Ordering::AcqRel)
    {
        return;
    }
    let row = match with_catalog(|catalog| catalog.entries.retained_values().next().cloned())
    {
        Ok(Some(row)) => row,
        Ok(None) => return,
        Err(_) =>
        {
            changed();
            return;
        }
    };
    let refused = row.refusal.load(Ordering::Acquire);
    if refused != 0
    {
        let Some(mut state) = row.state.try_lock()
        else
        {
            changed();
            return;
        };
        if !state.retiring
        {
            if advance(&mut state).is_err()
            {
                return;
            }
            state.status = refused;
            state.kind = State::Quarantined;
            state.retiring = true;
            state.withdrawn = None;
        }
    }
    let (report_plan, activation) = {
        let state = row.state.lock();
        let plan = state.plan.as_ref().cloned();
        let activation = if state.kind == State::Active && !state.retiring && row.owner.accepting()
        {
            plan.as_ref().map(|plan| (pin(&row, plan.clone()), state.requested_harts))
        }
        else
        {
            None
        };
        (plan, activation)
    };
    if let Some(plan) = report_plan
    {
        plan.report();
    }
    if let Some((pin, count)) = activation
    {
        // The actual core publication takes SCHEDULER then the same Active
        // phase exclusion used by its THRD claim. No Runtime guard crosses it.
        if let Err(status) = crate::machine::scheduler::set_policy_harts(&pin, count)
        {
            if status == SERVICE_BUSY
            {
                changed();
            }
            else
            {
                let _ = refuse(&pin, status);
            }
        }
    }
    let intent = {
        let state = row.state.lock();
        if !state.retiring
        {
            return;
        }
        if state.withdrawn.is_none()
        {
            let Some(registration) = row.receipt.get().copied()
            else
            {
                return;
            };
            Some(Withdrawal { owner: row.owner.clone(),
                              registration,
                              status: state.status })
        }
        else
        {
            None
        }
    };
    if let Some(intent) = intent
    {
        let removed =
            match super::RUNTIME.with(|runtime| runtime.registry.withdraw_scheduler(&intent))
            {
                Ok(Ok(removed)) => removed,
                Ok(Err(cause)) =>
                {
                    retain_withdrawal_refusal(&row, WithdrawalRefusal::Registry(cause));
                    return;
                }
                Err(cause) =>
                {
                    retain_withdrawal_refusal(&row, WithdrawalRefusal::Runtime(cause));
                    return;
                }
            };
        // Actual interface payload and canonical completion leave outside Runtime.
        drop(removed);
        let completion =
            match super::RUNTIME.with(|runtime| runtime.registry.scheduler_withdrawal_ack(&intent))
            {
                Ok(Ok(completion)) => completion,
                Ok(Err(cause)) =>
                {
                    retain_withdrawal_refusal(&row, WithdrawalRefusal::Registry(cause));
                    return;
                }
                Err(cause) =>
                {
                    retain_withdrawal_refusal(&row, WithdrawalRefusal::Runtime(cause));
                    return;
                }
            };
        let Some(completion) = completion
        else
        {
            return;
        };
        let acknowledgement = completion.into_ack();
        if !acknowledgement.matches(intent.registration)
        {
            retain_withdrawal_refusal(&row, WithdrawalRefusal::Acknowledgement);
            return;
        }
        let mut state = row.state.lock();
        state.withdrawn = Some(acknowledgement);
        state.withdrawal_refusal = None;
    }
    let plan = {
        let state = row.state.lock();
        if !state.retiring || state.callbacks != 0 || state.callback_count_uncertain
        {
            return;
        }
        let Some(plan) = state.plan.as_ref()
        else
        {
            return;
        };
        plan.clone()
    };
    // No catalog/phase/runtime/scheduler guard crosses actual backend withdrawal.
    match plan.retire()
    {
        Ok(true) => (),
        Ok(false) => return,
        Err(status) =>
        {
            let mut state = row.state.lock();
            let distinct = state.status != status || state.kind != State::Quarantined;
            state.status = status;
            state.kind = State::Quarantined;
            if distinct
            {
                state.withdrawn = None;
                changed();
            }
            return;
        }
    }
    let payload = {
        let mut state = row.state.lock();
        if state.callbacks != 0
           || state.callback_count_uncertain
           || state.plan.as_ref().is_some_and(|p| Arc::strong_count(p) != 2)
           || state.binding.as_ref().is_some_and(|b| Arc::strong_count(b) != 1)
        {
            return;
        }
        if advance(&mut state).is_err()
        {
            state.status = NOT_SUPPORTED;
            state.kind = State::Quarantined;
            return;
        }
        state.kind = State::Retired;
        INSTALLED.store(false, Ordering::Release);
        (state.plan.take(), state.binding.take())
    };
    drop(payload);
    drop(plan);
    super::recovery::changed();
}
pub(crate) fn install() -> Result<(), Status>
{
    let kinds = crate::toolbox::handles::registry(|registry| {
                    Ok((registry.get(KindCode(*b"SCHR"))?, registry.get(KindCode(*b"SCHS"))?))
                }).map_err(|e| e.status())?
                  .map_err(|e| crate::toolbox::handles::registry_status(&e))?;
    CATALOG.publish(Catalog { entries: Store::owned(kinds.0, kinds.1).map_err(|_| NO_MEMORY)? })
           .map_err(|e| e.status())?;
    super::register_synchronous_service(Offer { class:          wire::CONTROL_CLASS,
                                                version:        wire::VERSION,
                                                request_schema: 1,
                                                reply_schema:   1,
                                                capabilities:   0,
                                                max_request:    control::PREPARE_BYTES as u32,
                                                max_reply:      control::MAX_REPLY as u32,
                                                flags:          Offer::EXCLUSIVE
                                                                | Offer::SERIALIZED
                                                                | Offer::KERNEL_ONLY },
                                        request,
                                        None)?;
    Ok(())
}
pub(crate) struct ReleaseRegistration(pub bool);
pub(crate) fn specification(storage: bool) -> Specification<'static>
{
    Specification { kind:     KindCode(if storage { *b"SCHS" } else { *b"SCHR" }),
                    label:    if storage
                    {
                        "Scheduler registration storage"
                    }
                    else
                    {
                        "Scheduler policy registration"
                    },
                    provider: "System Scheduler",
                    capacity: CapacityPolicy::HardBound { slots:  1,
                                                          reason: "One scheduler policy \
                                                                   registration and backing per \
                                                                   module" },
                    source:   Source::Resident }
}
impl<C: Context + ?Sized> Release<C, Failure> for ReleaseRegistration
{
    fn release(&self, _: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        if ticket.key().kind() != KindCode(if self.0 { *b"SCHS" } else { *b"SCHR" })
        {
            return Err(Failure::registration_mismatch(ticket));
        }
        // Preserve the linear ticket even if another hart owns the catalog.
        let result = InvocationGate::run(|| {
            CATALOG.with_owned(ticket, |ticket, catalog| {
                       let row = catalog.entries
                                        .retained_values()
                                        .find(|row| row.owner.owner() == ticket.key().owner());
                       if let Some(row) = row
                       {
                           let state = row.state.lock();
                           if state.kind != State::Retired || Arc::strong_count(row) != 1
                           {
                               drop(state);
                               let _ = stop(row);
                               changed();
                               return Err(Failure::scheduler_pending(ticket,
                                                                     row.generation
                                                                        .load(Ordering::Acquire)));
                           }
                       }
                       if self.0
                       {
                           catalog.entries
                                  .release_storage(ticket)
                                  .map_err(Failure::from)
                                  .map(|()| None)
                       }
                       else
                       {
                           let removed = catalog.entries.release(ticket).map_err(Failure::from)?;
                           if catalog.entries.retained_values().next().is_none()
                           {
                               INSTALLED.store(false, Ordering::Release);
                               RETIRING.store(false, Ordering::Release);
                           }
                           Ok(Some(removed))
                       }
                   })
        });
        match result
        {
            Ok(Ok(removed)) =>
            {
                // Payload/refund/destructors and final canonical ACK outside locks.
                drop(removed);
                Ok(())
            }
            Ok(Err(error)) => Err(error),
            Err((cause, ticket)) => Err(Failure::scheduler_unavailable(ticket, cause))
        }
    }
}

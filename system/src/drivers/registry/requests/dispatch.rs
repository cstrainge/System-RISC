//! DREQ-owned delivery storage. A caller's Poll is correlation, not permission
//! to skip Start or repeat a terminal provider effect. No borrowed ABI buffer
//! survives the synchronous invocation which supplied it.
#![forbid(unsafe_code)]
use alloc::{sync::Arc, vec::Vec};

use mac_abi::{BAD_ABI, BAD_ARGUMENT, NO_MEMORY, OPERATION_CANCELLED, ROLLBACK_FAILED,
              SERVICE_BUSY, Status};
use mac_module::abi;
use mac_object_table::shared::InvocationGate as _;
use spinning_top::Spinlock;

use crate::toolbox::handles::InvocationGate;

fn alias_released()
{
    #[cfg(target_arch = "riscv64")]
    crate::drivers::request_retirement_alias_released();
}
/// Publication-time ownership has the same post-decrement retirement wake as
/// a dispatch lease. No strong-count observation substitutes for that wake.
pub(crate) struct Preparation
{
    storage:      Option<Arc<Storage>>,
    cancellation: Option<super::cancellation::Reservation>
}
impl Preparation
{
    pub(crate) fn storage(&self) -> Result<&Arc<Storage>, Status>
    {
        self.storage.as_ref().ok_or(BAD_ARGUMENT)
    }

    pub(super) fn cancellation_ready(&self) -> bool
    {
        self.cancellation.is_some()
    }

    pub(super) fn take_cancellation(&mut self) -> Option<super::cancellation::Reservation>
    {
        self.cancellation.take()
    }
}
impl Drop for Preparation
{
    fn drop(&mut self)
    {
        drop(self.cancellation.take());
        drop(self.storage.take());
        alias_released();
    }
}
pub(crate) fn prepare(subject: &super::Subject, origin: super::Origin, input: &[u8],
                      reply_bytes: usize)
                      -> Result<Preparation, Status>
{
    let operation = || {
        let storage = Storage::prepare(subject.scope().subject(), input, reply_bytes)?;
        let cancellation = if origin == super::Origin::Direct && subject.current_request().is_none()
        {
            Some(super::cancellation::Reservation::new(reply_bytes).map_err(crate::drivers::status)?)
        }
        else
        {
            None
        };
        Ok(Preparation { storage: Some(storage),
                         cancellation })
    };
    #[cfg(target_arch = "riscv64")]
    if let Some(activity) = subject.recovery_activity()
    {
        if subject.uses_protected_publication()
           && !(origin == super::Origin::Direct && subject.current_request().is_none())
        {
            return activity.with_storage(operation);
        }
        return crate::context::without_recovery(operation);
    }
    let _ = (subject, origin);
    operation()
}

#[derive(Clone, Copy)]
enum Phase
{
    Unentered,
    Pending,
    Terminal
    {
        status: Status,
        length: usize
    },
    Uncertain(Status)
}
struct Buffers
{
    input:  Vec<u8>,
    output: Vec<u8>
}
struct State
{
    phase:           Phase,
    buffers:         Option<Buffers>,
    active:          bool,
    progress_waiter: bool
}
pub(crate) struct Storage
{
    state:       Spinlock<State>,
    input_bytes: usize,
    reply_bytes: usize,
    owner:       mac_object_table::domain::DomainId
}
impl Storage
{
    /// All storage is fallibly admitted under the actual original Subject and
    /// allocation selection before publishing its canonical DREQ. The current
    /// queue's Broker API borrows Work buffers, so queued requests also need
    /// this owned copy; their existing Work retains its reconciliation data.
    fn prepare(owner: mac_object_table::domain::DomainId, input: &[u8], reply_bytes: usize)
               -> Result<Arc<Self>, Status>
    {
        let mut owned_input = Vec::new();
        owned_input.try_reserve_exact(input.len()).map_err(|_| NO_MEMORY)?;
        owned_input.extend_from_slice(input);
        let mut output = Vec::new();
        output.try_reserve_exact(reply_bytes).map_err(|_| NO_MEMORY)?;
        output.resize(reply_bytes, 0);
        Arc::try_new(Self { state: Spinlock::new(State { phase:           Phase::Unentered,
                                                         buffers:
                                                             Some(Buffers { input: owned_input,
                                                                            output }),
                                                         active:          false,
                                                         progress_waiter: false }),
                            input_bytes: input.len(),
                            reply_bytes,
                            owner }).map_err(|_| NO_MEMORY)
    }

    pub(crate) fn fits(&self, input_bytes: usize, reply_bytes: usize) -> bool
    {
        self.input_bytes == input_bytes && self.reply_bytes == reply_bytes
    }

    pub(crate) fn active(&self) -> bool
    {
        InvocationGate::run(|| self.state.lock().active)
    }

    /// Observe actual callback activity and register a one-shot progress wake
    /// only when the unique lease still owns the buffers. The same gate as
    /// handback prevents losing the transition to an inactive/available request.
    pub(crate) fn active_or_subscribe(&self) -> bool
    {
        InvocationGate::run(|| {
            let mut state = self.state.lock();
            if state.buffers.is_none()
            {
                state.progress_waiter = true;
            }
            state.active
        })
    }

    pub(crate) fn terminal(&self) -> bool
    {
        InvocationGate::run(|| matches!(self.state.lock().phase, Phase::Terminal { .. }))
    }

    pub(crate) fn uncertainty(&self) -> Option<Status>
    {
        InvocationGate::run(|| {
            match self.state.lock().phase
            {
                Phase::Uncertain(status) => Some(status),
                _ => None
            }
        })
    }

    pub(crate) fn acquire(self: &Arc<Self>) -> Result<Lease, Status>
    {
        let (buffers, phase) = InvocationGate::run(|| {
            let mut state = self.state.lock();
            if matches!(state.phase, Phase::Uncertain(_))
            {
                return Err(ROLLBACK_FAILED);
            }
            let buffers = state.buffers.take().ok_or(SERVICE_BUSY)?;
            let phase = state.phase;
            state.active = !matches!(phase, Phase::Terminal { .. });
            Ok((buffers, phase))
        })?;
        Ok(Lease { storage: Some(self.clone()),
                   buffers: Some(buffers),
                   phase,
                   entered: false })
    }
}

/// The only owned buffers for one DREQ invocation. Drop before foreign entry
/// restores the previous phase. Drop after entry without a returned result
/// preserves uncertainty and the canonical request for cleanup quarantine.
/// This does not claim containment of arbitrary privileged faults.
pub(crate) struct Lease
{
    storage: Option<Arc<Storage>>,
    buffers: Option<Buffers>,
    phase:   Phase,
    entered: bool
}
impl Lease
{
    pub(crate) fn delivery(&mut self, requested: u32) -> Option<u32>
    {
        match self.phase
        {
            Phase::Unentered if requested == abi::CANCEL =>
            {
                // The provider has never seen Start, so there is no effect to
                // cancel and no foreign callback is needed to acknowledge it.
                self.returned(OPERATION_CANCELLED, 0);
                None
            }
            Phase::Unentered => Some(abi::START),
            Phase::Pending => Some(requested),
            Phase::Terminal { .. } | Phase::Uncertain(_) => None
        }
    }

    pub(crate) fn buffers(&mut self) -> Result<(&[u8], &mut [u8], &mut bool), Status>
    {
        let buffers = self.buffers.as_mut().ok_or(BAD_ABI)?;
        Ok((&buffers.input, &mut buffers.output, &mut self.entered))
    }

    /// Record the actual returned result before any registry reacquisition.
    /// Malformed results are uncertainty, never fabricated completion ACKs.
    pub(crate) fn returned(&mut self, status: Status, length: usize)
    {
        let valid = self.buffers.as_ref().is_some_and(|b| length <= b.output.len())
                    && (status <= 0 || status == abi::PENDING)
                    && (status != abi::PENDING || length == 0);
        self.phase = if !valid
        {
            Phase::Uncertain(BAD_ABI)
        }
        else if status == abi::PENDING
        {
            Phase::Pending
        }
        else
        {
            Phase::Terminal { status,
                              length }
        };
        self.entered = false;
        if let Some(storage) = &self.storage
        {
            InvocationGate::run(|| {
                let mut state = storage.state.lock();
                state.phase = self.phase;
                state.active = false;
            });
        }
    }

    pub(crate) fn uncertain(&mut self, status: Status)
    {
        self.phase = Phase::Uncertain(status);
        self.entered = false;
        if let Some(storage) = &self.storage
        {
            InvocationGate::run(|| {
                let mut state = storage.state.lock();
                state.phase = self.phase;
                state.active = false;
            });
        }
    }

    pub(crate) fn reply(&self) -> Result<Option<(Status, &[u8])>, Status>
    {
        match self.phase
        {
            Phase::Terminal { status,
                              length } =>
            {
                let bytes =
                    self.buffers.as_ref().and_then(|b| b.output.get(..length)).ok_or(BAD_ABI)?;
                Ok(Some((status, bytes)))
            }
            Phase::Uncertain(_) => Err(ROLLBACK_FAILED),
            Phase::Pending => Ok(None),
            Phase::Unentered => Err(BAD_ARGUMENT)
        }
    }
}
impl Drop for Lease
{
    fn drop(&mut self)
    {
        let phase = if self.entered { Phase::Uncertain(ROLLBACK_FAILED) } else { self.phase };
        let buffers = self.buffers.take();
        // No buffer or capability destructor runs under this mailbox lock.
        if let Some(storage) = self.storage.take()
        {
            let owner = storage.owner;
            let (displaced, progress_waiter) = InvocationGate::run(|| {
                let mut state = storage.state.lock();
                state.phase = phase;
                state.active = false;
                let displaced = core::mem::replace(&mut state.buffers, buffers);
                let progress_waiter = core::mem::take(&mut state.progress_waiter);
                (displaced, progress_waiter)
            });
            debug_assert!(displaced.is_none(), "unique DREQ dispatch lease invariant");
            drop(displaced);
            // Arc must really decrement before waking a retirement which was
            // waiting for its final external alias. No per-event retry scan.
            drop(storage);
            alias_released();
            if progress_waiter
            {
                #[cfg(target_arch = "riscv64")]
                crate::drivers::request_progress_changed(owner);
                #[cfg(not(target_arch = "riscv64"))]
                let _ = owner;
            }
        }
    }
}

//! Resident cleanup refusals retain both typed cause and the unique retry ticket.
#![forbid(unsafe_code)]
use mac_object_table::{domain::{CompletionError, PoolError, ReleasedResource},
                       registry::{CleanupFailure, Release}};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cause
{
    #[cfg(target_arch = "riscv64")]
    ThreadPreparations
    {
        preparing: usize
    },
    #[cfg(target_arch = "riscv64")]
    NativeJoin
    {
        readers:      usize,
        preparations: usize
    },
    #[cfg(target_arch = "riscv64")]
    ApplicationHeap(crate::toolbox::application_heap::CleanupCause),
    #[cfg(target_arch = "riscv64")]
    NativeTls(crate::toolbox::native_threads::CleanupCause),
    #[cfg(target_arch = "riscv64")]
    RequestRegistryUnavailable(crate::state::AccessError),
    #[cfg(target_arch = "riscv64")]
    ScrapManagerUnavailable(crate::state::AccessError),
    ContextUnavailable
    {
        required: &'static str
    },
    #[cfg(target_arch = "riscv64")]
    SchedulerPending
    {
        generation: u64
    },
    #[cfg(target_arch = "riscv64")]
    SchedulerUnavailable(crate::state::AccessError),
    DriverInterfaceBusy
    {
        module:  u64,
        pending: usize
    },
    DriverDependencyBusy
    {
        provider:  u64,
        dependent: u64
    },
    DeviceStop
    {
        ordinal: u32,
        status:  mac_abi::Status
    },
    DevicePending
    {
        ordinal:  u32,
        stopped:  bool,
        mappings: usize,
        reset:    Option<u64>
    },
    #[cfg(target_arch = "riscv64")]
    DeviceManagerUnavailable(crate::state::AccessError),
    DriverDependencyScopeActive
    {
        provider:  u64,
        dependent: u64
    },
    DriverReplyPending
    {
        request: u64,
        module:  u64,
        bytes:   usize,
        status:  mac_abi::Status
    },
    DriverCancellationUncertain
    {
        request: u64,
        module:  u64,
        status:  mac_abi::Status
    },
    Facility(PoolError),
    Alias(mac_object_table::aliases::ReleaseCause),
    RetirementStorage
    {
        provider: &'static str,
        owner:    mac_object_table::domain::DomainId,
        kind:     mac_object_table::domain::KindCode,
        pending:  usize,
        capacity: usize
    },
    DragEffectStorage
    {
        pending:   usize,
        requested: usize
    },
    #[cfg(target_arch = "riscv64")]
    MemoryManagerUnavailable(crate::state::AccessError),
    #[cfg(target_arch = "riscv64")]
    FileManagerUnavailable(crate::state::AccessError),
    WindowDefinitionDisposal
    {
        window:    u64,
        procedure: i16,
        reply:     i64
    }
}
impl core::fmt::Display for Cause
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            #[cfg(target_arch = "riscv64")]
            Self::ThreadPreparations { preparing } =>
            {
                write!(f,
                       "native thread backing retains {preparing} actual prepared publications; \
                        original THST ticket retained")
            }
            #[cfg(target_arch = "riscv64")]
            Self::NativeJoin { readers,
                               preparations } =>
            {
                write!(f,
                       "native join right retains {readers} actual readers and {preparations} \
                        prepared publications; original ticket retained")
            }
            #[cfg(target_arch = "riscv64")]
            Self::ApplicationHeap(cause) => cause.fmt(f),
            #[cfg(target_arch = "riscv64")]
            Self::NativeTls(cause) => cause.fmt(f),
            #[cfg(target_arch = "riscv64")]
            Self::RequestRegistryUnavailable(cause) =>
            {
                write!(f,
                       "driver request cleanup could not enter its registry ({cause:?}); exact \
                        ticket and pending request remain retained for retry")
            }
            #[cfg(target_arch = "riscv64")]
            Self::ScrapManagerUnavailable(cause) =>
            {
                write!(f,
                       "Scrap extension cleanup could not enter its resident manager ({cause:?}); \
                        exact ticket, session and snapshot backing remain retained for retry")
            }
            Self::ContextUnavailable { required } =>
            {
                write!(f,
                       "registered cleanup requires the {required} context; payload and unique \
                        ticket remain retained for retry")
            }
            #[cfg(target_arch = "riscv64")]
            Self::SchedulerPending { generation } =>
            {
                write!(f,
                       "scheduler registration {generation} awaits actual callback and \
                        target-owned quiescence; original payload and cleanup ticket retained")
            }
            #[cfg(target_arch = "riscv64")]
            Self::SchedulerUnavailable(cause) =>
            {
                write!(f,
                       "scheduler registration cleanup catalog unavailable ({cause:?}); original \
                        ticket retained")
            }
            Self::DriverInterfaceBusy { module,
                                        pending } =>
            {
                write!(f,
                       "driver module {module} still has {pending} requests using this interface; \
                        wait for actual completion or acknowledged cancellation before retrying \
                        cleanup")
            }
            Self::DeviceStop { ordinal,
                               status } =>
            {
                write!(f,
                       "device {ordinal} reset/stop probe failed with status {status}; no stop \
                        acknowledgement exists; DMA, claim and unique cleanup ticket remain \
                        retained for retry")
            }
            Self::DevicePending { ordinal,
                                  stopped,
                                  mappings,
                                  reset } =>
            {
                write!(f,
                       "device {ordinal} retirement awaits hardware stop or dependent cleanup \
                        (stopped={stopped}, mappings={mappings}, reset={reset:?}); claim, DMA and \
                        unique cleanup ticket remain retained for retry")
            }
            #[cfg(target_arch = "riscv64")]
            Self::DeviceManagerUnavailable(cause) =>
            {
                write!(f,
                       "device cleanup could not enter its manager ({cause:?}); claim, DMA and \
                        unique cleanup ticket remain retained for retry")
            }
            Self::DriverDependencyScopeActive { provider,
                                                dependent } =>
            {
                write!(f,
                       "module {dependent} still has an admitted backing operation using provider \
                        {provider}; wait for actual close or cancellation acknowledgement; \
                        dependency and original cleanup ticket remain retained")
            }
            Self::Facility(error) => error.fmt(f),
            Self::DriverReplyPending { request,
                                       module,
                                       bytes,
                                       status } =>
            {
                write!(f,
                       "module {module} abandoned request {request} with {bytes} acknowledged \
                        reply bytes (status {status}); committed effects require class recovery; \
                        reply, provider pin and unique cleanup ticket remain retained")
            }
            Self::DriverCancellationUncertain { request,
                                                module,
                                                status } =>
            {
                write!(f,
                       "module {module} request {request} has an uncertain cancellation result \
                        (status {status}); no terminal acknowledgement is proven; reply storage, \
                        provider pin and unique cleanup ticket remain retained")
            }
            Self::DriverDependencyBusy { provider,
                                         dependent } =>
            {
                write!(f,
                       "driver module {provider} still supplies a declared dependency to module \
                        {dependent}; unload the dependent module first; interface payload and \
                        cleanup ticket remain retained")
            }
            Self::Alias(error) => error.fmt(f),
            Self::RetirementStorage { provider,
                                      owner,
                                      kind,
                                      pending,
                                      capacity } =>
            {
                write!(f,
                       "{provider} {kind} cleanup for {owner} cannot retain another asynchronous \
                        object: {pending} pending, reserved capacity {capacity}; payload, \
                        provider token and cleanup ticket remain owned for retry")
            }
            Self::DragEffectStorage { pending,
                                      requested } =>
            {
                write!(f,
                       "Drag Manager could not reserve {requested} notification slots with \
                        {pending} pending; protocol state and cleanup ticket remain retained for \
                        retry")
            }
            #[cfg(target_arch = "riscv64")]
            Self::MemoryManagerUnavailable(error) =>
            {
                write!(f,
                       "Memory Manager cleanup could not enter its resident state ({error:?}); \
                        payload, backing and cleanup ticket remain retained for retry")
            }
            #[cfg(target_arch = "riscv64")]
            Self::FileManagerUnavailable(error) =>
            {
                write!(f,
                       "File Manager cleanup could not enter its resident state ({error:?}); file \
                        payload, backing and cleanup ticket remain retained for retry")
            }
            Self::WindowDefinitionDisposal { window,
                                             procedure,
                                             reply } =>
            {
                write!(f,
                       "WDEF {} variation {} refused disposal of window {window} with reply \
                        {reply}; definition state and cleanup ticket remain retained for retry",
                       procedure >> 4,
                       procedure & 15)
            }
        }
    }
}
impl core::error::Error for Cause
{
}
#[derive(Debug)]
pub(crate) struct Failure
{
    ticket: ReleasedResource,
    cause:  Cause
}
impl Failure
{
    #[cfg(target_arch = "riscv64")]
    pub(crate) fn thread_preparations(ticket: ReleasedResource, preparing: usize) -> Self
    {
        Self { ticket,
               cause: Cause::ThreadPreparations { preparing } }
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn native_join(ticket: ReleasedResource, readers: usize, preparations: usize)
                              -> Self
    {
        Self { ticket,
               cause: Cause::NativeJoin { readers,
                                          preparations } }
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn native_tls(ticket: ReleasedResource,
                             cause: crate::toolbox::native_threads::CleanupCause)
                             -> Self
    {
        Self { ticket,
               cause: Cause::NativeTls(cause) }
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn application_heap(ticket: ReleasedResource,
                                   cause: crate::toolbox::application_heap::CleanupCause)
                                   -> Self
    {
        Self { ticket,
               cause: Cause::ApplicationHeap(cause) }
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn scheduler_pending(ticket: ReleasedResource, generation: u64) -> Self
    {
        Self { ticket,
               cause: Cause::SchedulerPending { generation } }
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn scheduler_unavailable(ticket: ReleasedResource, cause: crate::state::AccessError)
                                        -> Self
    {
        Self { ticket,
               cause: Cause::SchedulerUnavailable(cause) }
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn request_registry_unavailable(ticket: ReleasedResource,
                                               cause: crate::state::AccessError)
                                               -> Self
    {
        Self { ticket,
               cause: Cause::RequestRegistryUnavailable(cause) }
    }

    pub(crate) fn device_stop(ticket: ReleasedResource, ordinal: u32, status: mac_abi::Status)
                              -> Self
    {
        Self { ticket,
               cause: Cause::DeviceStop { ordinal,
                                          status } }
    }

    pub(crate) fn device_pending(ticket: ReleasedResource, ordinal: u32, stopped: bool,
                                 mappings: usize, reset: Option<u64>)
                                 -> Self
    {
        Self { ticket,
               cause: Cause::DevicePending { ordinal,
                                             stopped,
                                             mappings,
                                             reset } }
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn devices_unavailable(ticket: ReleasedResource, cause: crate::state::AccessError)
                                      -> Self
    {
        Self { ticket,
               cause: Cause::DeviceManagerUnavailable(cause) }
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn scrap_unavailable(ticket: ReleasedResource, cause: crate::state::AccessError)
                                    -> Self
    {
        Self { ticket,
               cause: Cause::ScrapManagerUnavailable(cause) }
    }

    pub(crate) fn context_unavailable(ticket: ReleasedResource, required: &'static str) -> Self
    {
        Self { ticket,
               cause: Cause::ContextUnavailable { required } }
    }

    pub(crate) fn driver_interface_busy(ticket: ReleasedResource, module: u64, pending: usize)
                                        -> Self
    {
        Self { ticket,
               cause: Cause::DriverInterfaceBusy { module,
                                                   pending } }
    }

    pub(crate) fn driver_dependency_busy(ticket: ReleasedResource, provider: u64, dependent: u64)
                                         -> Self
    {
        Self { ticket,
               cause: Cause::DriverDependencyBusy { provider,
                                                    dependent } }
    }

    pub(crate) fn driver_dependency_scope_active(ticket: ReleasedResource, provider: u64,
                                                 dependent: u64)
                                                 -> Self
    {
        Self { ticket,
               cause: Cause::DriverDependencyScopeActive { provider,
                                                           dependent } }
    }

    pub(crate) fn driver_reply_pending(ticket: ReleasedResource, request: u64, module: u64,
                                       bytes: usize, status: mac_abi::Status)
                                       -> Self
    {
        Self { ticket,
               cause: Cause::DriverReplyPending { request,
                                                  module,
                                                  bytes,
                                                  status } }
    }

    pub(crate) fn driver_cancellation_uncertain(ticket: ReleasedResource, request: u64,
                                                module: u64, status: mac_abi::Status)
                                                -> Self
    {
        Self { ticket,
               cause: Cause::DriverCancellationUncertain { request,
                                                           module,
                                                           status } }
    }

    pub(crate) fn document_retirement_storage(ticket: ReleasedResource, pending: usize,
                                              capacity: usize)
                                              -> Self
    {
        let key = ticket.key();
        Self { ticket,
               cause: Cause::RetirementStorage { provider: "Process Manager",
                                                 owner: key.owner(),
                                                 kind: key.kind(),
                                                 pending,
                                                 capacity } }
    }

    pub(crate) fn job_retirement_storage(ticket: ReleasedResource, pending: usize, capacity: usize)
                                         -> Self
    {
        let key = ticket.key();
        Self { ticket,
               cause: Cause::RetirementStorage { provider: "System I/O Manager",
                                                 owner: key.owner(),
                                                 kind: key.kind(),
                                                 pending,
                                                 capacity } }
    }

    pub(crate) fn file_retirement_storage(ticket: ReleasedResource, pending: usize,
                                          capacity: usize)
                                          -> Self
    {
        let key = ticket.key();
        Self { ticket,
               cause: Cause::RetirementStorage { provider: "File Manager",
                                                 owner: key.owner(),
                                                 kind: key.kind(),
                                                 pending,
                                                 capacity } }
    }

    pub(crate) fn drag_effect_storage(ticket: ReleasedResource, pending: usize) -> Self
    {
        Self { ticket,
               cause: Cause::DragEffectStorage { pending,
                                                 requested: 4 } }
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn files_unavailable(ticket: ReleasedResource, error: crate::state::AccessError)
                                    -> Self
    {
        Self { ticket,
               cause: Cause::FileManagerUnavailable(error) }
    }

    #[cfg(target_arch = "riscv64")]
    pub(crate) fn memory_unavailable(ticket: ReleasedResource, error: crate::state::AccessError)
                                     -> Self
    {
        Self { ticket,
               cause: Cause::MemoryManagerUnavailable(error) }
    }

    pub(crate) fn window_definition(ticket: ReleasedResource, window: u64, procedure: i16,
                                    reply: i64)
                                    -> Self
    {
        Self { ticket,
               cause: Cause::WindowDefinitionDisposal { window,
                                                        procedure,
                                                        reply } }
    }
}

pub(crate) mod context;
impl From<CompletionError> for Failure
{
    fn from(error: CompletionError) -> Self
    {
        Self { cause:  Cause::Facility(error.cause()),
               ticket: error.into_ticket() }
    }
}
impl From<mac_object_table::aliases::ReleaseError> for Failure
{
    fn from(error: mac_object_table::aliases::ReleaseError) -> Self
    {
        let (ticket, cause) = error.into_parts();
        Self { ticket,
               cause: Cause::Alias(cause) }
    }
}
impl CleanupFailure for Failure
{
    type Cause = Cause;

    fn into_parts(self) -> (ReleasedResource, Cause)
    {
        (self.ticket, self.cause)
    }

    fn registration_mismatch(ticket: ReleasedResource) -> Self
    {
        Self::from(<CompletionError as CleanupFailure>::registration_mismatch(ticket))
    }
}
impl core::fmt::Display for Failure
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        self.cause.fmt(f)
    }
}
impl core::error::Error for Failure
{
}

/// Adapt facility-only resident providers without discarding their typed cause.
pub(crate) struct Complete<R>(pub R);
impl<C: ?Sized, R: Release<C>> Release<C, Failure> for Complete<R>
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        self.0.release(context, ticket).map_err(Failure::from)
    }
}

mod checkpoint;
pub(crate) use checkpoint::Checkpoint;
pub use checkpoint::CheckpointError;

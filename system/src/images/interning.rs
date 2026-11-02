//! Ordinary immutable-source admission, with factories and disposal outside IMAGES.
use super::*;
type AliasPrepared = aliases::PreparedIntern<Arc<Source>, InvocationGate, Arc<Instance>>;
type AliasPermit = aliases::InternPermit<Arc<Source>, InvocationGate>;
pub(crate) struct LoadPlan
{
    owner:         OwnerRef,
    allocation:    crate::machine::scheduler::accounting::AllocationAuthority,
    base:          u64,
    extent:        u64,
    linked_origin: u64,
    limit:         u64,
    module:        bool,
    aliases:       aliases::InternPlan<Arc<Source>, InvocationGate, Arc<Instance>>
}
struct Pending
{
    owner:      OwnerRef,
    allocation: crate::machine::scheduler::accounting::AllocationAuthority,
    extent:     u64,
    limit:      u64,
    instance:   Arc<Instance>,
    loading:    mac_kernel_model::image::Loading,
    usage:      Use
}
pub(crate) struct LoadPreparation
{
    pending: Pending,
    aliases: AliasPrepared
}
pub(crate) struct LoadPermit
{
    pending: Pending,
    aliases: AliasPermit
}
pub(crate) struct LoadFailure<P>
{
    pub(crate) cause:       Status,
    pub(crate) preparation: P
}
pub(crate) struct LoadRetiredCapacity(aliases::RetiredInternCapacity<Arc<Source>,
                                                                      InvocationGate,
                                                                      Arc<Instance>>);
impl LoadRetiredCapacity
{
    pub(crate) fn dispose(self)
    {
        self.0.dispose();
    }
}
pub(crate) struct LoadRetiredPublication
{
    pending:     Option<Pending>,
    recipient:   Option<(OwnerRef, crate::machine::scheduler::accounting::AllocationAuthority)>,
    publication: aliases::RetiredInternPublication<Arc<Source>, InvocationGate>
}
impl LoadRetiredPublication
{
    pub(crate) fn dispose(self)
    {
        drop(self.pending);
        drop(self.recipient);
        self.publication.dispose();
    }
}
pub(crate) struct RejectedLoad
{
    cause:    aliases::InternError,
    rollback: Option<aliases::InternRollback>,
    pending:  Pending,
    aliases:  aliases::InternRejected<Arc<Source>, InvocationGate, Arc<Instance>>
}
pub(crate) enum PlanningCause
{
    Admission(Status),
    Intern(aliases::InternError)
}
impl PlanningCause
{
    pub(crate) fn status(&self) -> Status
    {
        match self
        {
            Self::Admission(status) => *status,
            Self::Intern(cause) => intern_status(*cause)
        }
    }
}
impl core::fmt::Display for PlanningCause
{
    fn fmt(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Admission(status) =>
            {
                write!(out, "image recipient/placement admission refused with status {status}")
            }
            Self::Intern(cause) => core::fmt::Display::fmt(cause, out)
        }
    }
}
pub(crate) struct PlanningFailure
{
    pub(crate) cause:       PlanningCause,
    pub(crate) preparation: Arc<Source>
}
fn intern_status(cause: aliases::InternError) -> Status
{
    use mac_object_table::store::PreparedError;
    match cause
    {
        aliases::InternError::Alias(cause) => status(cause),
        aliases::InternError::Publication(PreparedError::Pool(cause)) =>
        {
            status(aliases::Error::Pool(cause))
        }
        aliases::InternError::Changed
        | aliases::InternError::CapacityChanged
        | aliases::InternError::Publication(PreparedError::CapacityChanged) => SERVICE_BUSY,
        aliases::InternError::CounterExhausted
        | aliases::InternError::Publication(PreparedError::WrongStore
                                          | PreparedError::RetiredStore) => BAD_ABI
    }
}
impl LoadPlan
{
    pub(crate) fn prepare(self, executable: Vec<core::ops::Range<u64>>,
                          writable: Vec<core::ops::Range<u64>>)
                          -> Result<LoadPreparation, Status>
    {
        let aliases = self.aliases.prepare().map_err(|failure| {
                                                 serial!("IMAGE_INTERN_PREPARATION_REFUSED {}",
                                                         failure.cause);
                                                 intern_status(failure.cause)
                                             })?;
        let instance = prepare_instance(&self.owner, &self.allocation, self.module, self.base,
                                        self.extent, self.linked_origin, executable, writable)?;
        let loading = instance.execution.begin_load().map_err(|_| SERVICE_BUSY)?;
        let usage = instance.activity.retain()?;
        Ok(LoadPreparation { pending: Pending { owner: self.owner,
                                                allocation: self.allocation,
                                                extent: self.extent,
                                                limit: self.limit,
                                                instance,
                                                loading,
                                                usage },
                             aliases })
    }
}
impl Manager
{
    pub(crate) fn plan_load(&self, owner: &OwnerRef,
                            allocation:&crate::machine::scheduler::accounting::AllocationAuthority,
                            source: Arc<Source>, linked_origin: u64, preferred: u64, extent: u64,
                            relocation_base: u64, limit: u64)
                            -> Result<LoadPlan, PlanningFailure>
    {
        let validated = (|| {
            let role_matches =
                matches!((&source.role, owner.owner().kind()),
                         (Role::Application { .. }, mac_object_table::domain::DomainKind::Process)
                         | (Role::Module, mac_object_table::domain::DomainKind::Module));
            if !role_matches || allocation.owner() != owner.owner()
            {
                return Err(ACCESS_DENIED);
            }
            if !owner.accepting()
            {
                return Err(SERVICE_BUSY);
            }
            self.placement(preferred, extent, relocation_base, limit)
        })();
        let base = match validated
        {
            Ok(base) => base,
            Err(cause) =>
            {
                return Err(PlanningFailure { cause:       PlanningCause::Admission(cause),
                                             preparation: source });
            }
        };
        let module = source.role == Role::Module;
        match self.sources.plan_intern(owner, source)
        {
            Ok(aliases) =>
            {
                Ok(LoadPlan { owner: owner.clone(),
                              allocation: allocation.clone(),
                              base,
                              extent,
                              linked_origin,
                              limit,
                              module,
                              aliases })
            }
            Err(failure) =>
            {
                Err(PlanningFailure { cause:       PlanningCause::Intern(failure.cause),
                                      preparation: failure.preparation })
            }
        }
    }

    pub(crate) fn admit_load(
        &mut self, preparation: LoadPreparation)
        -> Result<(LoadPermit, LoadRetiredCapacity), LoadFailure<LoadPreparation>>
    {
        match self.sources.admit_intern(preparation.aliases)
        {
            Ok((aliases, retired)) =>
            {
                Ok((LoadPermit { pending: preparation.pending,
                                 aliases },
                    LoadRetiredCapacity(retired)))
            }
            Err(failure) =>
            {
                Err(LoadFailure { cause:       intern_status(failure.cause),
                                  preparation: LoadPreparation { pending: preparation.pending,
                                                                 aliases: failure.preparation } })
            }
        }
    }

    pub(crate) fn publish_load(
        &mut self, preparation: LoadPermit)
        -> Result<(PreparedLoad, LoadRetiredPublication), LoadFailure<LoadPublishRemainder>>
    {
        let pending = &preparation.pending;
        if !pending.owner.accepting()
           || pending.allocation.owner() != pending.owner.owner()
           || pending.instance.binding.get().is_some()
           || !self.free_placement(pending.instance.base, pending.extent, pending.limit)
        {
            return Err(LoadFailure { cause:       SERVICE_BUSY,
                                     preparation: LoadPublishRemainder::Unpublished(preparation) });
        }
        let LoadPermit { pending,
                         aliases } = preparation;
        match self.sources.publish_intern(aliases, pending.instance.clone())
        {
            Ok((publication, retired)) =>
            {
                let (_, receipt) = publication.into_parts();
                let binding = Binding(receipt);
                pending.instance.binding.call_once(|| binding);
                // Canonical target may be a different allocation from the equal
                // input source. Only the actual born alias supplies this Read.
                let Some((source, _)) = self.sources.completing(&receipt)
                else
                {
                    return Err(LoadFailure{cause:BAD_ABI,preparation:LoadPublishRemainder::Published(LoadRetiredPublication{pending:Some(pending),recipient:None,publication:retired})});
                };
                let Pending { owner,
                              allocation,
                              extent: _,
                              limit: _,
                              instance,
                              loading,
                              usage } = pending;
                let read = Read { source: source.clone(),
                                  instance,
                                  binding,
                                  usage };
                Ok((PreparedLoad { loading,
                                   read },
                    LoadRetiredPublication { pending:     None,
                                             recipient:   Some((owner, allocation)),
                                             publication: retired }))
            }
            Err(failure) =>
            {
                let invalid_rollback =
                    failure.rollback.as_ref().is_some_and(|rollback| !rollback.pending());
                let cause = if invalid_rollback { BAD_ABI } else { intern_status(failure.cause) };
                Err(LoadFailure{cause,preparation:LoadPublishRemainder::Rejected(RejectedLoad{cause:failure.cause,rollback:failure.rollback,pending,aliases:failure.preparation})})
            }
        }
    }
}
pub(crate) enum LoadPublishRemainder
{
    Unpublished(LoadPermit),
    Rejected(RejectedLoad),
    Published(LoadRetiredPublication)
}

impl LoadPublishRemainder
{
    pub(crate) fn dispose(self)
    {
        match self
        {
            Self::Unpublished(preparation) => drop(preparation),
            Self::Rejected(RejectedLoad { cause,
                                          rollback,
                                          pending,
                                          aliases }) =>
            {
                serial!("IMAGE_INTERN_PUBLICATION_REFUSED {cause}");
                if let Some(rollback) = rollback
                {
                    serial!("IMAGE_INTERN_ROLLBACK_RETAINED {rollback}");
                }
                drop(pending);
                aliases.dispose();
            }
            Self::Published(retired) => retired.dispose()
        }
    }
}

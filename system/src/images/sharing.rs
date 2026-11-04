//! Prepared sharing of the recovery source's actual canonical immutable target.
//! Original source Read survives new alias birth; new execution state is private.
use super::*;
type AliasPlan = aliases::SharePlan<InvocationGate, Arc<Instance>>;
type AliasPrepared = aliases::PreparedShare<InvocationGate, Arc<Instance>>;
type AliasPermit = aliases::SharePermit<InvocationGate>;
pub(crate) struct RetiredCapacity(aliases::RetiredShareCapacity<InvocationGate, Arc<Instance>>);
impl RetiredCapacity
{
    pub(crate) fn dispose(self)
    {
        self.0.dispose();
    }
}
pub(crate) struct RetiredPublication
{
    recipient:   Recipient,
    publication: aliases::RetiredSharePublication<InvocationGate>
}
impl RetiredPublication
{
    pub(crate) fn dispose(self)
    {
        drop(self.recipient);
        self.publication.dispose();
    }
}

struct Recipient
{
    owner:         OwnerRef,
    allocation:    crate::machine::scheduler::accounting::AllocationAuthority,
    base:          u64,
    extent:        u64,
    linked_origin: u64,
    limit:         u64
}
pub(crate) struct RecoverySharePlan
{
    recipient: Recipient,
    sharing:   AliasPlan
}
pub(crate) struct PreparedRecoveryShare
{
    recipient: Recipient,
    sharing:   AliasPrepared
}
pub(crate) struct RecoverySharePermit
{
    recipient: Recipient,
    // Last: actual preparation completion/progress follows recipient pins.
    sharing:   AliasPermit
}
struct PendingRead
{
    source:   Arc<Source>,
    instance: Arc<Instance>,
    usage:    Use
}
pub(crate) struct PreparedRecoveryCopy
{
    loading:   mac_kernel_model::image::Loading,
    tag:       Arc<Instance>,
    read:      PendingRead,
    recipient: Recipient,
    // Last: original unpublished slot/credit survives every payload owner.
    sharing:   AliasPermit
}
pub(crate) struct AdmissionFailure
{
    pub(crate) cause:       Status,
    pub(crate) preparation: PreparedRecoveryShare
}
pub(crate) struct PublishFailure
{
    pub(crate) cause:       Status,
    pub(crate) preparation: PreparedRecoveryCopy
}
fn share_status(cause: aliases::ShareError) -> Status
{
    use mac_object_table::store::PreparedError;
    match cause
    {
        aliases::ShareError::Alias(cause) => status(cause),
        aliases::ShareError::SourceChanged { .. } => NOT_FOUND,
        aliases::ShareError::CapacityChanged
        | aliases::ShareError::Publication(PreparedError::CapacityChanged) => SERVICE_BUSY,
        aliases::ShareError::Publication(PreparedError::Pool(cause)) =>
        {
            status(aliases::Error::Pool(cause))
        }
        aliases::ShareError::Publication(PreparedError::WrongStore
                                         | PreparedError::RetiredStore) => BAD_ABI
    }
}
impl RecoverySharePlan
{
    /// Detached buffers allocate under the caller's genuine replacement target
    /// and Permit. Partial failures unwind here, outside every manager guard.
    pub(crate) fn prepare(self) -> Result<PreparedRecoveryShare, Status>
    {
        let sharing = self.sharing.prepare().map_err(share_status)?;
        Ok(PreparedRecoveryShare { recipient: self.recipient,
                                   sharing })
    }
}
impl RecoverySharePermit
{
    /// All payload factories and fallible loading/read prepayment happen here.
    /// Refusal returns the original affine permit for outside disposal/retry.
    pub(crate) fn prepare(self, original: &Read,
                          layout: crate::toolbox::resources::loader::ApplicationLayout)
                          -> Result<PreparedRecoveryCopy, (Status, Self)>
    {
        if layout.preferred != self.recipient.linked_origin
           || layout.extent != self.recipient.extent
        {
            return Err((BAD_ABI, self));
        }
        let instance = match prepare_instance(&self.recipient.owner, &self.recipient.allocation,
                                              false, self.recipient.base, self.recipient.extent,
                                              self.recipient.linked_origin, layout.executable,
                                              layout.writable)
        {
            Ok(instance) => instance,
            Err(cause) => return Err((cause, self))
        };
        let loading = match instance.execution.begin_load()
        {
            Ok(loading) => loading,
            Err(_) => return Err((SERVICE_BUSY, self))
        };
        let usage = match instance.activity.retain()
        {
            Ok(usage) => usage,
            Err(cause) => return Err((cause, self))
        };
        let read = PendingRead { source: original.source.clone(),
                                 instance: instance.clone(),
                                 usage };
        Ok(PreparedRecoveryCopy { loading,
                                  tag: instance,
                                  read,
                                  recipient: self.recipient,
                                  sharing: self.sharing })
    }
}
impl Manager
{
    fn recovery_source(&self, original: &Read) -> Result<(), Status>
    {
        let (source, instance) = self.sources.completing(&original.binding.0).ok_or(NOT_FOUND)?;
        if !Arc::ptr_eq(source, &original.source)
           || !Arc::ptr_eq(instance, &original.instance)
           || instance.owner.owner() != original.binding.0.owner()
           || instance.binding.get() != Some(&original.binding)
           || instance.module
           || !matches!(source.role, Role::Application { .. })
        {
            return Err(ACCESS_DENIED);
        }
        // Source may be stopped: its genuine read prevents actual alias/target
        // retirement. This grants immutable sharing, never execution of it.
        Ok(())
    }

    pub(crate) fn plan_recovery_share(&self, original: &Read, owner: &OwnerRef,
                                      allocation: &crate::machine::scheduler::accounting::AllocationAuthority,
                                      layout: &crate::toolbox::resources::loader::ApplicationLayout)
                                      -> Result<RecoverySharePlan, Status>
    {
        self.recovery_source(original)?;
        if owner.owner().kind() != mac_object_table::domain::DomainKind::Process
           || allocation.owner() != owner.owner()
        {
            return Err(ACCESS_DENIED);
        }
        if !owner.accepting()
        {
            return Err(SERVICE_BUSY);
        }
        let limit = crate::machine::memory::VM_APPLICATION_END;
        let base = self.placement(owner.owner(),
                                  layout.preferred,
                                  layout.extent,
                                  crate::machine::memory::VM_APPLICATION_IMAGE_BASE,
                                  limit)?;
        let sharing = self.sources
                          .plan_share(original.owner(), original.binding.0, owner)
                          .map_err(share_status)?;
        Ok(RecoverySharePlan { recipient: Recipient { owner: owner.clone(),
                                                      allocation: allocation.clone(),
                                                      base,
                                                      extent: layout.extent,
                                                      linked_origin: layout.preferred,
                                                      limit },
                               sharing })
    }

    pub(crate) fn admit_recovery_share(
        &mut self, preparation: PreparedRecoveryShare)
        -> Result<(RecoverySharePermit, RetiredCapacity), AdmissionFailure>
    {
        match self.sources.admit_share(preparation.sharing)
        {
            Ok((sharing, retired)) =>
            {
                Ok((RecoverySharePermit { recipient: preparation.recipient,
                                          sharing },
                    RetiredCapacity(retired)))
            }
            Err(failure) =>
            {
                Err(AdmissionFailure { cause:       share_status(failure.cause),
                                       preparation:
                                           PreparedRecoveryShare { recipient: preparation.recipient,
                                                                   sharing:   failure.preparation } })
            }
        }
    }

    pub(crate) fn publish_recovery_copy(
        &mut self, original: &Read, preparation: PreparedRecoveryCopy)
        -> Result<(PreparedLoad, RetiredPublication), PublishFailure>
    {
        let checked =
            self.recovery_source(original).and_then(|()| {
                                              if !preparation.recipient.owner.accepting()
                                              {
                                                  return Err(SERVICE_BUSY);
                                              }
                                              if preparation.recipient.owner.owner()
                                                 != preparation.read.instance.owner.owner()
                                                 || preparation.read.instance.allocation.owner()
                                                    != preparation.recipient.owner.owner()
                                                 || !Arc::ptr_eq(&preparation.tag,
                                                                 &preparation.read.instance)
                                                 || !Arc::ptr_eq(&preparation.read.source,
                                                                 &original.source)
                                                 || preparation.read
                                                               .instance
                                                               .binding
                                                               .get()
                                                               .is_some()
                                              {
                                                  return Err(ACCESS_DENIED);
                                              }
                                              if !self.free_placement(preparation.recipient
                                                                                 .owner
                                                                                 .owner(),
                                                                      preparation.recipient.base,
                                                                      preparation.recipient.extent,
                                                                      preparation.recipient.limit)
                                              {
                                                  return Err(SERVICE_BUSY);
                                              }
                                              Ok(())
                                          });
        if let Err(cause) = checked
        {
            return Err(PublishFailure { cause,
                                        preparation });
        }
        let PreparedRecoveryCopy { loading,
                                   tag,
                                   read,
                                   recipient,
                                   sharing } = preparation;
        match self.sources.publish_share(sharing, tag)
        {
            Ok((publication, retired)) =>
            {
                let (_, receipt) = publication.into_parts();
                let binding = Binding(receipt);
                read.instance.binding.call_once(|| binding);
                let read = Read { source: read.source,
                                  instance: read.instance,
                                  binding,
                                  usage: read.usage };
                // Recipient owns only authentic metadata pins; return it through
                // an outside owner rather than dropping under IMAGES.
                Ok((PreparedLoad { loading,
                                   read },
                    RetiredPublication { recipient,
                                         publication: retired }))
            }
            Err(failure) =>
            {
                Err(PublishFailure { cause:       share_status(failure.cause),
                                     preparation: PreparedRecoveryCopy { loading,
                                                                         tag: failure.value,
                                                                         read,
                                                                         recipient,
                                                                         sharing: failure.permit } })
            }
        }
    }
}

//! Detached publication of another alias to an actual canonical source target.
//! No immutable value is re-interned, nor is a new System object published.
use super::*;

#[derive(Debug)]
pub enum ShareError
{
    Alias(Error),
    Publication(store::PreparedError),
    CapacityChanged,
    SourceChanged
    {
        alias: u64
    }
}
impl From<Error> for ShareError
{
    fn from(cause: Error) -> Self
    {
        Self::Alias(cause)
    }
}
impl From<PoolError> for ShareError
{
    fn from(cause: PoolError) -> Self
    {
        Self::Alias(Error::Pool(cause))
    }
}
impl fmt::Display for ShareError
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Alias(cause) => cause.fmt(out),
            Self::Publication(cause) => cause.fmt(out),
            Self::CapacityChanged =>
            {
                out.write_str("shared alias retirement capacity changed; replan outside the \
                               manager")
            }
            Self::SourceChanged { alias } =>
            {
                write!(out, "canonical source alias {alias} changed or finished retirement")
            }
        }
    }
}
impl core::error::Error for ShareError
{
}

struct SourceProof<G: InvocationGate>
{
    owner:  OwnerRef<G>,
    alias:  store::CompletionKey,
    target: Target
}
/// Observation plus genuine source/recipient registration and owner pins.
/// Its scalar target coordinates are private and revalidated at every commit.
pub struct SharePlan<G: InvocationGate, S>
{
    aliases:           store::PublicationPlan<Alias<S>, G>,
    source:            SourceProof<G>,
    deferred_capacity: usize,
    recipient:         DomainId,
    kind:              crate::domain::KindCode
}
pub struct PreparedShare<G: InvocationGate, S>
{
    aliases:  store::PreparedPublication<Alias<S>, G>,
    source:   SourceProof<G>,
    deferred: Vec<store::Removed<Alias<S>, G>>
}
pub struct ShareAdmissionFailure<G: InvocationGate, S>
{
    pub cause:       ShareError,
    pub preparation: PreparedShare<G, S>
}
/// Owns the original unpublished canonical slot/credit. No target reference
/// count changes until publish_share actually accepts the alias.
pub struct SharePermit<G: InvocationGate>
{
    aliases: store::PublicationPermit<G>,
    source:  SourceProof<G>
}
#[must_use]
pub struct RetiredShareCapacity<G: InvocationGate, S>
{
    aliases:  store::RetiredCapacity<Alias<S>, G>,
    deferred: Vec<store::Removed<Alias<S>, G>>
}
impl<G: InvocationGate, S> RetiredShareCapacity<G, S>
{
    pub fn dispose(self)
    {
        self.aliases.dispose();
        drop(self.deferred);
    }
}
#[must_use]
pub struct RetiredSharePublication<G: InvocationGate>
{
    publication: store::RetiredPublication<G>,
    source:      SourceProof<G>
}
impl<G: InvocationGate> RetiredSharePublication<G>
{
    pub fn dispose(self)
    {
        // Source proof owns only ledger authority, never caller payload.
        drop(self.source);
        self.publication.dispose();
    }
}
pub struct SharePublishFailure<G: InvocationGate, S>
{
    pub cause:  ShareError,
    pub permit: SharePermit<G>,
    pub value:  S
}
impl<G: InvocationGate, S> SharePlan<G, S>
{
    /// Caller runs this under the recipient's original Kernel target, outside
    /// every manager/ledger guard. The System hook preserves storage origin.
    /// Partial allocations unwind here, before any permit or payload escapes.
    pub fn prepare(self) -> Result<PreparedShare<G, S>, ShareError>
    {
        let mut deferred = Vec::new();
        G::with_system_allocation(|| deferred.try_reserve_exact(self.deferred_capacity))
            .map_err(|_| Error::Pool(PoolError::Storage {
                owner: self.recipient, kind: self.kind,
                used: 0, requested_slots: self.deferred_capacity, limit: None
            }))?;
        let aliases = self.aliases.prepare()?;
        Ok(PreparedShare { aliases,
                           source: self.source,
                           deferred })
    }
}
impl<T, G: InvocationGate, S> Store<T, G, S>
{
    fn completed_alias(&self, receipt: &store::CompletionKey) -> Option<&Alias<S>>
    {
        self.aliases.completing(receipt).or_else(|| {
                                            self.deferred
                                                .iter()
                                                .find_map(|removed| removed.completing(receipt))
                                        })
    }

    fn share_source(&self, source: &OwnerRef<G>, receipt: store::CompletionKey)
                    -> Result<Target, ShareError>
    {
        if source.owner() != receipt.owner()
        {
            return Err(Error::Foreign { alias:    receipt.id(),
                                        expected: source.owner(),
                                        actual:   receipt.owner() }.into());
        }
        if !self.system.accepting()
        {
            return Err(PoolError::Stopping { owner: self.system.owner() }.into());
        }
        // Completing permits the original pending/deferred source alias. Its
        // manager must retain its real reader across preparation and birth.
        let alias = self.completed_alias(&receipt)
                        .ok_or(ShareError::SourceChanged { alias: receipt.id() })?;
        if self.objects.completion_key(alias.target.id) != Some(alias.target.completion)
           || self.objects.get(alias.target.id).is_none()
        {
            return Err(Error::TargetGone { alias: receipt.id() }.into());
        }
        Ok(alias.target)
    }

    fn check_share_source(&self, source: &SourceProof<G>) -> Result<(), ShareError>
    {
        if self.share_source(&source.owner, source.alias)? != source.target
        {
            return Err(ShareError::SourceChanged { alias: source.alias.id() });
        }
        Ok(())
    }

    pub(super) fn deferred_required(&self, additional: usize) -> Result<usize, ShareError>
    {
        self.deferred
            .len()
            .checked_add(self.aliases.stored_len())
            .and_then(|n| n.checked_add(self.aliases.publication_credits()))
            .and_then(|n| n.checked_add(additional))
            .ok_or_else(|| PoolError::InvalidGrowth.into())
    }

    /// An original full completion receipt and genuine source/recipient owners
    /// select the actual existing target. Stopping the source does not revoke
    /// the manager's retained immutable read; recipient admission still applies.
    pub fn plan_share(&self, source: &OwnerRef<G>, receipt: store::CompletionKey,
                      recipient: &OwnerRef<G>)
                      -> Result<SharePlan<G, S>, ShareError>
    {
        let target = self.share_source(source, receipt)?;
        let aliases = self.aliases.plan_publication(recipient)?;
        let required = self.deferred_required(1)?;
        Ok(SharePlan { aliases,
                       source: SourceProof { owner: source.clone(),
                                             alias: receipt,
                                             target },
                       recipient: recipient.owner(),
                       kind: self.alias_kind,
                       deferred_capacity: if self.deferred.capacity() < required
                       {
                           required
                       }
                       else
                       {
                           0
                       } })
    }

    /// No allocation, payload construction or disposal occurs here. Capacity
    /// installed before a later refusal remains resident; displaced owners are
    /// returned in the SAME preparation for destruction outside the guard.
    pub fn admit_share(
        &mut self, mut preparation: PreparedShare<G, S>)
        -> Result<(SharePermit<G>, RetiredShareCapacity<G, S>), ShareAdmissionFailure<G, S>>
    {
        let checked =
            self.check_share_source(&preparation.source).and_then(|()| self.deferred_required(1));
        let required = match checked
        {
            Ok(required) => required,
            Err(cause) =>
            {
                return Err(ShareAdmissionFailure { cause,
                                                   preparation });
            }
        };
        if self.deferred.capacity() < required
        {
            if preparation.deferred.capacity() < required
            {
                return Err(ShareAdmissionFailure { cause: ShareError::CapacityChanged,
                                                   preparation });
            }
            // Capacity is checked first; append only moves the existing unique
            // removal guards. The old empty allocation travels out in failure
            // or retired capacity, never through a destructor in this turn.
            preparation.deferred.append(&mut self.deferred);
            core::mem::swap(&mut preparation.deferred, &mut self.deferred);
        }
        match self.aliases.admit_prepared(preparation.aliases)
        {
            Ok((aliases, retired)) =>
            {
                Ok((SharePermit { aliases,
                                  source: preparation.source },
                    RetiredShareCapacity { aliases:  retired,
                                           deferred: preparation.deferred }))
            }
            Err(failure) =>
            {
                Err(ShareAdmissionFailure { cause:       ShareError::Publication(failure.cause),
                                            preparation: PreparedShare { aliases:
                                                                             failure.preparation,
                                                                         source:
                                                                             preparation.source,
                                                                         deferred:
                                                                             preparation.deferred } })
            }
        }
    }

    pub fn publish_share(
        &mut self, permit: SharePermit<G>, value: S)
        -> Result<(store::Publication, RetiredSharePublication<G>), SharePublishFailure<G, S>>
    {
        let checked = self.check_share_source(&permit.source).and_then(|()| {
            let required = self.deferred_required(0)?;
            if self.deferred.capacity() < required { return Err(ShareError::CapacityChanged); }
            let object = self.objects.get(permit.source.target.id)
                .ok_or(Error::TargetGone { alias: permit.source.alias.id() })?;
            object.references.load(Ordering::Acquire).checked_add(1)
                .ok_or_else(|| Error::ReferenceLimit { alias: permit.source.alias.id() }.into())
        });
        let next = match checked
        {
            Ok(next) => next,
            Err(cause) =>
            {
                return Err(SharePublishFailure { cause,
                                                 permit,
                                                 value });
            }
        };
        let SharePermit { aliases,
                          source } = permit;
        let alias = Alias { target: source.target,
                            tag:    value };
        // The immutable object was validated above, and &mut self excludes its
        // retirement until the successful birth's refcount update below.
        let object = match self.objects.get(source.target.id)
        {
            Some(object) => object,
            None => return Err(SharePublishFailure { cause: Error::TargetGone { alias: source.alias.id() }.into(),
                permit: SharePermit { aliases, source }, value: alias.tag })
        };
        match self.aliases.publish_prepared(aliases, alias)
        {
            Ok((publication, retired)) =>
            {
                object.references.store(next, Ordering::Release);
                Ok((publication,
                    RetiredSharePublication { publication: retired,
                                              source }))
            }
            Err(failure) =>
            {
                Err(SharePublishFailure { cause:  ShareError::Publication(failure.cause),
                                          permit: SharePermit { aliases: failure.permit,
                                                                source },
                                          value:  failure.value.tag })
            }
        }
    }
}

//! Detached immutable interning. Negative observations use the canonical
//! object pool's existing birth counter, never a second identity or revision.
use super::*;

#[derive(Clone, Copy, Debug)]
pub enum InternError
{
    Alias(Error),
    Publication(store::PreparedError),
    Changed,
    CounterExhausted,
    CapacityChanged
}
impl From<PoolError> for InternError
{
    fn from(cause: PoolError) -> Self
    {
        Self::Alias(Error::Pool(cause))
    }
}
impl fmt::Display for InternError
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Alias(cause) => cause.fmt(out),
            Self::Publication(cause) => cause.fmt(out),
            Self::Changed =>
            {
                out.write_str("canonical immutable source set changed; replan with the retained \
                               input")
            }
            Self::CounterExhausted =>
            {
                out.write_str("canonical System object birth counter exhausted; restart required")
            }
            Self::CapacityChanged =>
            {
                out.write_str("immutable alias retirement capacity changed; replan outside manager")
            }
        }
    }
}
#[derive(Clone, Copy)]
enum Observation
{
    Existing(Target),
    Absent(u64)
}
pub struct InternFailure<P>
{
    pub cause:       InternError,
    pub preparation: P
}
pub struct InternPlan<T, G: InvocationGate, S>
{
    value:             T,
    observation:       Observation,
    recipient:         DomainId,
    kind:              crate::domain::KindCode,
    aliases:           store::PublicationPlan<Alias<S>, G>,
    objects:           Option<store::PublicationPlan<Object<T>, G>>,
    deferred_capacity: usize
}
// A partial admission remains the same affine preparation. No successful
// first admission is discarded under a manager if the second one refuses.
enum Capacity<T, G: InvocationGate>
{
    Prepared(store::PreparedPublication<T, G>),
    Admitted(store::PublicationPermit<G>)
}
pub struct PreparedIntern<T, G: InvocationGate, S>
{
    value:           T,
    observation:     Observation,
    aliases:         Capacity<Alias<S>, G>,
    objects:         Option<Capacity<Object<T>, G>>,
    retired_aliases: Option<store::RetiredCapacity<Alias<S>, G>>,
    retired_objects: Option<store::RetiredCapacity<Object<T>, G>>,
    deferred:        Vec<store::Removed<Alias<S>, G>>
}
pub struct InternPermit<T, G: InvocationGate>
{
    value:       T,
    observation: Observation,
    aliases:     store::PublicationPermit<G>,
    objects:     Option<store::PublicationPermit<G>>
}
pub struct RetiredInternCapacity<T, G: InvocationGate, S>
{
    aliases:  Option<store::RetiredCapacity<Alias<S>, G>>,
    objects:  Option<store::RetiredCapacity<Object<T>, G>>,
    deferred: Vec<store::Removed<Alias<S>, G>>
}
impl<T, G: InvocationGate, S> RetiredInternCapacity<T, G, S>
{
    pub fn dispose(self)
    {
        drop(self.deferred);
        if let Some(x) = self.aliases
        {
            x.dispose();
        }
        if let Some(x) = self.objects
        {
            x.dispose();
        }
    }
}
/// Payloads precede canonical publication owners on disposal. A retained
/// rollback is still in the real Store and original System ledger walk.
pub struct RetiredInternPublication<T, G: InvocationGate>
{
    unused:  Option<T>,
    removed: Option<store::Removal<Object<T>, G, ()>>,
    aliases: Option<store::RetiredPublication<G>>,
    objects: Option<store::RetiredPublication<G>>
}
impl<T, G: InvocationGate> RetiredInternPublication<T, G>
{
    pub fn dispose(self)
    {
        drop(self.unused);
        drop(self.removed);
        if let Some(x) = self.aliases
        {
            x.dispose();
        }
        if let Some(x) = self.objects
        {
            x.dispose();
        }
    }
}
#[derive(Debug)]
pub struct InternRollback
{
    completion: store::CompletionKey,
    pending:    bool
}
impl InternRollback
{
    pub fn pending(&self) -> bool
    {
        self.pending
    }
}
impl fmt::Display for InternRollback
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        write!(out,
               "new canonical System object {} in {} retained after alias refusal; actual ledger \
                pending={}; retained canonical record awaits the existing cleanup protocol, no \
                ACK issued",
               self.completion.id(),
               self.completion.owner(),
               self.pending)
    }
}
pub struct InternPublishFailure<T, G: InvocationGate, S>
{
    pub cause:       InternError,
    pub rollback:    Option<InternRollback>,
    // Public opaque owning remainder; caller must dispose it outside manager.
    pub preparation: InternRejected<T, G, S>
}
pub struct InternRejected<T, G: InvocationGate, S>
{
    value:   Option<T>,
    tag:     S,
    aliases: store::PublicationPermit<G>,
    objects: Option<store::PublicationPermit<G>>,
    retired: RetiredInternPublication<T, G>
}
impl<T, G: InvocationGate, S> InternPlan<T, G, S>
{
    pub fn prepare(self) -> Result<PreparedIntern<T, G, S>, InternFailure<T>>
    {
        let Self { value,
                   observation,
                   recipient,
                   kind,
                   aliases,
                   objects,
                   deferred_capacity } = self;
        let prepared = (|| {
            let mut deferred = Vec::new();
            G::with_system_allocation(||deferred.try_reserve_exact(deferred_capacity))
                .map_err(|_|PoolError::Storage{owner:recipient,kind,used:0,requested_slots:deferred_capacity,limit:None})?;
            let aliases = aliases.prepare()?;
            let objects = match objects
            {
                Some(plan) => Some(G::with_system_allocation(|| plan.prepare())?),
                None => None
            };
            Ok::<_, InternError>((aliases, objects, deferred))
        })();
        match prepared
        {
            Ok((aliases, objects, deferred)) =>
            {
                Ok(PreparedIntern { value,
                                    observation,
                                    aliases: Capacity::Prepared(aliases),
                                    objects: objects.map(Capacity::Prepared),
                                    deferred,
                                    retired_aliases: None,
                                    retired_objects: None })
            }
            Err(cause) =>
            {
                Err(InternFailure { cause,
                                    preparation: value })
            }
        }
    }
}
impl<T, G: InvocationGate, S> Store<T, G, S>
{
    fn object_births(&self) -> Result<u64, InternError>
    {
        if !self.system.accepting()
        {
            return Err(PoolError::Stopping { owner: self.system.owner() }.into());
        }
        let births = self.system.access(|index| {
                                    index.pools()
                                         .find(|pool| pool.kind() == self.objects.kind())
                                         .map_or(0, |pool| pool.counters().0)
                                });
        if births == u64::MAX { Err(InternError::CounterExhausted) } else { Ok(births) }
    }

    fn check_intern(&self, observation: Observation) -> Result<(), InternError>
    {
        match observation
        {
            Observation::Absent(births) =>
            {
                if self.object_births()? == births { Ok(()) } else { Err(InternError::Changed) }
            }
            Observation::Existing(target) =>
            {
                if !self.system.accepting()
                {
                    return Err(PoolError::Stopping { owner: self.system.owner() }.into());
                }
                if self.objects.completion_key(target.id) == Some(target.completion)
                   && self.objects.get(target.id).is_some()
                {
                    Ok(())
                }
                else
                {
                    Err(InternError::Changed)
                }
            }
        }
    }

    pub fn plan_intern(&self, recipient: &OwnerRef<G>, value: T)
                       -> Result<InternPlan<T, G, S>, InternFailure<T>>
        where T: PartialEq
    {
        let observed = (|| {
            let births = self.object_births()?;
            let existing =
                self.objects.iter().find_map(|(id, object)| (object.value == value).then_some(id));
            let observation = match existing
            {
                Some(id) =>
                {
                    Observation::Existing(Target { id,
                                                   completion: self.objects
                                                                   .completion_key(id)
                                                                   .ok_or(InternError::Changed)? })
                }
                None => Observation::Absent(births)
            };
            let aliases = self.aliases.plan_publication(recipient)?;
            let objects = if matches!(observation, Observation::Absent(_))
            {
                Some(self.objects.plan_publication(&self.system)?)
            }
            else
            {
                None
            };
            let required = self.deferred_required(1).map_err(intern_share_error)?;
            Ok::<_, InternError>((observation,
                                  aliases,
                                  objects,
                                  if self.deferred.capacity() < required { required } else { 0 }))
        })();
        match observed
        {
            Ok((observation, aliases, objects, deferred_capacity)) =>
            {
                Ok(InternPlan { value,
                                observation,
                                recipient: recipient.owner(),
                                kind: self.alias_kind,
                                aliases,
                                objects,
                                deferred_capacity })
            }
            Err(cause) =>
            {
                Err(InternFailure { cause,
                                    preparation: value })
            }
        }
    }

    pub fn admit_intern(
        &mut self, mut p: PreparedIntern<T, G, S>)
        -> Result<(InternPermit<T, G>, RetiredInternCapacity<T, G, S>),
                  InternFailure<PreparedIntern<T, G, S>>>
    {
        let checked=self.check_intern(p.observation).and_then(|()|self.deferred_required(usize::from(matches!(p.aliases,Capacity::Prepared(_)))).map_err(intern_share_error));
        let required = match checked
        {
            Ok(n) => n,
            Err(cause) =>
            {
                return Err(InternFailure { cause,
                                           preparation: p });
            }
        };
        if self.deferred.capacity() < required
        {
            if p.deferred.capacity() < required
            {
                return Err(InternFailure { cause:       InternError::CapacityChanged,
                                           preparation: p });
            }
            p.deferred.append(&mut self.deferred);
            core::mem::swap(&mut p.deferred, &mut self.deferred);
        }
        let mut refusal = None;
        p.aliases = match p.aliases
        {
            Capacity::Prepared(prepared) =>
            {
                match self.aliases.admit_prepared(prepared)
                {
                    Ok((permit, old)) =>
                    {
                        p.retired_aliases = Some(old);
                        Capacity::Admitted(permit)
                    }
                    Err(failure) =>
                    {
                        refusal = Some(InternError::Publication(failure.cause));
                        Capacity::Prepared(failure.preparation)
                    }
                }
            }
            other => other
        };
        if let Some(cause) = refusal
        {
            return Err(InternFailure { cause,
                                       preparation: p });
        }
        p.objects = match p.objects
        {
            Some(Capacity::Prepared(prepared)) =>
            {
                Some(match self.objects.admit_prepared(prepared)
                     {
                         Ok((permit, old)) =>
                         {
                             p.retired_objects = Some(old);
                             Capacity::Admitted(permit)
                         }
                         Err(failure) =>
                         {
                             refusal = Some(InternError::Publication(failure.cause));
                             Capacity::Prepared(failure.preparation)
                         }
                     })
            }
            other => other
        };
        if let Some(cause) = refusal
        {
            return Err(InternFailure { cause,
                                       preparation: p });
        }
        match (p.aliases, p.objects)
        {
            (Capacity::Admitted(aliases), objects @ (None | Some(Capacity::Admitted(_)))) =>
            {
                let objects = match objects
                {
                    Some(Capacity::Admitted(permit)) => Some(permit),
                    _ => None
                };
                Ok((InternPermit { value: p.value,
                                   observation: p.observation,
                                   aliases,
                                   objects },
                    RetiredInternCapacity { aliases:  p.retired_aliases,
                                            objects:  p.retired_objects,
                                            deferred: p.deferred }))
            }
            (aliases, objects) =>
            {
                Err(InternFailure { cause:       InternError::CapacityChanged,
                                    preparation: PreparedIntern { aliases,
                                                                  objects,
                                                                  ..p } })
            }
        }
    }
}
fn intern_share_error(cause: ShareError) -> InternError
{
    match cause
    {
        ShareError::Alias(cause) => InternError::Alias(cause),
        ShareError::Publication(cause) => InternError::Publication(cause),
        ShareError::CapacityChanged => InternError::CapacityChanged,
        ShareError::SourceChanged { .. } => InternError::Changed
    }
}
impl<T, G: InvocationGate, S> Store<T, G, S>
{
    pub fn publish_intern(
        &mut self, permit: InternPermit<T, G>, tag: S)
        -> Result<(store::Publication, RetiredInternPublication<T, G>),
                  InternPublishFailure<T, G, S>>
    {
        let empty = RetiredInternPublication { unused:  None,
                                               removed: None,
                                               aliases: None,
                                               objects: None };
        let checked=self.check_intern(permit.observation).and_then(|()| {
            self.aliases.validate_prepared(&permit.aliases).map_err(InternError::Publication)?;
            if let Some(object)=permit.objects.as_ref(){self.objects.validate_prepared(object).map_err(InternError::Publication)?;}
            let required=self.deferred_required(0).map_err(intern_share_error)?;
            if self.deferred.capacity()<required{return Err(InternError::CapacityChanged);}
            match permit.observation {
                Observation::Existing(target)=>{
                    let object=self.objects.completing(&target.completion).ok_or(InternError::Changed)?;
                    object.references.load(Ordering::Acquire).checked_add(1).ok_or(InternError::Alias(Error::ReferenceLimit{alias:permit.aliases.id()}))?;
                    if permit.objects.is_some(){return Err(InternError::Changed);}
                },
                Observation::Absent(_)=>if permit.objects.is_none(){return Err(InternError::Changed);}
            }
            Ok(())
        });
        if let Err(cause) = checked
        {
            return Err(InternPublishFailure { cause,
                                              rollback: None,
                                              preparation: InternRejected { value:
                                                                                Some(permit.value),
                                                                            tag,
                                                                            aliases:
                                                                                permit.aliases,
                                                                            objects:
                                                                                permit.objects,
                                                                            retired: empty } });
        }
        let InternPermit { value,
                           observation,
                           aliases,
                           objects } = permit;
        let (target,fresh,mut retired)=match (observation,objects) {
            (Observation::Existing(target),None)=>(target,false,RetiredInternPublication{unused:Some(value),..empty}),
            (Observation::Absent(_),Some(object_permit))=>{
                match self.objects.publish_prepared(object_permit,Object{value,references:AtomicUsize::new(0)}) {
                    Ok((publication,old))=>{
                        let (id,completion)=publication.into_parts();
                        (Target{id,completion},true,RetiredInternPublication{objects:Some(old),..empty})
                    },
                    Err(failure)=>return Err(InternPublishFailure{cause:InternError::Publication(failure.cause),rollback:None,preparation:InternRejected{value:Some(failure.value.value),tag,aliases,objects:Some(failure.permit),retired:empty}})
                }
            },
            (_,objects)=>return Err(InternPublishFailure{cause:InternError::Changed,rollback:None,preparation:InternRejected{value:Some(value),tag,aliases,objects,retired:empty}})
        };
        let attempt = (|| {
            if !self.system.accepting()
            {
                return Err((InternError::Alias(Error::Pool(PoolError::Stopping { owner:
                                                                                     self.system
                                                                                         .owner() })),
                            aliases,
                            tag));
            }
            let Some(object) = self.objects.completing(&target.completion)
            else
            {
                return Err((InternError::Changed, aliases, tag));
            };
            let Some(next) = object.references.load(Ordering::Acquire).checked_add(1)
            else
            {
                return Err((InternError::Alias(Error::ReferenceLimit { alias: aliases.id() }),
                            aliases,
                            tag));
            };
            match self.aliases.publish_prepared(aliases,
                                                Alias { target,
                                                        tag })
            {
                Ok((publication, old)) =>
                {
                    object.references.store(next, Ordering::Release);
                    Ok((publication, old))
                }
                Err(failure) =>
                {
                    Err((InternError::Publication(failure.cause),
                         failure.permit,
                         failure.value.tag))
                }
            }
        })();
        match attempt
        {
            Ok((publication, old)) =>
            {
                retired.aliases = Some(old);
                Ok((publication, retired))
            }
            Err((cause, aliases, tag)) =>
            {
                let mut rollback = None;
                if fresh
                {
                    retired.removed = self.objects.remove_with_backing(target.id);
                    if retired.removed.is_none()
                    {
                        let pending = self.objects.pending_completion(&target.completion);
                        rollback = Some(InternRollback { completion: target.completion,
                                                         pending });
                    }
                }
                Err(InternPublishFailure { cause,
                                           rollback,
                                           preparation: InternRejected { value: None,
                                                                         tag,
                                                                         aliases,
                                                                         objects: None,
                                                                         retired } })
            }
        }
    }
}

impl<T, G: InvocationGate, S> InternRejected<T, G, S>
{
    pub fn dispose(self)
    {
        drop(self.value);
        drop(self.tag);
        self.retired.dispose();
        drop(self.aliases);
        drop(self.objects);
    }
}

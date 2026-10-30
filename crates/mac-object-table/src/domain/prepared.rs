//! Prepared ordinary value capacity. No allocation or owning destruction occurs
//! while installing into the authoritative owner ledger.
use super::*;

#[derive(Clone, Copy)]
pub(crate) struct PoolRequest
{
    pub kind:         KindCode,
    pub policy:       CapacityPolicy,
    pub registration: Option<u64>,
    pub attach:       bool
}
#[derive(Clone, Copy)]
struct PoolPlan
{
    request: PoolRequest,
    slots:   usize,
    grow:    bool
}
pub(crate) struct LedgerCapacityPlan
{
    owner:     DomainId,
    directory: usize,
    pools:     [Option<PoolPlan>; 2]
}
struct PreparedPool
{
    plan:     PoolPlan,
    metadata: Option<DomainSlots>
}
pub(crate) struct PreparedLedgerCapacity
{
    owner:     DomainId,
    directory: Vec<DomainSlots>,
    pools:     [Option<PreparedPool>; 2]
}

impl DomainSlots
{
    fn prepared_slot(&self) -> Result<Option<usize>, PoolError>
    {
        let len = self.generations.len();
        let words = len.div_ceil(64);
        if self.parents.len() != len
           || [self.occupied.len(),
               self.reserved.len(),
               self.retired.len(),
               self.pending.len(),
               self.quarantined.len()].iter()
                                      .any(|&n| n != words)
        {
            return Err(PoolError::InvalidGrowth);
        }
        for word in 0..words
        {
            let remaining = len - word * 64;
            let valid = if remaining >= 64 { u64::MAX } else { (1u64 << remaining) - 1 };
            let free = !(self.occupied[word]
                         | self.reserved[word]
                         | self.retired[word]
                         | self.pending[word])
                       & valid;
            if free != 0
            {
                return Ok(Some(word * 64 + free.trailing_zeros() as usize));
            }
        }
        Ok(None)
    }

    fn prepared_binding(&self, request: PoolRequest) -> Result<(), PoolError>
    {
        if self.policy != request.policy
           || request.attach && (self.value_storage || self.live != 0 || self.pending_count != 0)
           || !request.attach && (!self.value_storage || self.registration != request.registration)
        {
            return Err(PoolError::ValueStorageAttached { owner: self.owner,
                                                         kind:  self.kind });
        }
        Ok(())
    }
}

impl OwnerIndex
{
    pub(crate) fn plan_value_capacity(&self, requests: [Option<PoolRequest>; 2])
                                      -> Result<LedgerCapacityPlan, PoolError>
    {
        let mut plans = [None, None];
        let mut directory = self.pools.len();
        for (at, request) in requests.into_iter().enumerate()
        {
            let Some(request) = request
            else
            {
                continue;
            };
            self.authorize(request.kind, None)?;
            let (slots, grow) = if let Some(pool) =
                self.pools.iter().find(|p| p.kind == request.kind)
            {
                pool.prepared_binding(request)?;
                let len = pool.storage_slots();
                if pool.prepared_slot()?.is_some()
                {
                    (len, false)
                }
                else
                {
                    if request.policy.limit().is_some_and(|limit| len >= limit)
                    {
                        return Err(pool.exhausted());
                    }
                    let next = len.checked_mul(2)
                                  .filter(|&n| n > len)
                                  .or_else(|| len.checked_add(1))
                                  .ok_or(PoolError::InvalidGrowth)?;
                    (request.policy.limit().map_or(next, |limit| next.min(limit)), true)
                }
            }
            else
            {
                if !request.attach
                {
                    return Err(PoolError::UnregisteredKind { owner: self.owner,
                                                             kind:  request.kind });
                }
                directory = directory.checked_add(1).ok_or(PoolError::InvalidGrowth)?;
                if request.policy.limit() == Some(0)
                {
                    return Err(PoolError::InvalidGrowth);
                }
                (1, true)
            };
            plans[at] = Some(PoolPlan { request,
                                        slots,
                                        grow });
        }
        let directory = if self.pools.capacity() < directory { directory } else { 0 };
        Ok(LedgerCapacityPlan { owner: self.owner,
                                directory,
                                pools: plans })
    }
}

impl LedgerCapacityPlan
{
    pub(crate) fn slots(&self) -> usize
    {
        self.pools[0].map_or(0, |pool| pool.slots)
    }

    pub(crate) fn prepare(self) -> Result<PreparedLedgerCapacity, PoolError>
    {
        let kind = self.pools[0].ok_or(PoolError::InvalidGrowth)?.request.kind;
        let error = PoolError::Storage { owner: self.owner,
                                         kind,
                                         used: 0,
                                         requested_slots: self.directory,
                                         limit: None };
        let mut directory = Vec::new();
        directory.try_reserve_exact(self.directory).map_err(|_| error)?;
        let mut pools = [None, None];
        for (at, plan) in self.pools.into_iter().enumerate()
        {
            let Some(plan) = plan
            else
            {
                continue;
            };
            let metadata = if plan.grow
            {
                let mut pool =
                    DomainSlots::new(self.owner, plan.request.kind, plan.request.policy)?;
                // This is outside all ledger/Store guards. Existing fresh-slot
                // semantics initialize only newly admitted capacity.
                pool.grow_storage(plan.slots)?;
                Some(pool)
            }
            else
            {
                None
            };
            pools[at] = Some(PreparedPool { plan,
                                            metadata });
        }
        Ok(PreparedLedgerCapacity { owner: self.owner,
                                    directory,
                                    pools })
    }
}

impl PreparedLedgerCapacity
{
    pub(crate) fn directory_fits(&self, index: &OwnerIndex) -> Result<bool, PoolError>
    {
        let missing =
            self.pools
                .iter()
                .flatten()
                .filter(|prepared| {
                    !index.pools.iter().any(|pool| pool.kind == prepared.plan.request.kind)
                })
                .count();
        let required = index.pools.len().checked_add(missing).ok_or(PoolError::InvalidGrowth)?;
        Ok(index.pools.capacity() >= required || self.directory.capacity() >= required)
    }

    pub(crate) fn slots(&self) -> usize
    {
        self.pools[0].as_ref().map_or(0, |pool| pool.plan.slots)
    }

    /// Capacity only: no attachment, slot reservation or birth occurs here.
    /// Swapped-out empty directory and old metadata remain owned by self.
    pub(crate) fn install(&mut self, index: &mut OwnerIndex) -> Result<(), PoolError>
    {
        if self.owner != index.owner
        {
            return Err(PoolError::ForeignOwner { expected: self.owner,
                                                 actual:   index.owner });
        }
        let mut missing = 0usize;
        for prepared in self.pools.iter().flatten()
        {
            let request = prepared.plan.request;
            index.authorize(request.kind, None)?;
            if let Some(pool) = index.pools.iter().find(|p| p.kind == request.kind)
            {
                pool.prepared_binding(request)?;
                pool.prepared_slot()?;
                if pool.storage_slots() < prepared.plan.slots && prepared.metadata.is_none()
                {
                    return Err(PoolError::InvalidGrowth);
                }
            }
            else
            {
                if !request.attach || prepared.metadata.is_none()
                {
                    return Err(PoolError::InvalidGrowth);
                }
                missing = missing.checked_add(1).ok_or(PoolError::InvalidGrowth)?;
            }
        }
        let required = index.pools.len().checked_add(missing).ok_or(PoolError::InvalidGrowth)?;
        if index.pools.capacity() < required
        {
            if self.directory.capacity() < required
            {
                return Err(PoolError::InvalidGrowth);
            }
            self.directory.extend(index.pools.drain(..));
            core::mem::swap(&mut index.pools, &mut self.directory);
        }
        for prepared in self.pools.iter_mut().flatten()
        {
            let request = prepared.plan.request;
            if let Some(pool) = index.pool_mut(request.kind)
            {
                if pool.storage_slots() >= prepared.plan.slots
                {
                    continue;
                }
                let metadata = prepared.metadata.as_mut().ok_or(PoolError::InvalidGrowth)?;
                macro_rules! preserve {
                    ($field:ident) => {{
                        metadata.$field[..pool.$field.len()].copy_from_slice(&pool.$field);
                        core::mem::swap(&mut pool.$field, &mut metadata.$field);
                    }};
                }
                // Copy CURRENT contents, preserving every generation, pending,
                // quarantined, retired and parent bit. Counters/cursors stay.
                preserve!(occupied);
                preserve!(reserved);
                preserve!(retired);
                preserve!(pending);
                preserve!(quarantined);
                preserve!(generations);
                preserve!(parents);
            }
            else
            {
                let mut pool = prepared.metadata.take().ok_or(PoolError::InvalidGrowth)?;
                pool.registration = request.registration;
                index.pools.push(pool);
            }
        }
        index.revision = index.revision.saturating_add(1);
        Ok(())
    }

    /// One owner turn validates both pools BEFORE attachment/reservation/birth.
    /// None of these effects allocate. Returned slot is not a public identity.
    pub(crate) fn admit(&self, index: &mut OwnerIndex)
                        -> Result<(usize, Option<ResourceKey>), PoolError>
    {
        let mut slots = [None, None];
        for (at, prepared) in self.pools.iter().enumerate()
        {
            let Some(prepared) = prepared
            else
            {
                continue;
            };
            let request = prepared.plan.request;
            index.authorize(request.kind, None)?;
            let pool = index.pools
                            .iter()
                            .find(|p| p.kind == request.kind)
                            .ok_or(PoolError::UnregisteredKind { owner: self.owner,
                                                                 kind:  request.kind })?;
            pool.prepared_binding(request)?;
            let Some(slot) = pool.prepared_slot()?
            else
            {
                return Err(PoolError::GrowthRequired { owner:         self.owner,
                                                       kind:          request.kind,
                                                       used:          pool.live,
                                                       retired:       pool.retired_count,
                                                       pending:       pool.pending_count,
                                                       storage_slots: pool.storage_slots(),
                                                       policy_limit:  pool.policy.limit() });
            };
            if at == 1 && pool.live.checked_add(1).is_none()
            {
                return Err(PoolError::InvalidGrowth);
            }
            slots[at] = Some(slot);
        }
        let first = self.pools[0].as_ref().ok_or(PoolError::InvalidGrowth)?;
        let slot = slots[0].ok_or(PoolError::InvalidGrowth)?;
        // Every checked index/pool remains exclusively owned by this same turn.
        for prepared in self.pools.iter().flatten()
        {
            if prepared.plan.request.attach
            {
                let pool =
                    index.pool_mut(prepared.plan.request.kind)
                         .expect("prepared admission validated this exact pool before its commit");
                pool.value_storage = true;
                pool.registration = prepared.plan.request.registration;
            }
        }
        index.pool_mut(first.plan.request.kind)
             .expect("prepared admission retains its validated value pool")
             .reserved[slot / 64] |= 1 << (slot % 64);
        let storage = match (self.pools[1].as_ref(), slots[1])
        {
            (Some(prepared), Some(slot)) =>
            {
                Some(index.pool_mut(prepared.plan.request.kind)
                          .expect("prepared admission retains its validated backing pool")
                          .occupy(slot, None))
            }
            _ => None
        };
        index.revision = index.revision.saturating_add(1);
        Ok((slot, storage))
    }
}

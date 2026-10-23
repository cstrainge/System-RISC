//! Read-only inspection of authoritative ledgers. No resident snapshot objects,
//! object-value scans, formatting allocations, or caller-selected identity.
#![forbid(unsafe_code)]
use mac_abi::{inspection::*, *};
use mac_object_table::{domain::{CapacityPolicy, Directory, DomainKind, LedgerReadError},
                       shared::InvocationGate};

/// Implemented by trusted resident process metadata, never by an ABI caller.
pub(crate) trait Subject
{
    fn label(&self) -> &str;
    fn foreground(&self) -> bool;
    fn authority(&self) -> u64;
}

fn reject(call: &mut Call, status: Status, message: &str) -> Status
{
    // All messages here are bounded constants. Do not truncate a UTF-8 scalar.
    call.detail_length = message.len() as u32;
    call.detail[..message.len()].copy_from_slice(message.as_bytes());
    status
}

pub(crate) fn request<T: Subject, G: InvocationGate>(directory: &Directory<T, G>, caller: u64,
                                                     call: &mut Call)
                                                     -> Status
{
    call.detail_length = 0;
    call.detail.fill(0);
    call.domain = Domain::default();
    call.pool = Pool::default();
    call.count = 0;
    if call.reserved != 0 || ![SELF, ALL].contains(&call.scope)
    {
        return reject(call, BAD_ARGUMENT, "invalid inspection scope or reserved field");
    }
    let Some(subject) = directory.get(caller)
    else
    {
        return reject(call, ACCESS_DENIED, "inspection caller has no live process domain");
    };
    if call.scope == ALL && subject.value().authority() & INSPECT_DOMAINS == 0
    {
        return reject(call,
                      ACCESS_DENIED,
                      "cross-domain inspection requires a System policy grant; own-domain \
                       inspection remains available");
    }
    if directory.revision() == u64::MAX
    {
        return reject(call,
                      NOT_SUPPORTED,
                      "inspection membership revision exhausted; restart System to obtain new \
                       snapshots");
    }
    call.coverage = PROCESS_DOMAINS | PARTIAL_LEDGERS;
    match call.operation
    {
        BEGIN =>
        {
            call.revision = directory.revision();
            call.count = if call.scope == SELF { 1 } else { directory.len() as u64 };
            OK
        }
        DOMAIN | LEDGER_POOL =>
        {
            if call.revision != directory.revision()
            {
                return reject(call,
                              SCAN_STALE,
                              "process membership changed during inspection; obtain a new \
                               snapshot and retry");
            }
            let entry = if call.scope == SELF
            {
                (call.index == 0).then_some(subject)
            }
            else
            {
                usize::try_from(call.index).ok().and_then(|i| directory.iter().nth(i))
            };
            let Some(entry) = entry
            else
            {
                return reject(call, NOT_FOUND, "domain index is outside this inspection snapshot");
            };
            if entry.owner().kind() != DomainKind::Process
            {
                return reject(call, NOT_SUPPORTED,
                              "this inspection source does not describe a process domain");
            }
            if call.operation == LEDGER_POOL
            {
                if call.scope == SELF && call.subject != caller
                {
                    return reject(call,
                                  ACCESS_DENIED,
                                  "ledger snapshot belongs to a different lifetime domain; \
                                   obtain your own snapshot");
                }
                if call.subject != entry.owner().incarnation()
                {
                    return reject(call,
                                  SCAN_STALE,
                                  "ledger subject changed; obtain a new ledger snapshot and \
                                   retry");
                }
                let Ok(index) = usize::try_from(call.pool_index)
                else
                {
                    return reject(call, NOT_FOUND, "pool index is outside this ledger snapshot");
                };
                let pool = match entry.ledger().pool_at(call.ledger_revision, index)
                {
                    Ok(pool) => pool,
                    Err(LedgerReadError::Stale { .. }) =>
                    {
                        return reject(call,
                                      SCAN_STALE,
                                      "domain ledger changed while reading pools; obtain a new \
                                       ledger snapshot and retry");
                    }
                    Err(LedgerReadError::RevisionExhausted { .. }) =>
                    {
                        return reject(call,
                                      NOT_SUPPORTED,
                                      "domain ledger revision exhausted; restart this domain to \
                                       obtain new snapshots");
                    }
                    Err(LedgerReadError::PoolNotFound { .. }) =>
                    {
                        return reject(call, NOT_FOUND,
                                      "pool index is outside this ledger snapshot");
                    }
                };
                let (policy, limit, reason) = match pool.policy
                {
                    CapacityPolicy::GrowableUntilMemory => (CAPACITY_GROWABLE, 0, ""),
                    CapacityPolicy::PerDomainQuota { slots } => (CAPACITY_QUOTA, slots as u64, ""),
                    CapacityPolicy::HardBound { slots,
                                                reason } => (CAPACITY_BOUND, slots as u64, reason)
                };
                if reason.len() > call.pool.reason.len()
                {
                    return reject(call, CORRUPT_DATA,
                                  "pool capacity reason exceeds the inspection wire limit");
                }
                call.pool = Pool { kind: pool.kind.0,
                                   policy,
                                   limit,
                                   live: pool.live as u64,
                                   retired: pool.retired as u64,
                                   pending: pool.pending as u64,
                                   slots: pool.slots as u64,
                                   births: pool.births,
                                   deaths: pool.deaths,
                                   generation_headroom: pool.generation_headroom,
                                   flags: if pool.births == u64::MAX || pool.deaths == u64::MAX
                                   {
                                       COUNTERS_SATURATED
                                   }
                                   else
                                   {
                                       0
                                   },
                                   ..Default::default() };
                call.pool.reason[..reason.len()].copy_from_slice(reason.as_bytes());
                call.pool.reason_length = reason.len() as u32;
                return OK;
            }
            let ledger = entry.ledger().summary();
            if ledger.revision == u64::MAX
            {
                return reject(call,
                              NOT_SUPPORTED,
                              "domain ledger revision exhausted; this domain cannot provide \
                               fresh inspection rows");
            }
            let label = entry.value().label();
            if label.len() > call.domain.name.len()
            {
                return reject(call, CORRUPT_DATA,
                              "resident domain label exceeds the inspection wire limit");
            }
            call.domain =
                Domain { incarnation: entry.owner().incarnation(),
                         kind: PROCESS,
                         flags: if ledger.accepting { ACCEPTING } else { 0 }
                                | if entry.value().foreground() { FOREGROUND } else { 0 }
                                | if ledger.births == u64::MAX || ledger.deaths == u64::MAX
                                {
                                    COUNTERS_SATURATED
                                }
                                else
                                {
                                    0
                                },
                         ledger_revision: ledger.revision,
                         kinds: ledger.kinds as u64,
                         live: ledger.live as u64,
                         retired: ledger.retired as u64,
                         pending: ledger.pending as u64,
                         slots: ledger.slots as u64,
                         births: ledger.births,
                         deaths: ledger.deaths,
                         ..Default::default() };
            call.domain.name[..label.len()].copy_from_slice(label.as_bytes());
            call.domain.name_length = label.len() as u32;
            OK
        }
        _ => reject(call, BAD_ARGUMENT, "unknown inspection operation")
    }
}

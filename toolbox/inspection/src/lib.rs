//! Capability-checked snapshots of resident lifetime domains.
//! Membership is revisioned; each row is an atomic ledger read with its own
//! revision. Rows do not claim a simultaneous snapshot of unrelated domains.
#![no_std]
#![forbid(unsafe_code)]
pub mod memory;
pub mod cpus;
pub use cpus::{Activity, ActivityDelta, Cpu, CpuState, Cpus, Residency, Unavailable};
use mac_abi::inspection as wire;
use mac_traps::{Client, Error, types::InlineText};
pub use memory::{MemorySummary, Protection, Region, RegionBatch, RegionKind, Regions, SystemMemory};
pub use wire::CHANGED_EVENT;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope
{
    Own,
    All
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Failure
{
    pub cause: Error,
    detail:    InlineText<{ wire::DETAIL_BYTES }>
}
impl core::fmt::Display for Failure
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        if self.detail.as_str().is_empty()
        {
            self.cause.fmt(f)
        }
        else
        {
            f.write_str(self.detail.as_str())
        }
    }
}
impl core::error::Error for Failure
{
}
pub type Result<T> = core::result::Result<T, Failure>;
fn failure(cause: Error, call: &wire::Call) -> Failure
{
    Failure { cause,
              detail: InlineText::from_reply(call.detail, call.detail_length).unwrap_or_default() }
}

/// Bytes covered by one selected budget. These are observations, not authority
/// to reserve memory or a proof that the subject is ready for retirement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountUsage
{
    pub limit:     u64,
    pub reserved:  u64,
    pub committed: u64
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryAccounting
{
    Unavailable,
    /// Actual execution backing, resident allocations/metadata and application
    /// heap extents charged to this domain's original accounts. Heap charges
    /// describe owned extents, not just live payload; do not count suballocations
    /// again. Executable images, unreceipted storage and CPU time are not covered.
    /// Shared System foundation/recovery storage is separate. This does not
    /// establish private application mappings or an all-System memory total.
    ExecutionAndOwnedMemory
    {
        kernel:      AccountUsage,
        application: AccountUsage,
        closing:     bool
    }
}
fn accounting(value: wire::Account) -> Option<MemoryAccounting>
{
    if value.coverage == wire::ACCOUNT_UNAVAILABLE
    {
        return (value == wire::Account::default()).then_some(MemoryAccounting::Unavailable);
    }
    if value.coverage != wire::ACCOUNT_EXECUTION_OWNED_MEMORY || value.closing > 1
    {
        return None;
    }
    let usage = |value: wire::AccountUsage| {
        value.reserved.checked_add(value.committed).filter(|&used| used <= value.limit)
            .map(|_| AccountUsage { limit: value.limit, reserved: value.reserved, committed: value.committed })
    };
    Some(MemoryAccounting::ExecutionAndOwnedMemory { kernel:      usage(value.kernel)?,
                                                     application: usage(value.application)?,
                                                     closing:     value.closing != 0 })
}

#[derive(Clone, Copy, Debug)]
pub struct Domain
{
    pub kind:               OwnerKind,
    pub incarnation:        u64,
    pub accepting:          bool,
    pub foreground:         bool,
    pub ledger_revision:    u64,
    pub kinds:              u64,
    /// Migrated resources only. Consult Snapshot::complete_ledgers first.
    pub live:               u64,
    pub retired:            u64,
    pub pending:            u64,
    pub quarantined:        u64,
    pub completion_faults:  u64,
    pub scoped_children:    u64,
    pub slots:              u64,
    pub births:             u64,
    pub deaths:             u64,
    pub counters_saturated: bool,
    pub accounting:         MemoryAccounting,
    pub name:               InlineText<{ mac_abi::INLINE_STRING_BYTES }>
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapacityPolicy
{
    GrowableUntilMemory,
    PerDomainQuota
    {
        slots: u64
    },
    HardBound
    {
        slots:  u64,
        reason: InlineText<{ wire::DETAIL_BYTES }>
    }
}
/// One migrated pool from the authoritative owner ledger. This is not the
/// separate global kind-registration/release-dispatch catalogue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pool
{
    pub registration:        Option<u64>,
    pub kind:                [u8; 4],
    pub capacity:            CapacityPolicy,
    pub live:                u64,
    pub retired:             u64,
    pub pending:             u64,
    pub quarantined:         u64,
    pub completion_faults:   u64,
    pub scoped_children:     u64,
    pub slots:               u64,
    pub births:              u64,
    pub deaths:              u64,
    /// Remaining increments at the generation high-water mark, including
    /// retired history; zero does not imply every slot is exhausted.
    pub generation_headroom: u64,
    pub counters_saturated:  bool
}
#[derive(Clone, Copy)]
pub struct Inspection<'a>
{
    client: Client<'a>
}
impl<'a> Inspection<'a>
{
    /// Public object-lifetime kind registrations. This query does not require
    /// inspector authority. File-kind mappings and their icons are a separate
    /// registry service, not object cleanup registrations.
    pub fn object_kinds(self) -> Result<ObjectKinds<'a>>
    {
        let mut call = wire::Call { operation: wire::KIND_BEGIN,
                                    ..Default::default() };
        self.client.inspection(&mut call).map_err(|e| failure(e, &call))?;
        Ok(ObjectKinds { client:   self.client,
                         revision: call.revision,
                         count:    call.count })
    }

    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client }
    }

    pub fn snapshot(self, scope: Scope) -> Result<Snapshot<'a>>
    {
        let mut call = wire::Call { scope: match scope
                                    {
                                        Scope::Own => wire::SELF,
                                        Scope::All => wire::ALL
                                    },
                                    ..Default::default() };
        self.client.inspection(&mut call).map_err(|error| failure(error, &call))?;
        Ok(Snapshot { client:   self.client,
                      scope:    call.scope,
                      revision: MembershipRevision { processes: call.revision,
                                                     modules:   call.module_revision },
                      count:    call.count,
                      coverage: call.coverage })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerKind
{
    System,
    Process,
    Module
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistrationSource
{
    Resident,
    BundleResource
    {
        bundle:   [u8; 16],
        resource: mac_traps::types::ResourceKey
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectKind
{
    pub id:            u64,
    pub namespace:     [u8; 4],
    pub release_owner: u64,
    pub owner_kind:    OwnerKind,
    pub label:         InlineText<{ mac_abi::INLINE_STRING_BYTES }>,
    pub provider:      InlineText<{ mac_abi::INLINE_STRING_BYTES }>,
    pub source:        RegistrationSource,
    pub capacity:      CapacityPolicy,
    pub admitting:     bool,
    /// Live resources and pinned cleanup invocations, including invalidated
    /// objects whose destruction has not completed. Not a memory usage count.
    pub uses:          u64
}
#[derive(Clone, Copy)]
pub struct ObjectKinds<'a>
{
    client:       Client<'a>,
    pub revision: u64,
    pub count:    u64
}
impl ObjectKinds<'_>
{
    pub fn at(self, index: u64) -> Result<ObjectKind>
    {
        self.read(wire::KIND, index, 0)
    }

    pub fn get(self, id: u64) -> Result<ObjectKind>
    {
        self.read(wire::KIND_ID, 0, id)
    }

    fn read(self, operation: u32, index: u64, subject: u64) -> Result<ObjectKind>
    {
        let mut call = wire::Call { operation,
                                    revision: self.revision,
                                    index,
                                    subject,
                                    ..Default::default() };
        self.client.inspection(&mut call).map_err(|e| failure(e, &call))?;
        let row = call.kind;
        if row.reserved != 0 || row.admitting > 1 || row.id == 0
        {
            return Err(failure(Error::BadAbi, &call));
        }
        let owner_kind = match row.owner_kind
        {
            wire::OWNER_SYSTEM => OwnerKind::System,
            wire::OWNER_PROCESS => OwnerKind::Process,
            wire::OWNER_MODULE => OwnerKind::Module,
            _ => return Err(failure(Error::BadAbi, &call))
        };
        let source = match row.source {
            wire::SOURCE_RESIDENT if row.bundle == [0; 16] && row.resource_kind == [0; 4] && row.resource_id == 0 => RegistrationSource::Resident,
            wire::SOURCE_BUNDLE_RESOURCE => RegistrationSource::BundleResource { bundle: row.bundle,
                resource: mac_traps::types::ResourceKey::from_bytes(row.resource_kind, row.resource_id) },
            _ => return Err(failure(Error::BadAbi, &call))
        };
        let label =
            InlineText::from_reply(row.label, row.label_length).map_err(|e| failure(e, &call))?;
        let provider = InlineText::from_reply(row.provider, row.provider_length).map_err(|e| {
                                                                                    failure(e,
                                                                                            &call)
                                                                                })?;
        let reason =
            InlineText::from_reply(row.reason, row.reason_length).map_err(|e| failure(e, &call))?;
        let capacity =
            decode_capacity(row.policy, row.limit, reason).map_err(|e| failure(e, &call))?;
        Ok(ObjectKind { id: row.id,
                        namespace: row.namespace,
                        release_owner: row.owner,
                        owner_kind,
                        label,
                        provider,
                        source,
                        capacity,
                        admitting: row.admitting != 0,
                        uses: row.uses })
    }
}

fn decode_capacity(policy: u32, limit: u64, reason: InlineText<{ wire::DETAIL_BYTES }>)
                   -> core::result::Result<CapacityPolicy, Error>
{
    match (policy, limit, reason.as_str().is_empty())
    {
        (wire::CAPACITY_GROWABLE, 0, true) => Ok(CapacityPolicy::GrowableUntilMemory),
        (wire::CAPACITY_QUOTA, slots, true) => Ok(CapacityPolicy::PerDomainQuota { slots }),
        (wire::CAPACITY_BOUND, slots, false) =>
        {
            Ok(CapacityPolicy::HardBound { slots,
                                           reason })
        }
        _ => Err(Error::BadAbi)
    }
}
/// Exact membership epochs for the process and module directories. Equality
/// compares both; neither epoch is truncated or combined through a hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MembershipRevision
{
    processes: u64,
    modules:   u64
}

/// No resident cursor or lease is allocated; abandoning a snapshot has nothing
/// to clean up. Caller authentication is repeated on every read.
#[derive(Clone, Copy)]
pub struct Snapshot<'a>
{
    client:       Client<'a>,
    scope:        u32,
    pub revision: MembershipRevision,
    pub count:    u64,
    coverage:     u64
}
impl<'a> Snapshot<'a>
{
    /// Capture a domain's current ledger revision within this membership
    /// snapshot. Any subsequent publication/invalidation/growth/cleanup rejects
    /// continuation with ScanStale; reacquire instead of mixing generations.
    pub fn ledger(self, index: u64) -> Result<LedgerSnapshot<'a>>
    {
        Ok(LedgerSnapshot { membership: self,
                            index,
                            domain: self.at(index)? })
    }

    pub fn complete_ledgers(self) -> bool
    {
        self.coverage & wire::PARTIAL_LEDGERS == 0
    }

    pub fn process_domains_only(self) -> bool
    {
        self.coverage & (wire::PROCESS_DOMAINS | wire::MODULE_DOMAINS | wire::SYSTEM_DOMAINS)
        == wire::PROCESS_DOMAINS
    }

    /// The real canonical System owner ledger is included. Its domain account
    /// can still be unavailable; mapping/foundation totals are a separate read.
    pub fn includes_system_domain(self) -> bool
    {
        self.coverage & wire::SYSTEM_DOMAINS != 0
    }

    pub fn includes_module_domains(self) -> bool
    {
        self.coverage & wire::MODULE_DOMAINS != 0
    }

    pub fn at(self, index: u64) -> Result<Domain>
    {
        let mut call = wire::Call { operation: wire::DOMAIN,
                                    scope: self.scope,
                                    revision: self.revision.processes,
                                    module_revision: self.revision.modules,
                                    index,
                                    ..Default::default() };
        self.client.inspection(&mut call).map_err(|error| failure(error, &call))?;
        let row = call.domain;
        if row.reserved != 0
        {
            return Err(failure(Error::BadAbi, &call));
        }
        let kind = match (row.kind, row.incarnation)
        {
            (wire::SYSTEM, 0) => OwnerKind::System,
            (wire::PROCESS, 1..) => OwnerKind::Process,
            (wire::MODULE, 1..) => OwnerKind::Module,
            _ => return Err(failure(Error::BadAbi, &call))
        };
        Ok(Domain { kind,
                    incarnation: row.incarnation,
                    accepting: row.flags & wire::ACCEPTING != 0,
                    foreground: row.flags & wire::FOREGROUND != 0,
                    ledger_revision: row.ledger_revision,
                    kinds: row.kinds,
                    live: row.live,
                    retired: row.retired,
                    pending: row.pending,
                    quarantined: row.quarantined,
                    completion_faults: row.completion_faults,
                    scoped_children: row.scoped_children,
                    slots: row.slots,
                    births: row.births,
                    deaths: row.deaths,
                    counters_saturated: row.flags & wire::COUNTERS_SATURATED != 0,
                    accounting: accounting(row.account).ok_or_else(|| {
                                                           failure(Error::BadAbi, &call)
                                                       })?,
                    name: InlineText::from_reply(row.name, row.name_length).map_err(|e| {
                                                                               failure(e, &call)
                                                                           })? })
    }
}

/// Why the System retains a cleanup ticket and its provider pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionCause
{
    Interrupted,
    AwaitingTicket,
    Refused
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompletionFault
{
    pub owner:            u64,
    pub kind:             [u8; 4],
    pub slot:             u64,
    pub generation:       u64,
    pub registration:     Option<u64>,
    pub cause:            CompletionCause,
    pub detail:           InlineText<{ wire::FAULT_DETAIL_BYTES }>,
    pub detail_truncated: bool
}
impl CompletionFault
{
    /// Maximum UTF-8 diagnostic payload; callers can bound a complete view
    /// without depending on the transport record layout.
    pub const MAX_DETAIL_BYTES: usize = wire::FAULT_DETAIL_BYTES;
}
impl core::fmt::Display for CompletionFault
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        write!(f,
               "owner #{} slot {} generation {}: {}{}",
               self.owner,
               self.slot,
               self.generation,
               self.detail.as_str(),
               if self.detail_truncated { " [detail truncated]" } else { "" })
    }
}

/// No resident allocation/cursor and no authority stored in this value.
/// Admission and both membership/ledger generations are checked on every read.
#[derive(Clone, Copy)]
pub struct LedgerSnapshot<'a>
{
    membership: Snapshot<'a>,
    index:      u64,
    domain:     Domain
}
impl LedgerSnapshot<'_>
{
    pub fn domain(self) -> Domain
    {
        self.domain
    }

    pub fn len(self) -> u64
    {
        self.domain.kinds
    }

    pub fn is_empty(self) -> bool
    {
        self.len() == 0
    }

    /// Number of retained completion failures in this exact ledger revision.
    /// These are already included in pending resources, not additional objects.
    pub fn completion_fault_count(self) -> u64
    {
        self.domain.completion_faults
    }

    pub fn completion_fault_at(self, index: u64) -> Result<CompletionFault>
    {
        let mut call = wire::Call { operation: wire::COMPLETION_FAULT,
                                    scope: self.membership.scope,
                                    revision: self.membership.revision.processes,
                                    module_revision: self.membership.revision.modules,
                                    index: self.index,
                                    ledger_revision: self.domain.ledger_revision,
                                    subject: self.domain.incarnation,
                                    pool_index: index,
                                    ..Default::default() };
        self.membership.client.inspection(&mut call).map_err(|e| failure(e, &call))?;
        let row = call.fault;
        if row.owner != self.domain.incarnation
           || row.generation == 0
           || row.flags & !wire::DETAIL_TRUNCATED != 0
        {
            return Err(failure(Error::BadAbi, &call));
        }
        let cause = match row.cause
        {
            wire::COMPLETION_INTERRUPTED => CompletionCause::Interrupted,
            wire::COMPLETION_AWAITING_TICKET => CompletionCause::AwaitingTicket,
            wire::COMPLETION_REFUSED => CompletionCause::Refused,
            _ => return Err(failure(Error::BadAbi, &call))
        };
        Ok(CompletionFault { owner:row.owner,kind:row.kind,slot:row.slot,generation:row.generation,
            registration:(row.registration!=0).then_some(row.registration),cause,
            detail:InlineText::from_reply(row.detail,row.detail_length).map_err(|e|failure(e,&call))?,
            detail_truncated:row.flags & wire::DETAIL_TRUNCATED!=0 })
    }

    pub fn at(self, index: u64) -> Result<Pool>
    {
        let mut call = wire::Call { operation: wire::LEDGER_POOL,
                                    scope: self.membership.scope,
                                    revision: self.membership.revision.processes,
                                    module_revision: self.membership.revision.modules,
                                    index: self.index,
                                    ledger_revision: self.domain.ledger_revision,
                                    subject: self.domain.incarnation,
                                    pool_index: index,
                                    ..Default::default() };
        self.membership.client.inspection(&mut call).map_err(|e| failure(e, &call))?;
        let row = call.pool;
        if row.flags & !wire::COUNTERS_SATURATED != 0
        {
            return Err(failure(Error::BadAbi, &call));
        }
        let reason =
            InlineText::from_reply(row.reason, row.reason_length).map_err(|e| failure(e, &call))?;
        let capacity =
            decode_capacity(row.policy, row.limit, reason).map_err(|e| failure(e, &call))?;
        Ok(Pool { registration: (row.registration != 0).then_some(row.registration),
                  kind: row.kind,
                  capacity,
                  live: row.live,
                  retired: row.retired,
                  pending: row.pending,
                  quarantined: row.quarantined,
                  completion_faults: row.completion_faults,
                  scoped_children: row.scoped_children,
                  slots: row.slots,
                  births: row.births,
                  deaths: row.deaths,
                  generation_headroom: row.generation_headroom,
                  counters_saturated: row.flags & wire::COUNTERS_SATURATED != 0 })
    }
}

#[cfg(test)]
mod account_tests
{
    use super::*;
    #[test]
    fn accounting_wire_rejects_unknown_coverage_bad_flags_overflow_and_hidden_counters()
    {
        assert_eq!(accounting(wire::Account::default()), Some(MemoryAccounting::Unavailable));
        let mut value = wire::Account { coverage: wire::ACCOUNT_EXECUTION_OWNED_MEMORY,
                                        ..Default::default() };
        value.kernel.limit = 4096;
        value.kernel.committed = 4096;
        assert!(matches!(accounting(value),
                         Some(MemoryAccounting::ExecutionAndOwnedMemory { .. })));
        value.closing = 2;
        assert_eq!(accounting(value), None);
        value.closing = 0;
        value.kernel.reserved = 1;
        assert_eq!(accounting(value), None);
        value.kernel.limit = u64::MAX;
        value.kernel.committed = u64::MAX;
        assert_eq!(accounting(value), None);
        value.kernel.reserved = 0;
        value.coverage = wire::ACCOUNT_UNAVAILABLE;
        assert_eq!(accounting(value), None);
        value = wire::Account { coverage: u32::MAX,
                                ..Default::default() };
        assert_eq!(accounting(value), None);
    }
}

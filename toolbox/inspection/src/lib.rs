//! Capability-checked snapshots of resident lifetime domains.
//! Membership is revisioned; each row is an atomic ledger read with its own
//! revision. Rows do not claim a simultaneous snapshot of unrelated domains.
#![no_std]
#![forbid(unsafe_code)]
use mac_abi::inspection as wire;
use mac_traps::{Client, Error, types::InlineText};
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

#[derive(Clone, Copy, Debug)]
pub struct Domain
{
    pub incarnation:        u64,
    pub accepting:          bool,
    pub foreground:         bool,
    pub ledger_revision:    u64,
    pub kinds:              u64,
    /// Migrated resources only. Consult Snapshot::complete_ledgers first.
    pub live:               u64,
    pub retired:            u64,
    pub pending:            u64,
    pub slots:              u64,
    pub births:             u64,
    pub deaths:             u64,
    pub counters_saturated: bool,
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
    pub kind:                [u8; 4],
    pub capacity:            CapacityPolicy,
    pub live:                u64,
    pub retired:             u64,
    pub pending:             u64,
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
                      revision: call.revision,
                      count:    call.count,
                      coverage: call.coverage })
    }
}
/// No resident cursor or lease is allocated; abandoning a snapshot has nothing
/// to clean up. Caller authentication is repeated on every read.
#[derive(Clone, Copy)]
pub struct Snapshot<'a>
{
    client:       Client<'a>,
    scope:        u32,
    pub revision: u64,
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
        self.coverage & wire::PROCESS_DOMAINS != 0
    }

    pub fn at(self, index: u64) -> Result<Domain>
    {
        let mut call = wire::Call { operation: wire::DOMAIN,
                                    scope: self.scope,
                                    revision: self.revision,
                                    index,
                                    ..Default::default() };
        self.client.inspection(&mut call).map_err(|error| failure(error, &call))?;
        let row = call.domain;
        if row.kind != wire::PROCESS || row.reserved != 0
        {
            return Err(failure(Error::BadAbi, &call));
        }
        Ok(Domain { incarnation:        row.incarnation,
                    accepting:          row.flags & wire::ACCEPTING != 0,
                    foreground:         row.flags & wire::FOREGROUND != 0,
                    ledger_revision:    row.ledger_revision,
                    kinds:              row.kinds,
                    live:               row.live,
                    retired:            row.retired,
                    pending:            row.pending,
                    slots:              row.slots,
                    births:             row.births,
                    deaths:             row.deaths,
                    counters_saturated: row.flags & wire::COUNTERS_SATURATED != 0,
                    name:
                        InlineText::from_reply(row.name, row.name_length).map_err(|e| {
                                                                             failure(e, &call)
                                                                         })? })
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

    pub fn at(self, index: u64) -> Result<Pool>
    {
        let mut call = wire::Call { operation: wire::LEDGER_POOL,
                                    scope: self.membership.scope,
                                    revision: self.membership.revision,
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
        let capacity = match (row.policy, row.limit, reason.as_str().is_empty())
        {
            (wire::CAPACITY_GROWABLE, 0, true) => CapacityPolicy::GrowableUntilMemory,
            (wire::CAPACITY_QUOTA, slots, true) => CapacityPolicy::PerDomainQuota { slots },
            (wire::CAPACITY_BOUND, slots, false) =>
            {
                CapacityPolicy::HardBound { slots,
                                            reason }
            }
            _ => return Err(failure(Error::BadAbi, &call))
        };
        Ok(Pool { kind: row.kind,
                  capacity,
                  live: row.live,
                  retired: row.retired,
                  pending: row.pending,
                  slots: row.slots,
                  births: row.births,
                  deaths: row.deaths,
                  generation_headroom: row.generation_headroom,
                  counters_saturated: row.flags & wire::COUNTERS_SATURATED != 0 })
    }
}

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
impl Snapshot<'_>
{
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

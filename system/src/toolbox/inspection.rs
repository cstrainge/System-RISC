//! Read-only inspection of authoritative ledgers. No resident snapshot objects,
//! object-value scans, formatting allocations, or caller-selected identity.
#![forbid(unsafe_code)]
pub(crate) mod memory;
use mac_abi::{inspection::*, *};
use mac_object_table::{domain::{CapacityPolicy, Directory, DomainKind, LedgerReadError},
                       shared::InvocationGate};

/// Implemented by trusted resident process metadata, never by an ABI caller.
pub(crate) trait Subject
{
    fn label(&self) -> &str;
    fn foreground(&self) -> bool;
    fn authority(&self) -> u64;
    /// Only actually adopted execution-backing accounting is exposed. Other
    /// managers/modules remain unavailable instead of reporting invented zeroes.
    fn execution_account(&self) -> Option<mac_kernel_model::account::Snapshot>
    {
        None
    }
}

fn reject(call: &mut Call, status: Status, message: &str) -> Status
{
    // All messages here are bounded constants. Do not truncate a UTF-8 scalar.
    call.detail_length = message.len() as u32;
    call.detail[..message.len()].copy_from_slice(message.as_bytes());
    status
}

/// Registry existence is public. Inspector authority is deliberately absent:
/// the host authenticates only that this is a live application context.
pub(crate) fn registered_kinds<C: ?Sized + 'static,
                               G: InvocationGate,
                               F: mac_object_table::registry::CleanupFailure + 'static>(
    registry: &mac_object_table::registry::Registry<C, G, F>, caller_live: bool, call: &mut Call)
    -> Status
{
    use mac_object_table::registry::{Error, Source};
    call.detail_length = 0;
    call.detail.fill(0);
    call.kind = RegisteredKind::default();
    call.count = 0;
    call.coverage = 0;
    if !caller_live
    {
        return reject(call, ACCESS_DENIED,
                      "kind registration queries require a live application context");
    }
    if call.reserved != 0 || ![SELF, ALL].contains(&call.scope)
    {
        return reject(call, BAD_ARGUMENT, "invalid kind query scope or reserved field");
    }
    let row = match call.operation
    {
        KIND_BEGIN =>
        {
            match registry.snapshot()
            {
                Ok((revision, count)) =>
                {
                    call.revision = revision;
                    call.count = count as u64;
                    return OK;
                }
                Err(error) => Err(error)
            }
        }
        KIND =>
        {
            match usize::try_from(call.index)
            {
                Ok(index) => registry.at(call.revision, index),
                Err(_) =>
                {
                    return reject(call, NOT_FOUND, "kind index is outside this registry snapshot");
                }
            }
        }
        KIND_ID => registry.by_id(call.revision, call.subject),
        _ => return reject(call, BAD_ARGUMENT, "unknown kind registration query")
    };
    let row = match row
    {
        Ok(row) => row,
        Err(Error::SnapshotStale { .. } | Error::Stale { .. } | Error::Missing { .. }) =>
        {
            return reject(call, SCAN_STALE,
                          "kind registrations changed; obtain a new registry snapshot and retry");
        }
        Err(Error::IndexNotFound { .. }) =>
        {
            return reject(call, NOT_FOUND, "kind index is outside this registry snapshot");
        }
        Err(Error::RegistrationNotFound { .. }) =>
        {
            return reject(call,
                          NOT_FOUND,
                          "registration identity is no longer live; resolve the current \
                           provider before retrying");
        }
        Err(Error::RevisionExhausted) =>
        {
            return reject(call,
                          NOT_SUPPORTED,
                          "kind registry revision exhausted; restart System to obtain new \
                           snapshots");
        }
        Err(error) =>
        {
            return reject(call,
                          super::handles::registry_status(&error),
                          "kind registry could not answer this request");
        }
    };
    let (policy, limit, reason) = match row.registration.capacity
    {
        CapacityPolicy::GrowableUntilMemory => (CAPACITY_GROWABLE, 0, ""),
        CapacityPolicy::PerDomainQuota { slots } => (CAPACITY_QUOTA, slots as u64, ""),
        CapacityPolicy::HardBound { slots,
                                    reason } => (CAPACITY_BOUND, slots as u64, reason)
    };
    if reason.len() > call.kind.reason.len()
    {
        return reject(call, CORRUPT_DATA,
                      "kind registration capacity reason exceeds the inspection wire limit");
    }
    let owner = row.registration.release.registration.owner();
    call.kind.id = row.id;
    call.kind.namespace = row.registration.code.0;
    call.kind.owner = owner.incarnation();
    call.kind.owner_kind = match owner.kind()
    {
        DomainKind::System => OWNER_SYSTEM,
        DomainKind::Process => OWNER_PROCESS,
        DomainKind::Module => OWNER_MODULE
    };
    call.kind.policy = policy;
    call.kind.limit = limit;
    call.kind.admitting = u32::from(row.admitting);
    call.kind.uses = row.uses;
    let label = row.label.as_str();
    call.kind.label[..label.len()].copy_from_slice(label.as_bytes());
    call.kind.label_length = label.len() as u32;
    let provider = row.provider.as_str();
    call.kind.provider[..provider.len()].copy_from_slice(provider.as_bytes());
    call.kind.provider_length = provider.len() as u32;
    call.kind.reason[..reason.len()].copy_from_slice(reason.as_bytes());
    call.kind.reason_length = reason.len() as u32;
    match row.source
    {
        Source::Resident => call.kind.source = SOURCE_RESIDENT,
        Source::BundleResource { bundle,
                                 kind,
                                 id } =>
        {
            call.kind.source = SOURCE_BUNDLE_RESOURCE;
            call.kind.bundle = bundle;
            call.kind.resource_kind = kind.0;
            call.kind.resource_id = id;
        }
    }
    OK
}

pub(crate) fn request<T: Subject, G: InvocationGate>(directory: &Directory<T, G>, caller: u64,
                                                     call: &mut Call)
                                                     -> Status
{
    request_sources(Sources::<T, T, G> { processes: directory,
                                         modules:   None },
                    caller,
                    call)
}

pub(crate) fn request_with_modules<T: Subject, M: Subject, G: InvocationGate>(processes:&Directory<T,G>,
                                                                              modules:&Directory<M,G>,
                                                                              caller: u64,
                                                                              call: &mut Call)
                                                                              -> Status
{
    request_sources(Sources { processes,
                              modules: Some(modules) },
                    caller,
                    call)
}
struct Entry<'a, G: InvocationGate>
{
    ledger:  &'a mac_object_table::shared::OwnerRef<G>,
    subject: &'a dyn Subject
}
impl<'a, G: InvocationGate> Entry<'a, G>
{
    fn owner(&self) -> mac_object_table::domain::DomainId
    {
        self.ledger.owner()
    }

    fn ledger(&self) -> &mac_object_table::shared::OwnerRef<G>
    {
        self.ledger
    }

    fn value(&self) -> &dyn Subject
    {
        self.subject
    }
}
fn entry<T: Subject, G: InvocationGate>(entry: &mac_object_table::domain::DomainEntry<T, G>)
                                        -> Entry<'_, G>
{
    Entry { ledger:  entry.ledger(),
            subject: entry.value() }
}
struct Sources<'a, T: Subject, M: Subject, G: InvocationGate>
{
    processes: &'a Directory<T, G>,
    modules:   Option<&'a Directory<M, G>>
}
impl<T: Subject, M: Subject, G: InvocationGate> Sources<'_, T, M, G>
{
    fn caller(&self, caller: u64) -> Option<Entry<'_, G>>
    {
        self.processes.get(caller).map(entry)
    }

    fn revision(&self) -> u64
    {
        self.processes.revision()
    }

    fn module_revision(&self, scope: u32) -> u64
    {
        if scope == SELF { 0 } else { self.modules.map_or(0, Directory::revision) }
    }

    fn count(&self) -> usize
    {
        self.processes.len() + self.modules.map_or(0, Directory::len)
    }

    fn at(&self, index: usize) -> Option<Entry<'_, G>>
    {
        if index < self.processes.len()
        {
            self.processes.iter().nth(index).map(entry)
        }
        else
        {
            self.modules?.iter().nth(index - self.processes.len()).map(entry)
        }
    }
}
fn request_sources<T: Subject, M: Subject, G: InvocationGate>(source: Sources<'_, T, M, G>,
                                                              caller: u64, call: &mut Call)
                                                              -> Status
{
    call.detail_length = 0;
    call.detail.fill(0);
    call.domain = Domain::default();
    call.pool = Pool::default();
    call.fault = CompletionFault::default();
    call.count = 0;
    if call.reserved != 0 || ![SELF, ALL].contains(&call.scope)
    {
        return reject(call, BAD_ARGUMENT, "invalid inspection scope or reserved field");
    }
    let Some(subject) = source.caller(caller)
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
    if source.revision() == u64::MAX || source.module_revision(call.scope) == u64::MAX
    {
        return reject(call,
                      NOT_SUPPORTED,
                      "inspection membership revision exhausted; restart System to obtain new \
                       snapshots");
    }
    call.coverage =
        PROCESS_DOMAINS
        | PARTIAL_LEDGERS
        | if source.modules.is_some() && call.scope == ALL { MODULE_DOMAINS } else { 0 };
    match call.operation
    {
        BEGIN =>
        {
            call.revision = source.revision();
            call.module_revision = source.module_revision(call.scope);
            call.count = if call.scope == SELF { 1 } else { source.count() as u64 };
            OK
        }
        DOMAIN | LEDGER_POOL | COMPLETION_FAULT =>
        {
            if call.revision != source.revision()
               || call.module_revision != source.module_revision(call.scope)
            {
                return reject(call,
                              SCAN_STALE,
                              "lifetime-domain membership changed during inspection; obtain a \
                               new snapshot and retry");
            }
            let entry = if call.scope == SELF
            {
                (call.index == 0).then_some(subject)
            }
            else
            {
                usize::try_from(call.index).ok().and_then(|i| source.at(i))
            };
            let Some(entry) = entry
            else
            {
                return reject(call, NOT_FOUND, "domain index is outside this inspection snapshot");
            };
            if !matches!(entry.owner().kind(), DomainKind::Process | DomainKind::Module)
            {
                return reject(call,
                              NOT_SUPPORTED,
                              "this inspection source does not describe the requested lifetime \
                               domain");
            }
            if matches!(call.operation, LEDGER_POOL | COMPLETION_FAULT)
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
                if call.operation == COMPLETION_FAULT
                {
                    let fault = match entry.ledger()
                                           .completion_fault_at(call.ledger_revision, index)
                    {
                        Ok(Some(fault)) => fault,
                        Ok(None) | Err(LedgerReadError::PoolNotFound { .. }) =>
                        {
                            return reject(call,
                                          NOT_FOUND,
                                          "completion fault index is outside this ledger \
                                           snapshot");
                        }
                        Err(LedgerReadError::Stale { .. }) =>
                        {
                            return reject(call,
                                          SCAN_STALE,
                                          "domain ledger changed while reading completion \
                                           faults; obtain a new ledger snapshot and retry");
                        }
                        Err(LedgerReadError::RevisionExhausted { .. }) =>
                        {
                            return reject(call,
                                          NOT_SUPPORTED,
                                          "domain ledger revision exhausted; restart this \
                                           domain to obtain new snapshots");
                        }
                    };
                    call.fault.owner = fault.key.owner().incarnation();
                    call.fault.kind = fault.key.kind().0;
                    call.fault.slot = fault.key.slot() as u64;
                    call.fault.generation = fault.key.generation().get();
                    call.fault.registration = fault.key.registration().unwrap_or(0);
                    call.fault.cause = match fault.cause
                    {
                        mac_object_table::completion::Cause::Interrupted => COMPLETION_INTERRUPTED,
                        mac_object_table::completion::Cause::AwaitingTicket =>
                        {
                            COMPLETION_AWAITING_TICKET
                        }
                        mac_object_table::completion::Cause::Refused(_) => COMPLETION_REFUSED
                    };
                    let mut detail = FaultText { bytes:     &mut call.fault.detail,
                                                 len:       0,
                                                 truncated: false };
                    let _ = core::fmt::write(&mut detail, format_args!("{}", fault.cause));
                    call.fault.detail_length = detail.len as u32;
                    call.fault.flags = if detail.truncated { DETAIL_TRUNCATED } else { 0 };
                    return OK;
                }
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
                call.pool = Pool { registration: pool.registration.unwrap_or(0),
                                   kind: pool.kind.0,
                                   policy,
                                   limit,
                                   live: pool.live as u64,
                                   retired: pool.retired as u64,
                                   pending: pool.pending as u64,
                                   quarantined: pool.quarantined as u64,
                                   completion_faults: pool.completion_faults as u64,
                                   scoped_children: pool.scoped_children as u64,
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
                         kind: match entry.owner().kind()
                         {
                             DomainKind::Process => PROCESS,
                             DomainKind::Module => MODULE,
                             DomainKind::System => OWNER_SYSTEM
                         },
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
                         account: match entry.value().execution_account()
                         {
                             None => Account::default(),
                             Some(snapshot) =>
                             {
                                 let usage = |value: mac_kernel_model::policy::AccountUsage| {
                                     AccountUsage { limit:     value.limit().default_value(),
                                                    reserved:  value.reserved(),
                                                    committed: value.committed() }
                                 };
                                 Account { coverage:    ACCOUNT_STACK_CPU_ONLY,
                                           closing:     u32::from(snapshot.closing),
                                           kernel:      usage(snapshot.kernel),
                                           application: usage(snapshot.application) }
                             }
                         },
                         ledger_revision: ledger.revision,
                         kinds: ledger.kinds as u64,
                         live: ledger.live as u64,
                         retired: ledger.retired as u64,
                         pending: ledger.pending as u64,
                         quarantined: ledger.quarantined as u64,
                         completion_faults: ledger.completion_faults as u64,
                         scoped_children: ledger.scoped_children as u64,
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

/// Bounded diagnostics preserve UTF-8 and explicitly mark any omitted suffix.
/// No allocation or payload callback occurs while serving an inspection row.
struct FaultText<'a>
{
    bytes:     &'a mut [u8],
    len:       usize,
    truncated: bool
}
impl core::fmt::Write for FaultText<'_>
{
    fn write_str(&mut self, text: &str) -> core::fmt::Result
    {
        if self.truncated
        {
            return Ok(());
        }
        let mut length = text.len().min(self.bytes.len() - self.len);
        while !text.is_char_boundary(length)
        {
            length -= 1;
        }
        self.bytes[self.len..self.len + length].copy_from_slice(&text.as_bytes()[..length]);
        self.len += length;
        self.truncated = length != text.len();
        Ok(())
    }
}


#[cfg(target_arch = "riscv64")]
static MEMBERSHIP_CHANGED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
#[cfg(target_arch = "riscv64")]
pub(crate) fn membership_changed()
{
    MEMBERSHIP_CHANGED.store(true, core::sync::atomic::Ordering::Release);
}
#[cfg(target_arch = "riscv64")]
pub(crate) fn take_membership_change() -> bool
{
    MEMBERSHIP_CHANGED.load(core::sync::atomic::Ordering::Acquire)
    && MEMBERSHIP_CHANGED.swap(false, core::sync::atomic::Ordering::AcqRel)
}

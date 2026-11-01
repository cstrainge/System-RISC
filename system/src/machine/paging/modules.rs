//! Real executable payload backing and the owning supervisor PTE transaction.
//! Each permanent root retains its own validated branch path. Application and
//! module payloads are charged to their original Kernel account; no user-mode
//! mapping or private application address space is implied.
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{fmt,
           sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering}};

use mac_abi::{BAD_ABI, BAD_ARGUMENT, NO_MEMORY, Status};
use mac_formats::native::{Descriptor, Role,
                          image::{self, Destination}};
use mac_kernel_model::{account::{AccountUse, Committed},
                       pages::{Access, PageOwnership, Permissions, PhysicalAddress}};
use mac_riscv_pages::{Decoded, Entry, LeafSize, Sv39};
use spinning_top::Spinlock as Mutex;

use super::{BOOT, Backing, Foundation, SPACES, interrupts};
use crate::{machine::{interrupts::NativeInvocationGate, memory::*},
            toolbox::handles::OwnerRef};

static REVISION: AtomicU64 = AtomicU64::new(2);
static TABLES: Mutex<Tables> = Mutex::new(Tables { branches: None });
static PENDING: AtomicBool = AtomicBool::new(false);
static RETIRING: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
pub(crate) enum Error
{
    Missing,
    Owner,
    Closed,
    State,
    Metadata,
    Extent
    {
        offset: u64,
        bytes:  u64
    },
    DmaSpan
    {
        address: u64,
        bytes:   u64
    },
    Format(mac_formats::Error),
    Account(mac_kernel_model::account::Error),
    Pages(crate::machine::pages::Error),
    Foundation(crate::allocator::foundation::Error),
    Encoding(mac_riscv_pages::Error),
    Conflict
    {
        address:  u64,
        expected: u64,
        observed: u64
    },
    Revision,
    Monitor(crate::machine::monitor::Error)
}
impl Error
{
    pub(crate) fn status(&self) -> Status
    {
        match self
        {
            Self::Metadata => NO_MEMORY,
            Self::Account(cause) => crate::allocation::account_status(cause),
            Self::Pages(cause) => cause.status(),
            Self::Foundation(cause) => cause.status(),
            Self::Monitor(cause) => cause.status(),
            Self::Closed => mac_abi::OPERATION_CANCELLED,
            Self::Owner => mac_abi::ACCESS_DENIED,
            Self::Format(mac_formats::Error::AllocationFailed(_)
                         | mac_formats::Error::BudgetExceeded(_)) => NO_MEMORY,
            Self::Extent { .. } | Self::DmaSpan { .. } => BAD_ARGUMENT,
            _ => BAD_ABI
        }
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Missing =>
            {
                out.write_str("executable mappings require the actual permanent supervisor roots")
            }
            Self::Owner =>
            {
                out.write_str("executable backing requires its original matching owner/account")
            }
            Self::Closed =>
            {
                out.write_str("image owner stopped before executable mapping publication")
            }
            Self::State =>
            {
                out.write_str("executable mapping transition does not match its retained owner; \
                               backing retained")
            }
            Self::Metadata =>
            {
                out.write_str("executable mapping metadata allocation refused before publication")
            }
            Self::Extent { offset,
                           bytes } =>
            {
                write!(out,
                       "executable memory span {offset:#x}+{bytes:#x} is unaligned, overlapping \
                        or outside its execution reservation")
            }
            Self::DmaSpan { address,
                            bytes } =>
            {
                write!(out,
                       "DMA span {address:#x}+{bytes:#x} must fit one owned physically contiguous \
                        module segment; holes and segment boundaries are not DMA buffers")
            }
            Self::Format(cause) => cause.fmt(out),
            Self::Account(cause) => cause.fmt(out),
            Self::Pages(cause) => cause.fmt(out),
            Self::Foundation(cause) => cause.fmt(out),
            Self::Encoding(cause) => cause.fmt(out),
            Self::Conflict { address,
                             expected,
                             observed } =>
            {
                write!(out,
                       "executable PTE {address:#x} changed: expected {expected:#x}, observed \
                        {observed:#x}; original mapping/backing retained")
            }
            Self::Revision =>
            {
                out.write_str("System mapping revision exhausted before PTE effects; restart \
                               required")
            }
            Self::Monitor(cause) => cause.fmt(out)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage
{
    PublishTranslations,
    PublishInstructions,
    RetireAdmission,
    RetireTranslations
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MonitorRefusal
{
    Uncertain
    {
        hart:     u64,
        sequence: u64
    },
    Refused
    {
        hart:   u64,
        result: i64
    },
    Stopping,
    Retired,
    ForeignIssuer,
    SequenceExhausted,
    ClockExhausted,
    Missing,
    Busy,
    Protocol
    {
        status: Status
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CleanupCause
{
    Pending
    {
        base: u64
    },
    Pte
    {
        base: u64, address: u64, expected: u64, observed: u64
    },
    Revision
    {
        base: u64
    },
    State
    {
        base: u64
    },
    Monitor
    {
        base: u64, stage: Stage, cause: MonitorRefusal
    }
}
impl fmt::Display for CleanupCause
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Pending { base } =>
            {
                write!(out,
                       "executable image {base:#x} awaits actual execution/readers or translation \
                        retirement")
            }
            Self::Pte { base,
                        address,
                        expected,
                        observed } =>
            {
                write!(out,
                       "executable image {base:#x} PTE {address:#x} differs: expected \
                        {expected:#x}, observed {observed:#x}")
            }
            Self::Revision { base } =>
            {
                write!(out, "executable image {base:#x} mapping revision exhausted")
            }
            Self::State { base } =>
            {
                write!(out, "executable image {base:#x} has an uncertain owning transition")
            }
            Self::Monitor { base,
                            stage,
                            cause } =>
            {
                write!(out, "executable image {base:#x} {stage:?} failed: {cause:?}")
            }
        }?;
        out.write_str("; original backing/account/cleanup ticket retained; restart required for \
                       quarantined transitions")
    }
}

struct Segment
{
    offset:  u64,
    memory:  u64,
    backing: Backing,
    _charge: Committed<NativeInvocationGate>
}
struct Payload
{
    extent:   usize,
    segments: Vec<Segment>
}
impl Destination for Payload
{
    type Error = Error;

    fn length(&self) -> usize
    {
        self.extent
    }

    fn clear(&mut self) -> Result<(), Error>
    {
        for segment in &self.segments
        {
            // SAFETY: actual unpublished exclusive PageBlock; no execution VA
            // or borrowed source slice is used as writable storage.
            unsafe {
                core::ptr::write_bytes(segment.backing.cpu_base() as *mut u8,
                                       0,
                                       segment.backing.bytes() as usize);
            }
        }
        Ok(())
    }

    fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<(), Error>
    {
        let start = offset as u64;
        let end = start.checked_add(bytes.len() as u64)
                       .ok_or(Error::Extent { offset: start,
                                              bytes:  bytes.len() as u64 })?;
        let segment =
            self.segments
                .iter()
                .find(|segment| start >= segment.offset && end <= segment.offset + segment.memory)
                .ok_or(Error::Extent { offset: start,
                                       bytes:  bytes.len() as u64 })?;
        let address = segment.backing
                             .cpu_base()
                             .checked_add((start - segment.offset) as usize)
                             .ok_or(Error::State)?;
        // SAFETY: complete range validated against the retained unique actual
        // segment. LoadPlan validates source/relocation geometry before writes.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), address as *mut u8, bytes.len());
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct SegmentMap
{
    virtual_base: u64,
    physical:     u64,
    bytes:        u64,
    memory:       u64,
    flags:        u32
}
#[derive(Clone, Copy)]
struct Slot
{
    cpu:     usize,
    index:   usize,
    bits:    u64,
    address: u64
}
impl Slot
{
    fn load(self) -> u64
    {
        // SAFETY: this private coordinate is derived from the owning Plan or
        // the retained Mapping entries. Their branch/root leases outlive this
        // bounded access; no reference is returned beyond that access.
        unsafe {AtomicU64::from_ptr((self.cpu as *mut u64).add(self.index))}.load(Ordering::Acquire)
    }

    fn store(self, bits: u64)
    {
        // SAFETY: same actual owner as load, with the native mapping turn held.
        unsafe {AtomicU64::from_ptr((self.cpu as *mut u64).add(self.index))}.store(bits,Ordering::Release);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase
{
    Prepared,
    Preparing,
    Mapped,
    Live,
    Cleared,
    Unmapped,
    Settled,
    Quarantined(CleanupCause)
}
struct Entries
{
    leaves:    Vec<Slot>,
    ancestors: Vec<Slot>
}
struct State
{
    phase:     Phase,
    requested: bool
}
/// The original IMAG instance owns this anchor before the first PTE effect.
/// Payload removal happens only after the actual translation receipt. Drop
/// itself performs no unmap, monitor wait or fallible teardown.
pub(crate) struct Mapping
{
    owner:    OwnerRef,
    base:     u64,
    segments: Vec<SegmentMap>,
    leaves:   spin::Once<Entries>,
    payload:  Mutex<Option<Payload>>,
    state:    Mutex<State>
}
impl Mapping
{
    fn with_state<R>(&self, operation: impl FnOnce(&mut State) -> R) -> R
    {
        interrupts::with_masked(|| operation(&mut self.state.lock()))
    }

    fn phase(&self) -> Phase
    {
        self.with_state(|state| state.phase)
    }

    fn set_phase(&self, phase: Phase)
    {
        self.with_state(|state| state.phase = phase);
    }

    fn settle_phase(&self, phase: Phase)
    {
        self.with_state(|state| {
                state.phase = phase;
                if state.requested
                {
                    state.requested = false;
                    RETIRING.fetch_sub(1, Ordering::AcqRel);
                }
            });
    }

    pub(crate) fn prepare(owner: &OwnerRef, account: &AccountUse<NativeInvocationGate>,
                          descriptor: &Descriptor, bytes: &[u8], relocations: &[u8], base: u64)
                          -> Result<Arc<Self>, Error>
    {
        let module = owner.owner().kind() == mac_object_table::domain::DomainKind::Module;
        let application = owner.owner().kind() == mac_object_table::domain::DomainKind::Process;
        if owner.owner() != account.owner()
           || !(module && descriptor.role == Role::CodeResource
                || application && descriptor.role == Role::Application)
        {
            return Err(Error::Owner);
        }
        if !owner.accepting()
        {
            return Err(Error::Closed);
        }
        let extent =
            descriptor.load_limit
                      .checked_sub(descriptor.load_base)
                      .filter(|bytes| *bytes != 0)
                      .ok_or(Error::Format(mac_formats::Error::Malformed("invalid executable \
                                                                          reservation")))?;
        let end = base.checked_add(extent).ok_or(Error::Extent { offset: base,
                                                                  bytes:  extent })?;
        let region = if module
        {
            base >= VM_MODULE_START && end <= VM_MODULE_END
        }
        else
        {
            super::super::memory::application_region(base, end)
        };
        if !region || !base.is_multiple_of(PAGE_BYTES) || !extent.is_multiple_of(PAGE_BYTES)
        {
            return Err(Error::Extent { offset: base,
                                       bytes:  extent });
        }
        let plan = image::LoadPlan::new(bytes, descriptor, Some(relocations), base)
            .map_err(Error::Format)?;
        let mut payload = Payload { extent:   plan.length(),
                                    segments: Vec::new() };
        let mut segments = Vec::new();
        payload.segments.try_reserve_exact(plan.segments().len()).map_err(|_| Error::Metadata)?;
        segments.try_reserve_exact(plan.segments().len()).map_err(|_| Error::Metadata)?;
        for segment in plan.segments()
        {
            let mapped = segment.memory_length
                                .checked_add(PAGE_BYTES - 1)
                                .map(|n| n / PAGE_BYTES * PAGE_BYTES)
                                .ok_or(Error::Extent { offset: segment.offset,
                                                       bytes:  segment.memory_length })?;
            if !segment.offset.is_multiple_of(PAGE_BYTES)
               || segment.offset.checked_add(mapped).is_none_or(|end| end > extent)
               || segments.iter().any(|old: &SegmentMap| {
                                     base + segment.offset < old.virtual_base + old.bytes
                                     && old.virtual_base < base + segment.offset + mapped
                                 })
            {
                return Err(Error::Extent { offset: segment.offset,
                                           bytes:  segment.memory_length });
            }
            let planned = Backing::planned_bytes(mapped as usize).map_err(Error::Pages)?;
            let charge = account.reserve_kernel(planned).map_err(Error::Account)?;
            let backing = Backing::reserve_uninitialized(mapped as usize).map_err(Error::Pages)?;
            if backing.bytes() != planned
            {
                return Err(Error::State);
            }
            segments.push(SegmentMap { virtual_base: base + segment.offset,
                                       physical:     backing.physical().0,
                                       bytes:        mapped,
                                       memory:       segment.memory_length,
                                       flags:        segment.flags });
            payload.segments.push(Segment { offset: segment.offset,
                                            memory: segment.memory_length,
                                            backing,
                                            _charge: charge.commit() });
        }
        plan.write_to(&mut payload).map_err(|cause| {
                                        match cause
                                        {
                                            image::WriteError::Format(cause) =>
                                            {
                                                Error::Format(cause)
                                            }
                                            image::WriteError::Destination(cause) => cause
                                        }
                                    })?;
        Arc::try_new(Self { owner: owner.clone(),
                            base,
                            segments,
                            leaves: spin::Once::new(),
                            payload: Mutex::new(Some(payload)),
                            state: Mutex::new(State { phase:     Phase::Prepared,
                                                      requested: false }) }).map_err(|_| {
                                                                                Error::Metadata
                                                                            })
    }

    pub(crate) fn publish(&self) -> Result<(), Error>
    {
        if !self.owner.accepting()
        {
            return Err(Error::Closed);
        }
        self.with_state(|state| {
                if state.phase != Phase::Prepared
                {
                    return Err(Error::State);
                }
                state.phase = Phase::Preparing;
                Ok(())
            })?;
        // Actual payload allocation/initialization is already finished. The
        // real command lease also excludes a competing executable PTE publisher
        // while private branch/rollback metadata is prepared; this avoids
        // treating a legitimate concurrently installed branch as corruption.
        let lease =
            crate::machine::monitor::prepare_translation_invalidation().map_err(Error::Monitor)?;
        let plan = Plan::prepare(&self.segments)?;
        let mut leaves = Vec::new();
        leaves.try_reserve_exact(plan.leaves.len()).map_err(|_| Error::Metadata)?;
        leaves.extend_from_slice(&plan.leaves); // fallible capacity precedes effects
        let mut ancestors = Vec::new();
        ancestors.try_reserve_exact(plan.ancestors.len()).map_err(|_| Error::Metadata)?;
        ancestors.extend_from_slice(&plan.ancestors);
        self.leaves.call_once(|| {
                       Entries { leaves,
                                 ancestors }
                   });
        if !self.owner.accepting()
        {
            return Err(Error::Closed);
        }
        plan.commit()?;
        self.set_phase(Phase::Mapped);
        match lease.finish()
        {
            Ok(receipt) => drop(receipt),
            Err(cause) =>
            {
                self.quarantine(Stage::PublishTranslations, &cause);
                return Err(Error::Monitor(cause));
            }
        }
        if let Err(cause) = crate::machine::monitor::publish_instructions()
        {
            if !matches!(cause, crate::machine::monitor::Error::Busy)
            {
                self.quarantine(Stage::PublishInstructions, &cause);
            }
            return Err(Error::Monitor(cause));
        }
        self.set_phase(Phase::Live);
        Ok(())
    }

    pub(crate) fn request_retirement(&self)
    {
        self.with_state(|state| {
                if !state.requested
                   && !matches!(state.phase, Phase::Settled | Phase::Quarantined(_))
                {
                    state.requested = true;
                    RETIRING.fetch_add(1, Ordering::AcqRel);
                    PENDING.store(true, Ordering::Release);
                    crate::machine::scheduler::wake::request();
                }
            });
    }

    pub(crate) fn settled(&self) -> bool
    {
        self.phase() == Phase::Settled
    }

    pub(crate) fn ready(&self) -> bool
    {
        self.with_state(|state| {
                state.requested && !matches!(state.phase, Phase::Settled | Phase::Quarantined(_))
            })
    }

    pub(crate) fn refusal(&self) -> CleanupCause
    {
        match self.phase()
        {
            Phase::Quarantined(cause) => cause,
            _ => CleanupCause::Pending { base: self.base }
        }
    }

    pub(crate) fn retire(&self) -> Result<(), Error>
    {
        let phase = self.phase();
        match phase
        {
            Phase::Settled => return Ok(()),
            Phase::Quarantined(_) | Phase::Cleared => return Err(Error::State),
            Phase::Prepared | Phase::Preparing | Phase::Unmapped =>
            {}
            Phase::Mapped | Phase::Live =>
            {
                let lease = match crate::machine::monitor::prepare_translation_invalidation()
                {
                    Ok(lease) => lease,
                    Err(cause) =>
                    {
                        if !matches!(cause, crate::machine::monitor::Error::Busy)
                        {
                            self.quarantine(Stage::RetireAdmission, &cause);
                        }
                        return Err(Error::Monitor(cause));
                    }
                };
                let leaves = self.leaves.get().ok_or(Error::State)?;
                if let Err(cause) = unmap(leaves)
                {
                    let refusal = match &cause
                    {
                        Error::Conflict { address,
                                          expected,
                                          observed } =>
                        {
                            CleanupCause::Pte { base:     self.base,
                                                address:  *address,
                                                expected: *expected,
                                                observed: *observed }
                        }
                        Error::Revision => CleanupCause::Revision { base: self.base },
                        _ => CleanupCause::State { base: self.base }
                    };
                    self.settle_phase(Phase::Quarantined(refusal));
                    return Err(cause);
                }
                self.set_phase(Phase::Cleared);
                match lease.finish()
                {
                    Ok(receipt) => self.confirm_unmapped(receipt),
                    Err(cause) =>
                    {
                        self.quarantine(Stage::RetireTranslations, &cause);
                        return Err(Error::Monitor(cause));
                    }
                }
            }
        }
        let payload = interrupts::with_masked(|| self.payload.lock().take());
        drop(payload); // actual PageBlocks before original Committed refunds
        self.settle_phase(Phase::Settled);
        crate::drivers::recovery_changed();
        crate::toolbox::processes::retirement::resources_released();
        Ok(())
    }

    fn confirm_unmapped(&self, _receipt: crate::machine::monitor::TranslationsInvalidated)
    {
        self.set_phase(Phase::Unmapped);
    }

    fn quarantine(&self, stage: Stage, error: &crate::machine::monitor::Error)
    {
        use crate::machine::monitor::Error as E;
        let cause = match error
        {
            E::Uncertain { hart,
                           sequence } =>
            {
                MonitorRefusal::Uncertain { hart:     *hart,
                                            sequence: *sequence }
            }
            E::Refused { hart,
                         status } =>
            {
                MonitorRefusal::Refused { hart:   *hart,
                                          result: *status }
            }
            E::Stopping => MonitorRefusal::Stopping,
            E::TransportRetired => MonitorRefusal::Retired,
            E::ForeignHart => MonitorRefusal::ForeignIssuer,
            E::SequenceExhausted => MonitorRefusal::SequenceExhausted,
            E::ClockOverflow => MonitorRefusal::ClockExhausted,
            E::Missing => MonitorRefusal::Missing,
            E::Busy => MonitorRefusal::Busy,
            E::Memory(_)
            | E::Directory(_)
            | E::Physical(_)
            | E::AlreadyAdmitted
            | E::Layout
            | E::Image
            | E::Entry(_)
            | E::StartBinding { .. }
            | E::StartRefused { .. }
            | E::WakeBinding { .. }
            | E::WakeOrder { .. }
            | E::Wake(_)
            | E::State { .. }
            | E::StopUnacknowledged { .. } => MonitorRefusal::Protocol { status: error.status() }
        };
        self.settle_phase(Phase::Quarantined(CleanupCause::Monitor { base: self.base,
                                                                     stage,
                                                                     cause }));
    }

    pub(crate) fn dma_address(&self, cpu: u64, bytes: u64) -> Result<u64, Error>
    {
        if !matches!(self.phase(), Phase::Mapped | Phase::Live)
        {
            return Err(Error::Closed);
        }
        let invalid = || {
            Error::DmaSpan { address: cpu,
                             bytes }
        };
        if bytes == 0
        {
            return Err(invalid());
        }
        let end = cpu.checked_add(bytes).ok_or_else(invalid)?;
        let segment = self.segments
                          .iter()
                          .find(|segment| {
                              cpu >= segment.virtual_base
                              && end <= segment.virtual_base + segment.memory
                          })
                          .ok_or_else(invalid)?;
        let entries = self.leaves.get().ok_or(Error::State)?;
        let lower = cpu & !(PAGE_BYTES - 1);
        let first = entries.leaves.partition_point(|leaf| leaf.address < lower);
        interrupts::with_masked(|| {
            let _tables = TABLES.lock();
            for entry in &entries.ancestors
            {
                let observed = entry.load();
                if observed != entry.bits
                {
                    return Err(Error::Conflict { address: entry.address,
                                                 expected: entry.bits,
                                                 observed });
                }
            }
            let mut next = lower;
            let selected = &entries.leaves[first..];
            let mut entries = selected.iter().take_while(|entry| entry.address < end);
            while let Some(first) = entries.next()
            {
                let second = entries.next().ok_or(Error::State)?;
                if first.address != next || second.address != next
                {
                    return Err(Error::State);
                }
                for entry in [first, second]
                {
                    let observed = entry.load();
                    if observed != entry.bits
                    {
                        return Err(Error::Conflict { address: entry.address,
                                                     expected: entry.bits,
                                                     observed });
                    }
                }
                next += PAGE_BYTES;
            }
            if next < end
            {
                return Err(Error::State);
            }
            segment.physical.checked_add(cpu - segment.virtual_base).ok_or(Error::State)
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Key
{
    root:   u64,
    level:  u8,
    prefix: u64
}
#[derive(Clone, Copy)]
struct Table
{
    cpu:      usize,
    physical: PhysicalAddress
}
struct Branch
{
    key:   Key,
    table: Foundation<Backing>,
    next:  Option<Box<Branch>>
}
impl Branch
{
    fn reference(&self) -> Table
    {
        Table { cpu:      self.table.cpu_base(),
                physical: self.table.physical() }
    }
}
struct Tables
{
    branches: Option<Box<Branch>>
}
impl Tables
{
    fn find(&self, key: Key) -> Option<Table>
    {
        let mut next = self.branches.as_deref();
        while let Some(branch) = next
        {
            if branch.key == key
            {
                return Some(branch.reference());
            }
            next = branch.next.as_deref();
        }
        None
    }
}
struct Plan
{
    branches:  Vec<Box<Branch>>,
    edits:     Vec<Slot>,
    leaves:    Vec<Slot>,
    checks:    Vec<Slot>,
    ancestors: Vec<Slot>
}
impl Plan
{
    fn prepare(segments: &[SegmentMap]) -> Result<Self, Error>
    {
        let mut plan = Self { branches:  Vec::new(),
                              edits:     Vec::new(),
                              leaves:    Vec::new(),
                              checks:    Vec::new(),
                              ancestors: Vec::new() };
        let spaces = SPACES.get().ok_or(Error::Missing)?;
        let boot = BOOT.get().ok_or(Error::Missing)?;
        let pages = segments.iter()
                            .try_fold(0usize, |n, s| n.checked_add((s.bytes / PAGE_BYTES) as usize))
                            .and_then(|n| n.checked_mul(2))
                            .ok_or(Error::Metadata)?;
        plan.leaves.try_reserve_exact(pages).map_err(|_| Error::Metadata)?;
        plan.edits.try_reserve_exact(pages).map_err(|_| Error::Metadata)?;
        for space in [&spaces.kernel, &spaces.firmware]
        {
            let root = space.owner.root_physical();
            let root_table = Table { cpu:      boot.ram
                                                   .virtual_address(root, PAGE_BYTES)
                                                   .map_err(|_| Error::Missing)?
                                                   .0
                                               as usize,
                                     physical: root };
            let mut l1_cache: Option<(u64, Table)> = None;
            let mut l0_cache: Option<(u64, Table)> = None;
            for segment in segments
            {
                let mut offset = 0;
                while offset < segment.bytes
                {
                    let address = segment.virtual_base + offset;
                    let l1_prefix = address & !(LeafSize::Huge.bytes() - 1);
                    let l1 = match l1_cache
                    {
                        Some((prefix, table)) if prefix == l1_prefix => table,
                        _ =>
                        {
                            let table = plan.table(Key { root:   root.0,
                                                         level:  1,
                                                         prefix: l1_prefix },
                                                   root_table,
                                                   address,
                                                   space)?;
                            l1_cache = Some((l1_prefix, table));
                            table
                        }
                    };
                    let l0_prefix = address & !(LeafSize::Large.bytes() - 1);
                    let l0 = match l0_cache
                    {
                        Some((prefix, table)) if prefix == l0_prefix => table,
                        _ =>
                        {
                            let table = plan.table(Key { root:   root.0,
                                                         level:  0,
                                                         prefix: l0_prefix },
                                                   l1,
                                                   address,
                                                   space)?;
                            l0_cache = Some((l0_prefix, table));
                            table
                        }
                    };
                    let permissions = Permissions { read:    segment.flags & image::READ != 0,
                                                    write:   segment.flags & image::WRITE != 0,
                                                    execute: segment.flags & image::EXECUTE != 0,
                                                    access:  Access::Supervisor,
                                                    global:  false };
                    let bits = Entry::leaf(PhysicalAddress(segment.physical + offset),
                                           LeafSize::Page,
                                           permissions,
                                           PageOwnership::Manual).map_err(Error::Encoding)?
                                                                 .bits();
                    let slot = Slot { cpu: l0.cpu,
                                      index: Sv39::index(address, 0).map_err(Error::Encoding)?,
                                      bits,
                                      address };
                    plan.leaves.push(slot);
                    plan.edits.push(slot);
                    offset += PAGE_BYTES;
                }
            }
        }
        plan.leaves.sort_unstable_by_key(|leaf| leaf.address);
        Ok(plan)
    }

    fn table(&mut self, key: Key, parent: Table, address: u64, space: &super::Space)
             -> Result<Table, Error>
    {
        if let Some(table) =
            self.branches.iter().find(|branch| branch.key == key).map(|branch| branch.reference())
        {
            return Ok(table);
        }
        let index = Sv39::index(address, key.level + 1).map_err(Error::Encoding)?;
        let mut slot = Slot { cpu: parent.cpu,
                              index,
                              bits: 0,
                              address };
        let observed = slot.load();
        self.ancestors.try_reserve(1).map_err(|_| Error::Metadata)?;
        if observed != 0
        {
            // Existing bootstrap branches can cover neighboring System/firmware
            // addresses. Reuse only a real retained table, never overwrite a
            // large leaf or manufacture ownership from an arbitrary PPN.
            let Decoded::Table { physical,
                                 global: false,
                                 ownership } =
                Entry::decode_raw(observed, key.level + 1).map_err(Error::Encoding)?
            else
            {
                return Err(Error::Conflict { address,
                                             expected: 0,
                                             observed });
            };
            let cpu = interrupts::with_masked(|| {
                          let managed = TABLES.lock().find(key);
                          match managed
                          {
                              Some(table)
                                  if table.physical == physical
                                     && ownership == PageOwnership::Manual =>
                              {
                                  Some(table.cpu)
                              }
                              Some(_) => None,
                              None if ownership == PageOwnership::Automatic =>
                              {
                                  space.tables.node(physical).ok().map(|node| node.cpu)
                              }
                              None => None
                          }
                      }).ok_or(Error::Conflict { address,
                                                 expected: 0,
                                                 observed })?;
            self.checks.try_reserve(1).map_err(|_| Error::Metadata)?;
            slot.bits = observed;
            self.ancestors.push(slot);
            self.checks.push(slot);
            return Ok(Table { cpu,
                              physical });
        }
        // A managed table with an absent parent is not silently reattached.
        // Its missing mapping is a conflict requiring the original owner.
        if interrupts::with_masked(|| TABLES.lock().find(key).is_some())
        {
            return Err(Error::Conflict { address,
                                         expected: 1,
                                         observed });
        }
        let branch =
            crate::allocator::with_system(crate::allocator::SystemClass::Foundation, || {
                let table = Foundation::<Backing>::allocate(PAGE_BYTES as usize)
                .map_err(Error::Foundation)?;
                Box::try_new(Branch { key,
                                      table,
                                      next: None }).map_err(|_| Error::Metadata)
            })?;
        self.branches.try_reserve(1).map_err(|_| Error::Metadata)?;
        self.edits.try_reserve(1).map_err(|_| Error::Metadata)?;
        let table = branch.reference();
        slot.bits =
            Entry::table(table.physical, false, PageOwnership::Manual).map_err(Error::Encoding)?
                                                                      .bits();
        self.ancestors.push(slot);
        self.edits.push(slot);
        self.branches.push(branch);
        Ok(table)
    }

    fn commit(mut self) -> Result<(), Error>
    {
        interrupts::with_masked(|| {
            let mut tables = TABLES.lock();
            for check in &self.checks
            {
                let observed = check.load();
                if observed != check.bits
                {
                    return Err(Error::Conflict { address: check.address,
                                                 expected: check.bits,
                                                 observed });
                }
            }
            for edit in &self.edits
            {
                let observed = edit.load();
                if observed != 0
                {
                    return Err(Error::Conflict { address: edit.address,
                                                 expected: 0,
                                                 observed });
                }
            }
            let revision = REVISION.load(Ordering::Acquire);
            let next =
                revision.checked_add(2).filter(|_| revision & 1 == 0).ok_or(Error::Revision)?;
            while let Some(mut branch) = self.branches.pop()
            {
                branch.next = tables.branches.take();
                tables.branches = Some(branch);
            }
            REVISION.store(revision + 1, Ordering::Release);
            for edit in &self.edits
            {
                edit.store(edit.bits);
            }
            REVISION.store(next, Ordering::Release);
            Ok(())
        })
    }
}
fn unmap(entries: &Entries) -> Result<(), Error>
{
    interrupts::with_masked(|| {
        let _tables = TABLES.lock();
        for leaf in entries.ancestors.iter().chain(entries.leaves.iter())
        {
            let observed = leaf.load();
            if observed != leaf.bits
            {
                return Err(Error::Conflict { address: leaf.address,
                                             expected: leaf.bits,
                                             observed });
            }
        }
        let revision = REVISION.load(Ordering::Acquire);
        let next = revision.checked_add(2).filter(|_| revision & 1 == 0).ok_or(Error::Revision)?;
        REVISION.store(revision + 1, Ordering::Release);
        for leaf in &entries.leaves
        {
            leaf.store(0);
        }
        REVISION.store(next, Ordering::Release);
        Ok(())
    })
}
/// Resolve only a real retained branch table, never an untrusted physical PPN.
pub(super) fn table_cpu(physical: PhysicalAddress) -> Option<usize>
{
    interrupts::with_masked(|| {
        let tables = TABLES.lock();
        let mut next = tables.branches.as_deref();
        while let Some(branch) = next
        {
            if branch.table.physical() == physical
            {
                return Some(branch.table.cpu_base());
            }
            next = branch.next.as_deref();
        }
        None
    })
}
pub(crate) fn revision() -> Option<u64>
{
    let revision = REVISION.load(Ordering::Acquire);
    (revision & 1 == 0).then_some(revision)
}
pub(crate) fn monitor_released()
{
    if RETIRING.load(Ordering::Acquire) != 0
    {
        PENDING.store(true, Ordering::Release);
        crate::machine::scheduler::wake::request();
    }
}
pub(crate) fn changed()
{
    monitor_released();
}
pub(crate) fn take_pending() -> bool
{
    PENDING.swap(false, Ordering::AcqRel)
}

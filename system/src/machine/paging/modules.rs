//! Real executable backing and the single owning native PTE transaction.
//! Trusted modules/definitions publish Supervisor leaves; applications and
//! Process definitions publish User leaves only in their owning Process root.
//! Every payload keeps its original physical owner and Kernel receipt.
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{alloc::Layout,
           fmt,
           sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering}};

use mac_abi::{BAD_ABI, BAD_ARGUMENT, NO_MEMORY, Status};
use mac_formats::native::{Descriptor, Role,
                          image::{self, Destination}};
use mac_kernel_model::{account::{AccountUse, Committed},
                       pages::{Access, PageBacking, PageOwnership, Permissions, PhysicalAddress}};
use mac_riscv_pages::{Decoded, Entry, LeafSize, Sv39};
use spinning_top::Spinlock as Mutex;

use super::{BOOT, Backing, BackingSource, Foundation, PagingBacking, SPACES, interrupts};
use crate::{machine::{interrupts::NativeInvocationGate, memory::*},
            toolbox::handles::OwnerRef};

static REVISION: AtomicU64 = AtomicU64::new(2);
static TABLES: Mutex<Tables> = Mutex::new(Tables { branches:        None,
                                                   kernel_branches: 0 });
static PENDING: AtomicBool = AtomicBool::new(false);
static RETIRING: AtomicUsize = AtomicUsize::new(0);
static WORKSPACE: Mutex<Option<RetirementWorkspace>> = Mutex::new(Some(RetirementWorkspace::new()));
static WAITING_WORKSPACE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkspaceError
{
    Busy,
    Capacity
    {
        pages: usize,
        roots: usize
    },
    Allocation
    {
        part:     &'static str,
        required: usize
    },
    Invariant
    {
        part:     &'static str,
        required: usize,
        capacity: usize
    }
}
impl WorkspaceError
{
    pub(crate) fn status(self) -> Status
    {
        match self
        {
            Self::Busy => mac_abi::SERVICE_BUSY,
            Self::Capacity { .. } | Self::Allocation { .. } => NO_MEMORY,
            Self::Invariant { .. } => BAD_ABI
        }
    }
}
impl fmt::Display for WorkspaceError
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Busy => out.write_str("translation retirement workspace is still in use"),
            Self::Capacity { pages,
                             roots } =>
            {
                write!(out,
                       "retirement workspace cannot represent {pages} pages across {roots} roots")
            }
            Self::Allocation { part,
                               required } =>
            {
                write!(out,
                       "ordinary System Foundation retirement workspace refused {required} {part} \
                        entries before publication")
            }
            Self::Invariant { part,
                              required,
                              capacity } =>
            {
                write!(out,
                       "admitted retirement workspace {part} capacity {capacity} cannot cover \
                        {required} entries")
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum Error
{
    Missing,
    Process(super::process::Error),
    Owner,
    Closed,
    State,
    UserMapping(CleanupCause),
    Metadata,
    Workspace(WorkspaceError),
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
    Recovery(crate::allocator::recovery::Error),
    Frames(super::FrameError),
    Foundation(crate::allocator::foundation::Error),
    OrdinaryPhysical(crate::allocator::ordinary_physical::Error),
    Encoding(mac_riscv_pages::Error),
    Conflict
    {
        address:  u64,
        expected: u64,
        observed: u64
    },
    Revision,
    CloneInventory(CloneInventoryError),
    Monitor(crate::machine::monitor::Error)
}
impl Error
{
    pub(crate) fn status(&self) -> Status
    {
        match self
        {
            Self::Process(e) => e.status(),
            Self::UserMapping(_) => BAD_ABI,
            Self::Metadata => NO_MEMORY,
            Self::Workspace(cause) => cause.status(),
            Self::Account(cause) => crate::allocation::account_status(cause),
            Self::Pages(cause) => cause.status(),
            Self::Recovery(crate::allocator::recovery::Error::Exhausted { .. }
                           | crate::allocator::recovery::Error::BackingRefused { .. }) => NO_MEMORY,
            Self::Recovery(crate::allocator::recovery::Error::Layout) => BAD_ARGUMENT,
            Self::Recovery(crate::allocator::recovery::Error::AlreadyInitialized) => BAD_ABI,
            Self::Frames(cause) => cause.status(),
            Self::Foundation(cause) => cause.status(),
            Self::OrdinaryPhysical(cause) => cause.status(),
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
            Self::Process(e) => e.fmt(out),
            Self::UserMapping(cause) => cause.fmt(out),
            Self::Workspace(cause) => cause.fmt(out),
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
            Self::Recovery(cause) => cause.fmt(out),
            Self::Frames(cause) => cause.fmt(out),
            Self::Foundation(cause) => cause.fmt(out),
            Self::OrdinaryPhysical(cause) => cause.fmt(out),
            Self::Encoding(cause) => cause.fmt(out),
            Self::Conflict { address,
                             expected,
                             observed } =>
            {
                write!(out,
                       "executable PTE {address:#x} changed: expected {expected:#x}, observed \
                        {observed:#x}; original mapping/backing retained")
            }
            Self::CloneInventory(cause) => cause.fmt(out),
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
    Workspace
    {
        base: u64, cause: WorkspaceError
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
            Self::Workspace { base,
                              cause } => write!(out, "executable image {base:#x}: {cause}"),
            Self::Monitor { base,
                            stage,
                            cause } =>
            {
                write!(out, "executable image {base:#x} {stage:?} failed: {cause:?}")
            }
        }?;
        out.write_str("; original backing/account/cleanup ticket retained")?;
        match self
        {
            Self::Pending { .. } =>
            {
                out.write_str("; waiting for actual quiescence and translation acknowledgment")
            }
            _ => out.write_str("; transition quarantined; restart required")
        }
    }
}

struct Segment
{
    offset:  u64,
    memory:  u64,
    backing: PayloadBacking
}
enum PayloadBacking
{
    Owner
    {
        // Actual physical ownership precedes its original domain charge.
        backing: PagingBacking,
        _charge: Committed<NativeInvocationGate>
    },
    System(crate::allocator::ordinary_physical::SystemBacking)
}
impl PayloadBacking
{
    fn cpu_base(&self) -> usize
    {
        match self
        {
            Self::Owner { backing,
                          .. } => backing.cpu_base(),
            Self::System(b) => b.cpu_base()
        }
    }

    fn physical(&self) -> PhysicalAddress
    {
        match self
        {
            Self::Owner { backing,
                          .. } => backing.physical(),
            Self::System(b) => b.physical()
        }
    }

    fn bytes(&self) -> u64
    {
        match self
        {
            Self::Owner { backing,
                          .. } => backing.bytes(),
            Self::System(b) => b.bytes()
        }
    }
}
/// Only genuine source authority can choose the corresponding physical receipt.
pub(crate) enum DefinitionTarget<'a>
{
    System(crate::kernel::SystemResources),
    Owner(AccountUse<NativeInvocationGate>),
    Process(&'a crate::images::Read)
}
#[derive(Clone, Copy)]
enum TargetKind
{
    Application,
    Module,
    Definition,
    UserDefinition
}
#[derive(Clone, Copy)]
enum PayloadAuthority<'a>
{
    Owner(&'a AccountUse<NativeInvocationGate>),
    System(crate::kernel::SystemResources)
}
pub(super) struct Payload
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
pub(super) struct SegmentMap
{
    pub(super) virtual_base: u64,
    pub(super) physical:     u64,
    pub(super) bytes:        u64,
    pub(super) memory:       u64,
    pub(super) flags:        u32
}
#[derive(Clone, Copy)]
struct Slot
{
    root:    u64,
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
/// A read-only observation for the unique prepared image writer. Mapped is
/// not itself an ACK receipt: only a returned instruction-publication Busy
/// after translation completion permits the same writer's retry.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublicationState
{
    Prepared,
    Mapped,
    Live,
    Blocked
}
struct Entries
{
    leaves:    Vec<Slot>,
    ancestors: Vec<Slot>
}
#[derive(Clone, Copy)]
struct RetirementDemand
{
    shared_pages:   usize,
    private_leaves: usize,
    private_roots:  usize
}
struct RetirementWorkspace
{
    roots:   Vec<super::process::Pin>,
    dynamic: Entries,
    demand:  RetirementDemand
}
impl RetirementWorkspace
{
    const fn new() -> Self
    {
        Self { roots:   Vec::new(),
               dynamic: Entries { leaves:    Vec::new(),
                                  ancestors: Vec::new() },
               demand:  RetirementDemand { shared_pages:   0,
                                           private_leaves: 0,
                                           private_roots:  0 } }
    }

    fn admit(&mut self, demand: RetirementDemand, registered: usize) -> Result<(), WorkspaceError>
    {
        let leaves = demand.shared_pages
                           .checked_mul(registered)
                           .ok_or(WorkspaceError::Capacity { pages: demand.shared_pages,
                                                             roots: registered })?
                           .max(demand.private_leaves);
        let ancestors = leaves.checked_mul(2).ok_or(WorkspaceError::Capacity { pages: leaves,
                                                                                roots: 2 })?;
        let roots = if demand.shared_pages == 0 { 0 } else { registered }.max(demand.private_roots);
        // This is genuine shared coordinator storage, never a retiring subject
        // or an ambient protected allocation. Existing headers keep this origin.
        crate::context::without_recovery(|| {
            crate::allocator::with_system(crate::allocator::SystemClass::Foundation, || {
                reserve_workspace(&mut self.roots, "root pins", roots)?;
                reserve_workspace(&mut self.dynamic.leaves, "leaves", leaves)?;
                reserve_workspace(&mut self.dynamic.ancestors, "ancestors", ancestors)
            })
        })?;
        self.demand = demand;
        Ok(())
    }
}
fn reserve_workspace<T>(values: &mut Vec<T>, part: &'static str, required: usize)
                        -> Result<(), WorkspaceError>
{
    if !values.is_empty()
    {
        return Err(WorkspaceError::Invariant { part,
                                               required: values.len(),
                                               capacity: 0 });
    }
    if required > values.capacity()
    {
        values.try_reserve_exact(required).map_err(|_| {
                                               WorkspaceError::Allocation { part,
                                                                            required }
                                           })?;
    }
    Ok(())
}
/// The sole owner moves out of its short metadata guard. No borrowed lock or
/// persistent Process pin survives into an ACK wait or between turns.
struct RetirementWorkspaceLoan
{
    workspace: RetirementWorkspace
}
impl RetirementWorkspaceLoan
{
    fn take() -> Result<Self, WorkspaceError>
    {
        interrupts::with_masked(|| {
            let mut available = WORKSPACE.lock();
            match available.take()
            {
                Some(workspace) => Ok(Self { workspace }),
                None =>
                {
                    // Absence and arming share the return gate, so its actual
                    // owner cannot return between this observation and arm.
                    WAITING_WORKSPACE.store(true, Ordering::Release);
                    Err(WorkspaceError::Busy)
                }
            }
        })
    }
}
impl Drop for RetirementWorkspaceLoan
{
    fn drop(&mut self)
    {
        let mut workspace = core::mem::replace(&mut self.workspace, RetirementWorkspace::new());
        // Real Pin releases and their progress markers run outside all guards.
        workspace.roots.clear();
        workspace.dynamic.leaves.clear();
        workspace.dynamic.ancestors.clear();
        let waiting = interrupts::with_masked(|| {
            // This non-Clone loan is the only writer after taking Some. The
            // destination is still None; no owned payload is displaced here.
            let mut available = WORKSPACE.lock();
            *available = Some(workspace);
            WAITING_WORKSPACE.swap(false, Ordering::AcqRel)
        });
        if waiting
        {
            // Issuer release can precede this availability transition. Wake
            // only actually armed waiters, not an unchanged refusal/idle loop.
            changed();
        }
    }
}
/// Field order releases an unissued command before returning scratch on every
/// pre-effect exit. Issued commands finish while these actual Pins stay owned.
struct RetirementTurn
{
    lease:     Option<crate::machine::monitor::TranslationLease>,
    workspace: Option<RetirementWorkspaceLoan>
}
fn add_private_demand(demand: &mut RetirementDemand, pages: usize, roots: usize)
                      -> Result<(), WorkspaceError>
{
    let leaves = pages.checked_mul(roots).ok_or(WorkspaceError::Capacity { pages,
                                                                            roots })?;
    demand.private_leaves = demand.private_leaves.max(leaves);
    if pages != 0
    {
        demand.private_roots = demand.private_roots.max(roots);
    }
    Ok(())
}
/// Call only inside the existing clone-to-link translation exclusion. The
/// counts and private page demand come from canonical roots/visibility records.
pub(super) fn admit_process_workspace(_lease: &crate::machine::monitor::TranslationLease,
                                      registered: usize, private_pages: usize, eligible: usize)
                                      -> Result<(), WorkspaceError>
{
    let mut loan = RetirementWorkspaceLoan::take()?;
    let mut demand = loan.workspace.demand;
    add_private_demand(&mut demand, private_pages, eligible)?;
    loan.workspace.admit(demand, registered)
}
fn admit_mapping_workspace(_lease: &crate::machine::monitor::TranslationLease, target: TargetKind,
                           owner: mac_object_table::domain::DomainId, pages: usize)
                           -> Result<(), Error>
{
    let registered = super::process::registered_count(None).map_err(Error::Process)?;
    let mut loan = RetirementWorkspaceLoan::take().map_err(Error::Workspace)?;
    let mut demand = loan.workspace.demand;
    if matches!(target, TargetKind::Definition)
       && owner.kind() == mac_object_table::domain::DomainKind::Process
    {
        let eligible = super::process::registered_count(Some(owner)).map_err(Error::Process)?;
        add_private_demand(&mut demand, pages, eligible).map_err(Error::Workspace)?;
    }
    else
    {
        demand.shared_pages = demand.shared_pages.max(pages);
    }
    loan.workspace.admit(demand, registered).map_err(Error::Workspace)
}
fn mapped_pages(segments: &[SegmentMap]) -> Result<usize, WorkspaceError>
{
    segments.iter().try_fold(0usize, |total, segment| {
                       let pages = usize::try_from(segment.bytes / PAGE_BYTES).map_err(|_| {
                                       WorkspaceError::Capacity { pages: usize::MAX,
                                                                  roots: 1 }
                                   })?;
                       total.checked_add(pages).ok_or(WorkspaceError::Capacity { pages,
                                                                                 roots: 1 })
                   })
}
struct State
{
    phase:        Phase,
    requested:    bool,
    user_running: bool,
    user_waiting: bool
}
/// The original canonical IMAG or DDEF record owns this anchor before the first PTE effect.
/// Payload removal happens only after the actual translation receipt. Drop
/// itself performs no unmap, monitor wait or fallible teardown.
pub(crate) struct Mapping
{
    owner:             OwnerRef,
    base:              u64,
    target:            TargetKind,
    source:            BackingSource,
    entry:             u64,
    segments:          Vec<SegmentMap>,
    retirement_pages:  usize,
    leaves:            spin::Once<Entries>,
    payload:           Mutex<Option<Arc<Payload>>>,
    definition_source: spin::Once<crate::images::Read>,
    user_mapping:      Mutex<Option<super::user::Mapping>>,
    state:             Mutex<State>
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
        let target = match (owner.owner().kind(), descriptor.role)
        {
            (mac_object_table::domain::DomainKind::Module, Role::CodeResource) =>
            {
                TargetKind::Module
            }
            (mac_object_table::domain::DomainKind::Process, Role::Application) =>
            {
                TargetKind::Application
            }
            _ => return Err(Error::Owner)
        };
        if owner.owner() != account.owner()
        {
            return Err(Error::Owner);
        }
        Self::prepare_payload(owner,
                              PayloadAuthority::Owner(account),
                              target,
                              BackingSource::Ordinary,
                              descriptor,
                              bytes,
                              relocations,
                              base)
    }

    /// The only protected executable customer is the genuine replacement
    /// Application. Its canonical image/account and Mapping lifecycle are unchanged.
    pub(crate) fn prepare_recovery_application(owner: &OwnerRef,
                                               account: &AccountUse<NativeInvocationGate>,
                                               permit: &crate::allocator::recovery::Permit,
                                               descriptor: &Descriptor, bytes: &[u8],
                                               relocations: &[u8], base: u64)
                                               -> Result<Arc<Self>, Error>
    {
        if owner.owner().kind() != mac_object_table::domain::DomainKind::Process
           || descriptor.role != Role::Application
           || owner.owner() != account.owner()
        {
            return Err(Error::Owner);
        }
        Self::prepare_payload(owner,
                              PayloadAuthority::Owner(account),
                              TargetKind::Application,
                              BackingSource::Protected(permit.clone()),
                              descriptor,
                              bytes,
                              relocations,
                              base)
    }

    pub(crate) fn prepare_definition(owner: &OwnerRef, authority: &DefinitionTarget,
                                     descriptor: &Descriptor, bytes: &[u8], relocations: &[u8],
                                     base: u64)
                                     -> Result<Arc<Self>, Error>
    {
        if descriptor.role != Role::CodeResource
        {
            return Err(Error::Owner);
        }
        let (payload_authority, target, original) = match authority
        {
            DefinitionTarget::System(source) if owner.owner() == source.owner().owner() =>
            {
                (PayloadAuthority::System(*source), TargetKind::Definition, None)
            }
            DefinitionTarget::Owner(account)
                if owner.owner().kind() == mac_object_table::domain::DomainKind::Module
                   && owner.owner() == account.owner() =>
            {
                (PayloadAuthority::Owner(account), TargetKind::Definition, None)
            }
            DefinitionTarget::Process(read)
                if owner.owner().kind() == mac_object_table::domain::DomainKind::Process
                   && owner.owner() == read.owner().owner()
                   && read.allocation().owner() == owner.owner() =>
            {
                (PayloadAuthority::Owner(read.allocation()),
                 TargetKind::UserDefinition,
                 Some(read.retain().map_err(|_| Error::Owner)?))
            }
            _ => return Err(Error::Owner)
        };
        let mapping = Self::prepare_payload(owner,
                                            payload_authority,
                                            target,
                                            BackingSource::Ordinary,
                                            descriptor,
                                            bytes,
                                            relocations,
                                            base)?;
        if let Some(read) = original
        {
            mapping.definition_source.call_once(|| read);
        }
        Ok(mapping)
    }

    fn prepare_payload(owner: &OwnerRef, authority: PayloadAuthority<'_>, target: TargetKind,
                       source: BackingSource, descriptor: &Descriptor, bytes: &[u8],
                       relocations: &[u8], base: u64)
                       -> Result<Arc<Self>, Error>
    {
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
        let region = match target
        {
            TargetKind::Module => base >= VM_MODULE_START && end <= VM_MODULE_END,
            TargetKind::Application => super::super::memory::application_region(base, end),
            TargetKind::Definition | TargetKind::UserDefinition =>
            {
                base >= DEFINITION_BASE && end <= DEFINITION_LIMIT
            }
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
            let backing = match authority
            {
                PayloadAuthority::Owner(account) =>
                {
                    match &source
                    {
                        BackingSource::Foundation =>
                        {
                            // Initial System table backing is not an image source.
                            return Err(Error::State);
                        }
                        BackingSource::Ordinary =>
                        {
                            let planned =
                                Backing::planned_bytes(mapped as usize).map_err(Error::Pages)?;
                            let charge = account.reserve_kernel(planned).map_err(Error::Account)?;
                            let backing = Backing::reserve_uninitialized(mapped as usize).map_err(Error::Pages)?;
                            if backing.bytes() != planned
                            {
                                return Err(Error::State);
                            }
                            PayloadBacking::Owner { backing: PagingBacking::Ordinary(backing),
                                                    _charge: charge.commit() }
                        }
                        BackingSource::Protected(permit) =>
                        {
                            let layout = Layout::from_size_align(mapped as usize, PAGE_BYTES as usize)
                            .map_err(|_| Error::Recovery(crate::allocator::recovery::Error::Layout))?;
                            let planned = crate::allocator::recovery::Permit::backing_layout(layout)
                            .map_err(Error::Recovery)?.size() as u64;
                            // Preflight the original account before consuming raw storage.
                            // Commit only after its real normalized extent is owned;
                            // LoadPlan clears it before any PTE publication.
                            let charge = account.reserve_kernel(planned).map_err(Error::Account)?;
                            let backing = permit.reserve_uninitialized_backing(layout)
                                                .map_err(Error::Recovery)?;
                            if backing.bytes() != planned
                            {
                                return Err(Error::State);
                            }
                            PayloadBacking::Owner { backing: PagingBacking::Protected(backing),
                                                    _charge: charge.commit() }
                        }
                    }
                }
                PayloadAuthority::System(source) =>
                {
                    let planned = Backing::planned_bytes(mapped as usize).map_err(Error::Pages)?;
                    let backing = crate::allocator::ordinary_physical::SystemBacking::reserve_uninitialized(source, mapped as usize).map_err(Error::OrdinaryPhysical)?;
                    if backing.bytes() != planned
                    {
                        return Err(Error::State);
                    }
                    PayloadBacking::System(backing)
                }
            };
            segments.push(SegmentMap { virtual_base: base + segment.offset,
                                       physical:     backing.physical().0,
                                       bytes:        mapped,
                                       memory:       segment.memory_length,
                                       flags:        segment.flags });
            payload.segments.push(Segment { offset: segment.offset,
                                            memory: segment.memory_length,
                                            backing });
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
        let retirement_pages = mapped_pages(&segments).map_err(Error::Workspace)?;
        Arc::try_new(Self { owner: owner.clone(),
                            base,
                            target,
                            source,
                            entry: plan.entry(),
                            segments,
                            retirement_pages,
                            leaves: spin::Once::new(),
                            payload: Mutex::new(Some(Arc::try_new(payload).map_err(|_| {
                                                                              Error::Metadata
                                                                          })?)),
                            definition_source: spin::Once::new(),
                            user_mapping: Mutex::new(None),
                            state: Mutex::new(State { phase:        Phase::Prepared,
                                                      requested:    false,
                                                      user_running: false,
                                                      user_waiting: false }) }).map_err(|_| {
                                                                                   Error::Metadata
                                                                               })
    }

    pub(crate) fn publication_state(&self) -> PublicationState
    {
        self.with_state(|state| {
                if state.requested
                {
                    return PublicationState::Blocked;
                }
                match state.phase
                {
                    Phase::Prepared => PublicationState::Prepared,
                    Phase::Mapped => PublicationState::Mapped,
                    Phase::Live => PublicationState::Live,
                    Phase::Preparing
                    | Phase::Cleared
                    | Phase::Unmapped
                    | Phase::Settled
                    | Phase::Quarantined(_) => PublicationState::Blocked
                }
            })
    }

    pub(crate) fn retirement_pages(&self) -> usize
    {
        self.retirement_pages
    }

    pub(crate) fn entry(&self) -> Option<u64>
    {
        (self.phase() == Phase::Live).then_some(self.entry)
    }

    pub(crate) fn quarantined(&self) -> bool
    {
        matches!(self.phase(), Phase::Quarantined(_) | Phase::Cleared)
    }

    pub(crate) fn unpublished(&self) -> bool
    {
        matches!(self.phase(), Phase::Prepared | Phase::Preparing)
    }

    pub(crate) fn publish(&self) -> Result<(), Error>
    {
        if matches!(self.target, TargetKind::UserDefinition)
        {
            return self.source.with_metadata(|| self.publish_user_definition());
        }
        if !self.owner.accepting()
        {
            return Err(Error::Closed);
        }
        if self.phase() == Phase::Live
        {
            return Ok(());
        }
        if self.phase() == Phase::Mapped
        {
            if let Err(cause) = crate::machine::monitor::publish_instructions()
            {
                if !matches!(cause, crate::machine::monitor::Error::Busy)
                {
                    self.quarantine(Stage::PublishInstructions, &cause)
                }
                return Err(Error::Monitor(cause));
            }
            self.set_phase(Phase::Live);
            return Ok(());
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
        if !matches!(self.target, TargetKind::Application)
        {
            admit_mapping_workspace(&lease,
                                    self.target,
                                    self.owner.owner(),
                                    self.retirement_pages)?;
        }
        // The Mapping retains its genuine origin across deferred publication;
        // scratch and retained Entries do not borrow the executor's heap source.
        let plan =
            self.source.with_metadata(|| {
                            let plan = Plan::prepare(&self.segments,
                                                     self.owner.owner(),
                                                     self.target,
                                                     &self.source)?;
                            let mut leaves = Vec::new();
                            leaves.try_reserve_exact(plan.leaves
                                                         .iter()
                                                         .filter(|s| is_permanent_root(s.root))
                                                         .count())
                                  .map_err(|_| Error::Metadata)?;
                            for slot in &plan.leaves
                            {
                                if is_permanent_root(slot.root)
                                {
                                    leaves.push(*slot)
                                }
                            } // exact fallible capacity precedes effects
                            let mut ancestors = Vec::new();
                            ancestors.try_reserve_exact(plan.ancestors
                                                            .iter()
                                                            .filter(|s| is_permanent_root(s.root))
                                                            .count())
                                     .map_err(|_| Error::Metadata)?;
                            for slot in &plan.ancestors
                            {
                                if is_permanent_root(slot.root)
                                {
                                    ancestors.push(*slot)
                                }
                            }
                            self.leaves.call_once(|| {
                                           Entries { leaves,
                                                     ancestors }
                                       });
                            Ok::<_, Error>(plan)
                        })?;
        if !self.owner.accepting()
        {
            return Err(Error::Closed);
        }
        let _committed = plan.commit()?;
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
        if matches!(self.target, TargetKind::UserDefinition)
        {
            return self.retire_user_definition();
        }
        let phase = self.phase();
        match phase
        {
            Phase::Settled => return Ok(()),
            Phase::Quarantined(_) | Phase::Cleared => return Err(Error::State),
            Phase::Prepared | Phase::Preparing | Phase::Unmapped =>
            {}
            Phase::Mapped | Phase::Live =>
            {
                // Arm a scratch wait before taking a new issuer. Otherwise a
                // still-lent workspace after monitor release could make this
                // unchanged Busy drop an unissued lease and wake itself.
                let workspace = if matches!(self.target, TargetKind::Application)
                {
                    None
                }
                else
                {
                    Some(RetirementWorkspaceLoan::take().map_err(Error::Workspace)?)
                };
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
                // Application uses only its original Entries. Other targets
                // borrow the admitted workspace and keep real Pins through ACK.
                let mut turn = RetirementTurn { lease: Some(lease),
                                                workspace };
                let prepared = (|| {
                    if let Some(loan) = &mut turn.workspace
                    {
                        let owner = (matches!(self.target, TargetKind::Definition)
                            && self.owner.owner().kind() == mac_object_table::domain::DomainKind::Process)
                            .then_some(self.owner.owner());
                        let workspace = &mut loan.workspace;
                        if self.retirement_pages != 0
                        {
                            super::process::snapshot_into(owner, &mut workspace.roots)
                                .map_err(Error::Process)?;
                        }
                        process_entries(&self.segments, &workspace.roots, &mut workspace.dynamic)?;
                    }
                    Ok::<_, Error>(())
                })();
                if let Err(cause) = prepared
                {
                    let workspace_error = match &cause
                    {
                        Error::Workspace(cause)
                        | Error::Process(super::process::Error::Workspace(cause)) => Some(*cause),
                        _ => None
                    };
                    if let Some(cause) = workspace_error.filter(|cause| {
                                             matches!(cause,
                                                      WorkspaceError::Invariant { .. }
                                                      | WorkspaceError::Capacity { .. })
                                         })
                    {
                        self.settle_phase(Phase::Quarantined(CleanupCause::Workspace { base:
                                                                                       self.base,
                                                                                   cause }));
                    }
                    return Err(cause);
                }
                let dynamic = turn.workspace.as_ref().map(|loan| &loan.workspace.dynamic);
                if let Err(cause) = unmap(leaves, dynamic)
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
                match turn.lease.take().ok_or(Error::State)?.finish()
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
        self.settle_phase(Phase::Quarantined(monitor_cause(self.base, stage, error)));
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
    table: BranchBacking,
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
    branches:        Option<Box<Branch>>,
    // Committed branches belonging only to the permanent kernel root. Empty
    // branches remain owned after unmap; private-root retirement cannot remove
    // them. Updated in the same lock/revision transaction as their PTEs.
    kernel_branches: usize
}
/// Inventory of actual owned tables, not a reservation or application quota.
/// The existing translation lease must span this snapshot and template copying.
pub(super) struct CloneFootprint
{
    pub(super) tables:   usize,
    pub(super) revision: u64
}
#[derive(Debug)]
pub(crate) enum CloneInventoryError
{
    Unavailable,
    Changing
    {
        revision: u64
    },
    Overflow
    {
        permanent:  usize,
        dynamic:    usize,
        additional: usize
    }
}
impl fmt::Display for CloneInventoryError
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Unavailable =>
            {
                out.write_str("System clone table inventory has no permanent kernel root")
            }
            Self::Changing { revision } =>
            {
                write!(out,
                       "System clone table inventory observed unfinished mapping revision \
                        {revision}; no Process root published")
            }
            Self::Overflow { permanent,
                             dynamic,
                             additional } =>
            {
                write!(out,
                       "System clone table inventory overflow: {permanent} permanent, {dynamic} \
                        committed dynamic, {additional} additional tables; no new mappings \
                        published")
            }
        }
    }
}
/// Bootstrap Frames.count includes the extracted root. Each later kernel
/// branch is retained by Tables exactly once. The clone keeps a subset of this
/// hierarchy and copies leaves without splitting them, so their sum is an upper
/// bound without walking page tables or counting other processes' roots.
pub(super) fn clone_footprint() -> Result<CloneFootprint, CloneInventoryError>
{
    let spaces = SPACES.get().ok_or(CloneInventoryError::Unavailable)?;
    interrupts::with_masked(|| {
        let inventory = TABLES.lock();
        let revision = REVISION.load(Ordering::Acquire);
        if revision & 1 != 0
        {
            return Err(CloneInventoryError::Changing { revision });
        }
        let tables =
            spaces.kernel
                  .tables
                  .count
                  .checked_add(inventory.kernel_branches)
                  .ok_or(CloneInventoryError::Overflow { permanent:  spaces.kernel.tables.count,
                                                         dynamic:    inventory.kernel_branches,
                                                         additional: 0 })?;
        Ok(CloneFootprint { tables,
                            revision })
    })
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
enum BranchBacking
{
    Foundation(Foundation<Backing>),
    // Suballocated from the already owned System heap, not a new page-pool
    // lease. Its unique RawBacking receipt is the complete occupancy charge.
    // Committed permanent branches remain retained after payload unmap.
    ProtectedSystem(crate::allocator::recovery::Backing),
    Process
    {
        backing: PagingBacking,
        _charge: Committed<NativeInvocationGate>
    }
}
impl BranchBacking
{
    fn cpu_base(&self) -> usize
    {
        match self
        {
            Self::Foundation(b) => b.cpu_base(),
            Self::ProtectedSystem(b) => b.cpu_base(),
            Self::Process { backing,
                            .. } => backing.cpu_base()
        }
    }

    fn physical(&self) -> PhysicalAddress
    {
        match self
        {
            Self::Foundation(b) => b.physical(),
            Self::ProtectedSystem(b) => b.physical(),
            Self::Process { backing,
                            .. } => mac_kernel_model::pages::PageBacking::physical(backing)
        }
    }
}

struct Plan
{
    _processes: Vec<super::process::Pin>,
    branches:   Vec<Box<Branch>>,
    edits:      Vec<Slot>,
    leaves:     Vec<Slot>,
    checks:     Vec<Slot>,
    ancestors:  Vec<Slot>
}
impl Plan
{
    fn prepare(segments: &[SegmentMap], owner: mac_object_table::domain::DomainId,
               target: TargetKind, source: &BackingSource)
               -> Result<Self, Error>
    {
        let mut plan = Self { _processes: Vec::new(),
                              branches:   Vec::new(),
                              edits:      Vec::new(),
                              leaves:     Vec::new(),
                              checks:     Vec::new(),
                              ancestors:  Vec::new() };
        let spaces = SPACES.get().ok_or(Error::Missing)?;
        let pages = segments.iter()
                            .try_fold(0usize, |n, s| n.checked_add((s.bytes / PAGE_BYTES) as usize))
                            .ok_or(Error::Metadata)?;
        let processes = participating_roots(target, owner, true)?;
        let total = pages.checked_mul(processes.len().checked_add(2).ok_or(Error::Metadata)?)
                         .ok_or(Error::Metadata)?;
        plan.leaves.try_reserve_exact(total).map_err(|_| Error::Metadata)?;
        plan.edits.try_reserve_exact(total).map_err(|_| Error::Metadata)?;
        // Applications are never permanent Kernel/FW mappings. Their owning
        // Process installs these same segments through prepare_user below.
        if !matches!(target, TargetKind::Application)
        {
            for space in [&spaces.kernel, &spaces.firmware]
            {
                plan.add_space(segments, space, None, source, Access::Supervisor)?;
            }
            for process in &processes
            {
                plan.add_space(segments,
                               process.space(),
                               Some(&process.allocation),
                               source,
                               Access::Supervisor)?;
            }
        }
        plan._processes = processes;
        plan.leaves.sort_unstable_by_key(|leaf| leaf.address);
        Ok(plan)
    }

    fn prepare_user(segments: &[SegmentMap], process: &super::process::Pin) -> Result<Self, Error>
    {
        for segment in segments
        {
            if segment.flags & image::WRITE != 0 && segment.flags & image::EXECUTE != 0
            {
                return Err(Error::Owner);
            }
        }
        let pages = mapped_pages(segments).map_err(Error::Workspace)?;
        let mut plan = Self { _processes: Vec::new(),
                              branches:   Vec::new(),
                              edits:      Vec::new(),
                              leaves:     Vec::new(),
                              checks:     Vec::new(),
                              ancestors:  Vec::new() };
        plan.leaves.try_reserve_exact(pages).map_err(|_| Error::Metadata)?;
        plan.edits.try_reserve_exact(pages).map_err(|_| Error::Metadata)?;
        plan.add_space(segments,
                       process.space(),
                       Some(&process.allocation),
                       &crate::context::recovery_allocation().map(BackingSource::Protected)
                                                             .unwrap_or(BackingSource::Ordinary),
                       Access::User)?;
        Ok(plan)
    }

    fn add_space(&mut self, segments: &[SegmentMap], space: &super::Space,
                 account: Option<&crate::machine::scheduler::accounting::AllocationAuthority>,
                 source: &BackingSource, access: Access)
                 -> Result<(), Error>
    {
        let boot = BOOT.get().ok_or(Error::Missing)?;
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
                        let table = self.table(Key { root:   root.0,
                                                     level:  1,
                                                     prefix: l1_prefix },
                                               root_table,
                                               address,
                                               space,
                                               account,
                                               source,
                                               access == Access::User)?;
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
                        let table = self.table(Key { root:   root.0,
                                                     level:  0,
                                                     prefix: l0_prefix },
                                               l1,
                                               address,
                                               space,
                                               account,
                                               source,
                                               access == Access::User)?;
                        l0_cache = Some((l0_prefix, table));
                        table
                    }
                };
                let permissions = Permissions { read: segment.flags & image::READ != 0,
                                                write: segment.flags & image::WRITE != 0,
                                                execute: segment.flags & image::EXECUTE != 0,
                                                access,
                                                global: false };
                let bits = Entry::leaf(PhysicalAddress(segment.physical + offset),
                                       LeafSize::Page,
                                       permissions,
                                       PageOwnership::Manual).map_err(Error::Encoding)?
                                                             .bits();
                let slot = Slot { root: root.0,
                                  cpu: l0.cpu,
                                  index: Sv39::index(address, 0).map_err(Error::Encoding)?,
                                  bits,
                                  address };
                // Branch edits consume their own admitted positions. Keep
                // the final leaf insertion fallible as well; a branch is
                // not allowed to steal untracked future-leaf headroom.
                self.edits.try_reserve(1).map_err(|_| Error::Metadata)?;
                self.leaves.push(slot);
                self.edits.push(slot);
                offset += PAGE_BYTES;
            }
        }
        Ok(())
    }

    fn table(&mut self, key: Key, parent: Table, address: u64, space: &super::Space,
             account: Option<&crate::machine::scheduler::accounting::AllocationAuthority>,
             source: &BackingSource, user: bool)
             -> Result<Table, Error>
    {
        if let Some(table) =
            self.branches.iter().find(|branch| branch.key == key).map(|branch| branch.reference())
        {
            return Ok(table);
        }
        let index = Sv39::index(address, key.level + 1).map_err(Error::Encoding)?;
        let mut slot = Slot { root: key.root,
                              cpu: parent.cpu,
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
        let prepare = || {
            let table = match account
            {
                Some(account) =>
                {
                    let layout = Layout::new::<[u64; 512]>()
                        .align_to(PAGE_BYTES as usize).map_err(|_| Error::State)?;
                    let origin=if user {source}else{&space.tables.source};
                    let bytes = origin.planned_bytes(layout).map_err(Error::Frames)?;
                    let charge = account.reserve_kernel(bytes).map_err(Error::Account)?;
                    let backing = origin.allocate_zeroed(layout).map_err(Error::Frames)?;
                    BranchBacking::Process { backing, _charge: charge.commit() }
                }
                None => match source
                {
                    BackingSource::Foundation =>
                    {
                        // Only initial prepare_space selects this source.
                        return Err(Error::State);
                    }
                    BackingSource::Ordinary => BranchBacking::Foundation(
                        Foundation::<Backing>::allocate(PAGE_BYTES as usize).map_err(Error::Foundation)?
                    ),
                    BackingSource::Protected(permit) =>
                    {
                        let layout = Layout::new::<[u64; 512]>()
                            .align_to(PAGE_BYTES as usize).map_err(|_| Error::State)?;
                        let backing = permit.allocate_backing(layout)
                            .map_err(|cause| Error::Frames(super::FrameError::Recovery(cause)))?;
                        BranchBacking::ProtectedSystem(backing)
                    }
                }
            };
            Box::try_new(Branch { key,
                                  table,
                                  next: None }).map_err(|_| Error::Metadata)
        };
        let branch = match account
        {
            Some(account) =>
            {
                let target = crate::allocator::Target::Owner {
                    account: account.clone(),
                    class: mac_kernel_model::policy::MemoryClass::Kernel
                };
                let origin = if user { source } else { &space.tables.source };
                origin.with_metadata(|| crate::allocator::with_target(target, prepare))
            }
            None =>
            {
                source.with_metadata(|| {
                          crate::allocator::with_system(crate::allocator::SystemClass::Foundation,
                                                        prepare)
                      })
            }
        }?;
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

    fn commit(mut self) -> Result<Self, Error>
    {
        let kernel = &SPACES.get().ok_or(Error::Missing)?.kernel;
        let kernel_root = kernel.owner.root_physical().0;
        let additional =
            self.branches.iter().filter(|branch| branch.key.root == kernel_root).count();
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
            // All inventory arithmetic must succeed before linking owners or
            // exposing PTEs. The true bootstrap count includes its root table.
            let overflow = || {
                Error::CloneInventory(CloneInventoryError::Overflow { permanent: kernel.tables
                                                                                       .count,
                                                                      dynamic:
                                                                          tables.kernel_branches,
                                                                      additional })
            };
            let kernel_branches =
                tables.kernel_branches.checked_add(additional).ok_or_else(overflow)?;
            kernel.tables.count.checked_add(kernel_branches).ok_or_else(overflow)?;
            let revision = REVISION.load(Ordering::Acquire);
            let next =
                revision.checked_add(2).filter(|_| revision & 1 == 0).ok_or(Error::Revision)?;
            REVISION.store(revision + 1, Ordering::Release);
            while let Some(mut branch) = self.branches.pop()
            {
                branch.next = tables.branches.take();
                tables.branches = Some(branch);
            }
            tables.kernel_branches = kernel_branches;
            for edit in &self.edits
            {
                edit.store(edit.bits);
            }
            REVISION.store(next, Ordering::Release);
            Ok(())
        })?;
        Ok(self)
    }
}
fn unmap(entries: &Entries, dynamic: Option<&Entries>) -> Result<(), Error>
{
    interrupts::with_masked(|| {
        let _tables = TABLES.lock();
        let components = [Some(entries), dynamic];
        for leaf in
            components.iter()
                      .flatten()
                      .flat_map(|entries| entries.ancestors.iter().chain(entries.leaves.iter()))
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
        for leaf in components.iter().flatten().flat_map(|entries| &entries.leaves)
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
pub(super) fn user_readers_released()
{
    crate::toolbox::resources::definitions::monitor_released();
    if RETIRING.load(Ordering::Acquire) != 0
    {
        PENDING.store(true, Ordering::Release);
        crate::machine::scheduler::wake::request();
    }
}
pub(crate) fn monitor_released()
{
    crate::toolbox::resources::definitions::monitor_released();
    super::process::monitor_released();
    super::user::monitor_released();
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

fn is_permanent_root(root: u64) -> bool
{
    SPACES.get()
          .is_some_and(|s| {
              root == s.kernel.owner.root_physical().0 || root == s.firmware.owner.root_physical().0
          })
}
/// Physical coordinates are interpreted only after matching a real retained
/// table owner. Source cloning holds the actual translation command lease.
pub(super) fn table_snapshot(physical: PhysicalAddress) -> Result<[u64; 512], Error>
{
    let spaces = SPACES.get().ok_or(Error::Missing)?;
    let cpu = table_in_space(&spaces.kernel, physical).or_else(|| {
                                                          table_in_space(&spaces.firmware, physical)
                                                      })
                                                      .ok_or(Error::State)?;
    let mut entries = [0u64; 512];
    for (index, entry) in entries.iter_mut().enumerate()
    {
        // SAFETY: table_in_space proves the complete aligned native table;
        // the actual command lease excludes table/PTE retirement. Returning
        // copied scalars avoids borrowing mutable hardware-visible entries.
        *entry =
            unsafe { AtomicU64::from_ptr((cpu as *mut u64).add(index)) }.load(Ordering::Acquire);
    }
    Ok(entries)
}

fn table_in_space(space: &super::Space, physical: PhysicalAddress) -> Option<usize>
{
    if space.owner.root_physical() == physical
    {
        return BOOT.get()?.ram.virtual_address(physical, PAGE_BYTES).ok().map(|a| a.0 as usize);
    }
    if let Ok(node) = space.tables.node(physical)
    {
        return Some(node.cpu);
    }
    interrupts::with_masked(|| {
        let tables = TABLES.lock();
        let mut next = tables.branches.as_deref();
        while let Some(branch) = next
        {
            if branch.key.root == space.owner.root_physical().0
               && branch.table.physical() == physical
            {
                return Some(branch.table.cpu_base());
            }
            next = branch.next.as_deref();
        }
        None
    })
}
fn process_entries(segments: &[SegmentMap], roots: &[super::process::Pin], entries: &mut Entries)
                   -> Result<(), Error>
{
    let pages = mapped_pages(segments).map_err(Error::Workspace)?;
    let count = pages.checked_mul(roots.len())
                     .ok_or(Error::Workspace(WorkspaceError::Capacity { pages,
                                                                        roots: roots.len() }))?;
    let ancestors = count.checked_mul(2)
                         .ok_or(Error::Workspace(WorkspaceError::Capacity { pages: count,
                                                                            roots: 2 }))?;
    for (part, required, capacity, len) in
        [("leaves", count, entries.leaves.capacity(), entries.leaves.len()),
         ("ancestors", ancestors, entries.ancestors.capacity(), entries.ancestors.len())]
    {
        if len != 0 || required > capacity
        {
            return Err(Error::Workspace(WorkspaceError::Invariant { part,
                                                                    required:
                                                                        required.max(len),
                                                                    capacity }));
        }
    }
    for row in roots
    {
        let space = row.space();
        for segment in segments
        {
            'page: for offset in (0..segment.bytes).step_by(PAGE_BYTES as usize)
            {
                let address = segment.virtual_base + offset;
                let mut physical = space.owner.root_physical();
                for level in [2, 1]
                {
                    let cpu = table_in_space(space, physical).ok_or(Error::State)?;
                    let index = Sv39::index(address, level).map_err(Error::Encoding)?;
                    let mut slot = Slot { root: space.owner.root_physical().0,
                                          cpu,
                                          index,
                                          bits: 0,
                                          address };
                    slot.bits = slot.load();
                    let child = match Entry::decode_raw(slot.bits, level).map_err(Error::Encoding)?
                    {
                        Decoded::Table { physical,
                                         .. } => physical,
                        Decoded::Invalid => continue 'page,
                        _ => return Err(Error::State)
                    };
                    entries.ancestors.push(slot);
                    physical = child;
                }
                let cpu = table_in_space(space, physical).ok_or(Error::State)?;
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
                let slot = Slot { root: space.owner.root_physical().0,
                                  cpu,
                                  index: Sv39::index(address, 0).map_err(Error::Encoding)?,
                                  bits,
                                  address };
                // A root prepared after publication inherited this mapping;
                // stopped roots omitted from a later publisher may not have it.
                if slot.load() != 0
                {
                    entries.leaves.push(slot);
                }
            }
        }
    }
    Ok(())
}
/// Canonical SPCE retirement owns the actual invalidation ACK before taking
/// these physical/receipt owners. No Mapping retains private-root raw Slots.
pub(super) fn retire_process_branches(root: PhysicalAddress)
{
    let mut removed = interrupts::with_masked(|| {
        let mut tables = TABLES.lock();
        let mut source = tables.branches.take();
        let mut keep = None;
        let mut removed = None;
        while let Some(mut branch) = source
        {
            source = branch.next.take();
            if branch.key.root == root.0
            {
                branch.next = removed.take();
                removed = Some(branch)
            }
            else
            {
                branch.next = keep.take();
                keep = Some(branch)
            }
        }
        tables.branches = keep;
        removed
    });
    while let Some(mut branch) = removed
    {
        removed = branch.next.take();
        drop(branch)
    }
}

fn participating_roots(target: TargetKind, owner: mac_object_table::domain::DomainId,
                       publication: bool)
                       -> Result<Vec<super::process::Pin>, Error>
{
    if matches!(target, TargetKind::Application)
    {
        return Ok(Vec::new());
    }
    let mut roots = super::process::snapshot(publication).map_err(Error::Process)?;
    if matches!(target, TargetKind::Definition)
       && owner.kind() == mac_object_table::domain::DomainKind::Process
    {
        roots.retain(|r| r.owner() == owner)
    }
    Ok(roots)
}

/// Retained validated slots for one owning Process's user mapping. The caller
/// keeps the real Process/table pin and physical payload until unmap ACK.
pub(super) struct UserEntries(Entries);
impl UserEntries
{
    pub(super) fn unmap(&self, _lease: &crate::machine::monitor::TranslationLease)
                        -> Result<(), Error>
    {
        unmap(&self.0, None)
    }
}
pub(super) fn map_user(process: &super::process::Pin, segments: &[SegmentMap],
                       _lease: &crate::machine::monitor::TranslationLease)
                       -> Result<UserEntries, Error>
{
    let plan = super::user::with_metadata(process, || Plan::prepare_user(segments, process))?;
    if !process.admitting()
    {
        return Err(Error::Closed);
    }
    let plan = plan.commit()?;
    Ok(UserEntries(Entries { leaves:    plan.leaves,
                             ancestors: plan.ancestors }))
}
impl Mapping
{
    /// The retained image Read is the caller's actual payload lifetime pin.
    pub(crate) fn user_spans(&self) -> Result<Vec<super::user::Span>, Error>
    {
        if !matches!(self.target, TargetKind::Application | TargetKind::UserDefinition)
           || matches!(self.phase(), Phase::Settled | Phase::Cleared | Phase::Quarantined(_))
        {
            return Err(Error::State);
        }
        let payload =
            interrupts::with_masked(|| self.payload.lock().as_ref().cloned()).ok_or(Error::State)?;
        let mut result = Vec::new();
        result.try_reserve_exact(self.segments.len()).map_err(|_| Error::Metadata)?;
        for (map, owned) in self.segments.iter().zip(&payload.segments)
        {
            result.push(super::user::Span { address:  map.virtual_base as usize,
                                            cpu:      owned.backing.cpu_base(),
                                            physical: PhysicalAddress(map.physical),
                                            bytes:    map.bytes as usize,
                                            flags:    map.flags });
        }
        Ok(result)
    }
}

impl Mapping
{
    fn user_turn<R>(&self,
                    operation: impl FnOnce(&mut super::user::Mapping) -> Result<R, super::user::Error>)
                    -> Result<R, Error>
    {
        let mut mapping =
            interrupts::with_masked(|| self.user_mapping.lock().take()).ok_or(Error::State)?;
        let result = operation(&mut mapping);
        let cause = mapping.quarantine_cause();
        let conflict = interrupts::with_masked(|| self.user_mapping.lock().replace(mapping));
        let conflict_present = conflict.is_some();
        drop(conflict);
        if let Some(cause) = cause
        {
            self.settle_phase(Phase::Quarantined(cause));
        }
        if conflict_present
        {
            return Err(Error::State);
        }
        result.map_err(|error| {
                  match error
                  {
                      super::user::Error::Monitor { cause,
                                                    .. } => Error::Monitor(cause),
                      super::user::Error::Paging(cause) => cause,
                      super::user::Error::Quarantined(cause) => Error::UserMapping(cause),
                      super::user::Error::Readers(count) =>
                      {
                          Error::Process(super::process::Error::Readers(count))
                      }
                      super::user::Error::Rejected(status) =>
                      {
                          Error::Process(super::process::Error::DefinitionVisibility { status })
                      }
                  }
              })
    }

    fn publish_user_definition(&self) -> Result<(), Error>
    {
        let _turn = self.user_operation()?;
        if !self.owner.accepting()
        {
            return Err(Error::Closed);
        }
        if self.phase() == Phase::Live
        {
            return Ok(());
        }
        if self.phase() == Phase::Prepared
        {
            self.with_state(|state| {
                    if state.phase != Phase::Prepared
                    {
                        return Err(Error::State);
                    }
                    state.phase = Phase::Preparing;
                    Ok(())
                })?;
            let prepared = (|| {
                let source = self.definition_source.get().ok_or(Error::Owner)?;
                let space = super::process::pin_definition(source).map_err(Error::Process)?;
                let payload=interrupts::with_masked(||self.payload.lock().as_ref().cloned()).ok_or(Error::State)?;
                let region =
                    super::user::with_metadata(&space, || {
                        super::user::Mapping::definition(&space,
                                                         payload,
                                                         source.retain()?,
                                                         self.user_spans()
                                                             .map_err(|e| e.status())?)
                    }).map_err(|status| {
                          Error::Process(super::process::Error::DefinitionVisibility { status })
                      })?;
                let old = interrupts::with_masked(|| self.user_mapping.lock().replace(region));
                let occupied = old.is_some();
                drop(old);
                if occupied
                {
                    return Err(Error::State);
                }
                Ok(())
            })();
            if let Err(cause) = prepared
            {
                self.set_phase(Phase::Prepared);
                return Err(cause);
            }
        }
        if self.phase() == Phase::Preparing
        {
            self.user_turn(|mapping| mapping.publish())?;
            self.set_phase(Phase::Mapped);
        }
        if self.phase() != Phase::Mapped
        {
            return Err(Error::State);
        }
        crate::machine::monitor::publish_instructions().map_err(Error::Monitor)?;
        self.set_phase(Phase::Live);
        Ok(())
    }

    fn retire_user_definition(&self) -> Result<(), Error>
    {
        let _turn = self.user_operation()?;
        if self.phase() == Phase::Settled
        {
            return Ok(());
        }
        if matches!(self.phase(), Phase::Quarantined(_) | Phase::Cleared)
        {
            return Err(Error::State);
        }
        let exists = interrupts::with_masked(|| self.user_mapping.lock().is_some());
        if exists
        {
            self.user_turn(|mapping| mapping.retire())?;
        }
        let payload = interrupts::with_masked(|| self.payload.lock().take());
        drop(payload);
        self.settle_phase(Phase::Settled);
        crate::toolbox::processes::retirement::resources_released();
        Ok(())
    }
}

struct UserOperation<'a>(&'a Mapping);
impl Drop for UserOperation<'_>
{
    fn drop(&mut self)
    {
        let wake = self.0.with_state(|state| {
                             state.user_running = false;
                             core::mem::take(&mut state.user_waiting)
                         });
        if wake
        {
            changed();
            crate::toolbox::processes::retirement::resources_released();
        }
    }
}
impl Mapping
{
    fn user_operation(&self) -> Result<UserOperation<'_>, Error>
    {
        self.with_state(|state| {
                if state.user_running
                {
                    state.user_waiting = true;
                    return Err(Error::Process(super::process::Error::Busy));
                }
                state.user_running = true;
                Ok(UserOperation(self))
            })
    }
}

/// Preserve the actual native monitor variant before a portable boundary status
/// is returned. This is shared with owning User mapping retirement.
pub(super) fn monitor_cause(base: u64, stage: Stage, error: &crate::machine::monitor::Error)
                            -> CleanupCause
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
    CleanupCause::Monitor { base,
                            stage,
                            cause }
}

impl Mapping
{
    /// Numeric supervisor alias only while caller retains the genuine image
    /// execution/read owner; no temporary Span Vec is required for WAIT32.
    pub(crate) fn user_alias(&self, address: usize, bytes: usize, write: bool)
                             -> Result<usize, Error>
    {
        if !matches!(self.target, TargetKind::Application | TargetKind::UserDefinition)
           || self.phase() != Phase::Live
        {
            return Err(Error::State);
        }
        let end = address.checked_add(bytes).ok_or(Error::State)?;
        let (index, map) = self.segments
                               .iter()
                               .enumerate()
                               .find(|(_, map)| {
                                   address as u64 >= map.virtual_base
                                   && end as u64 <= map.virtual_base + map.bytes
                                   && map.flags & image::READ != 0
                                   && (!write || map.flags & image::WRITE != 0)
                               })
                               .ok_or(Error::Owner)?;
        let payload =
            interrupts::with_masked(|| self.payload.lock().as_ref().cloned()).ok_or(Error::State)?;
        let backing = &payload.segments.get(index).ok_or(Error::State)?.backing;
        backing.cpu_base().checked_add(address - map.virtual_base as usize).ok_or(Error::State)
    }
}

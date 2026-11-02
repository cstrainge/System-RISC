//! Canonical, privately owned supervisor process roots. This is not U-mode
//! isolation: payload pointers and the service ABI retain their trusted contract.
use alloc::{sync::Arc, vec::Vec};
use core::{fmt,
           sync::atomic::{AtomicBool, Ordering}};

use mac_kernel_model::pages::{AddressSpace, AsidTag, PhysicalAddress};
use mac_object_table::{domain::{DomainId, DomainKind},
                       store::PreparedError};
use mac_riscv_pages::{Decoded, Entry, LeafSize, mapping::Tables};
use spinning_top::Spinlock;

use super::*;
use crate::{machine::{interrupts, scheduler::accounting::AllocationAuthority},
            toolbox::handles::{InvocationGate, OwnerRef}};
pub(crate) mod lifetime;
type Store = mac_object_table::store::Store<Arc<Root>, InvocationGate>;
static MANAGER: spin::Once<Spinlock<Store>> = spin::Once::new();
static ROOTS: Spinlock<Option<Arc<Root>>> = Spinlock::new(None);
static WAITING_PINS: AtomicBool = AtomicBool::new(false);
static WAITING_MONITOR: AtomicBool = AtomicBool::new(false);
static RETIREMENT_PENDING: AtomicBool = AtomicBool::new(false);
pub(super) fn monitor_released()
{
    if WAITING_MONITOR.swap(false, Ordering::AcqRel)
    {
        retirement_changed();
        crate::toolbox::processes::retirement::resources_released();
    }
}
fn retirement_changed()
{
    RETIREMENT_PENDING.store(true, Ordering::Release);
    crate::machine::scheduler::wake::request();
}
pub(crate) fn take_retirement_pending() -> bool
{
    RETIREMENT_PENDING.swap(false, Ordering::AcqRel)
}

#[derive(Debug)]
pub(crate) enum Error
{
    Missing,
    Authority,
    BackingOrigin,
    Metadata,
    Workspace(super::modules::WorkspaceError),
    CloneInventory(super::modules::CloneInventoryError),
    CloneFootprint
    {
        expected: usize,
        observed: usize,
        revision: u64
    },
    Template
    {
        physical: u64
    },
    DefinitionVisibility
    {
        status: mac_abi::Status
    },
    Threads,
    Readers(usize),
    Pending,
    Busy,
    Closed,
    Tables(super::Error),
    Encoding(mac_riscv_pages::Error),
    Publication(PreparedError),
    Pool(mac_object_table::domain::PoolError),
    Monitor(crate::machine::monitor::Error),
    Retirement(CleanupCause)
}
impl Error
{
    pub(crate) fn status(&self) -> mac_abi::Status
    {
        match self
        {
            Self::Metadata => mac_abi::NO_MEMORY,
            Self::Workspace(cause) => cause.status(),
            Self::DefinitionVisibility { status } => *status,
            Self::Authority | Self::BackingOrigin => mac_abi::ACCESS_DENIED,
            Self::Threads | Self::Readers(_) | Self::Pending | Self::Busy => mac_abi::SERVICE_BUSY,
            Self::Closed => mac_abi::OPERATION_CANCELLED,
            Self::Monitor(e) => e.status(),
            Self::Retirement(cause)=>cause.status(),
            Self::Pool(cause) | Self::Publication(PreparedError::Pool(cause)) =>crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(*cause)),
            Self::Tables(super::Error::Tables(mac_riscv_pages::mapping::MapError::Storage(cause))) => cause.status(),
            _ => mac_abi::BAD_ABI
        }
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Missing => f.write_str("Process space manager/root is unavailable"),
            Self::Authority =>
            {
                f.write_str("Process space does not match genuine Process/image/account")
            }
            Self::BackingOrigin =>
            {
                f.write_str("protected Process space admission cannot reuse a root from a \
                             different backing origin; original root retained")
            }
            Self::Metadata => f.write_str("Process page-table metadata admission refused"),
            Self::Workspace(cause) => cause.fmt(f),
            Self::CloneInventory(cause) => cause.fmt(f),
            Self::CloneFootprint { expected,
                                   observed,
                                   revision } =>
            {
                write!(f,
                       "Process clone exceeded System-owned table inventory at mapping revision \
                        {revision}: expected at most {expected} tables, observed {observed}; \
                        unpublished clone discarded")
            }
            Self::DefinitionVisibility { status } =>
            {
                write!(f,
                       "definition range visibility unavailable: status {status}; no Process root \
                        published")
            }
            Self::Template { physical } =>
            {
                write!(f,
                       "System template table {physical:#x} is not retained by its actual native \
                        owner")
            }
            Self::Threads => f.write_str("Process space awaits actual THRD/claim quiescence"),
            Self::Readers(n) => write!(f, "Process space retains {n} execution/publication pins"),
            Self::Pending =>
            {
                f.write_str("Process space awaits guard-free quiescence, translation ACK and \
                             physical disposal; original ticket retained")
            }
            Self::Busy =>
            {
                f.write_str("Process space is exclusively claimed for retirement; retry after \
                             actual progress")
            }
            Self::Closed => f.write_str("Process space admission is closed"),
            Self::Tables(e) => e.fmt(f),
            Self::Encoding(e) => e.fmt(f),
            Self::Publication(e) => write!(f, "Process space publication: {e:?}"),
            Self::Pool(e) => e.fmt(f),
            Self::Monitor(e) => e.fmt(f),
            Self::Retirement(cause) => cause.fmt(f)
        }
    }
}
/// Copyable bounded cause retained with the canonical cleanup ticket. The
/// full typed preparation error remains available before any publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CleanupCause
{
    Unavailable,
    Contract,
    Threads,
    Readers(usize),
    Pending,
    Monitor
    {
        status:   mac_abi::Status,
        hart:     Option<u64>,
        sequence: Option<u64>
    }
}
impl From<Error> for CleanupCause
{
    fn from(cause: Error) -> Self
    {
        match cause
        {
            Error::Retirement(cause) => cause,
            Error::Missing => Self::Unavailable,
            Error::Threads => Self::Threads,
            Error::Readers(n) => Self::Readers(n),
            Error::Pending | Error::Busy => Self::Pending,
            Error::Monitor(cause) =>
            {
                let (hart, sequence) = match &cause
                {
                    crate::machine::monitor::Error::Uncertain { hart,
                                                                sequence } =>
                    {
                        (Some(*hart), Some(*sequence))
                    }
                    crate::machine::monitor::Error::Refused { hart,
                                                              .. } => (Some(*hart), None),
                    _ => (None, None)
                };
                Self::Monitor { status: cause.status(),
                                hart,
                                sequence }
            }
            _ => Self::Contract
        }
    }
}
impl CleanupCause
{
    pub(crate) fn status(self) -> mac_abi::Status
    {
        match self
        {
            Self::Unavailable => mac_abi::NOT_SUPPORTED,
            Self::Contract => mac_abi::BAD_ABI,
            Self::Threads | Self::Readers(_) | Self::Pending => mac_abi::SERVICE_BUSY,
            Self::Monitor { status,
                            .. } => status
        }
    }
}
impl fmt::Display for CleanupCause
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Unavailable =>
            {
                f.write_str("Process space manager unavailable; original ticket retained")
            }
            Self::Contract =>
            {
                f.write_str("Process space canonical cleanup binding mismatch; original ticket \
                             retained")
            }
            Self::Threads => f.write_str("Process space awaits actual THRD/claim quiescence"),
            Self::Readers(n) =>
            {
                write!(f, "Process space retains {n} actual execution/publication pins")
            }
            Self::Pending =>
            {
                f.write_str("Process space awaits guard-free quiescence, translation ACK and \
                             physical disposal; original ticket retained")
            }
            Self::Monitor { status,
                            hart,
                            sequence } =>
            {
                write!(f,
                       "Process space translation invalidation refused: status={status} \
                        hart={hart:?} sequence={sequence:?}; original root/ticket retained")
            }
        }
    }
}
/// All physical owners precede the original account capability. Registry links
/// are detached before disposal; neither Process nor IMAG holds this Arc.
pub(crate) struct Root
{
    backing:               Spinlock<Option<Arc<Space>>>,
    register:              usize,
    pub(super) owner:      OwnerRef,
    pub(super) allocation: AllocationAuthority,
    image:                 crate::images::Binding,
    next:                  Spinlock<Option<Arc<Root>>>,
    closing:               AtomicBool,
    registered:            AtomicBool,
    retired:               AtomicBool,
    retirement_requested:  AtomicBool,
    retirement_claimed:    AtomicBool,
    retirement_failure:    Spinlock<Option<CleanupCause>>
}
impl Root
{
    pub(crate) fn owner(&self) -> DomainId
    {
        self.owner.owner()
    }

    pub(crate) fn register(&self) -> usize
    {
        self.register
    }

    pub(crate) fn matches(&self, owner: &OwnerRef, image: crate::images::Binding) -> bool
    {
        self.owner.owner() == owner.owner() && self.image == image
    }
}
/// One real execution/preparation/table-publication pin. Its marker runs
/// AFTER the Arc is released; Store plus registry are two backing owners, so
/// the generic one-backing-owner retained::Read checkpoint does not apply.
pub(crate) struct Pin
{
    // Must release Space first, then Root, then the actual progress marker.
    space:     Arc<Space>,
    root:      Arc<Root>,
    _released: PinReleased
}
struct PinReleased;
impl Drop for PinReleased
{
    fn drop(&mut self)
    {
        if WAITING_PINS.swap(false, Ordering::AcqRel)
        {
            retirement_changed();
            crate::toolbox::processes::retirement::resources_released();
        }
    }
}
impl Pin
{
    fn try_new(root: Arc<Root>, retirement_snapshot: bool) -> Result<Self, Error>
    {
        let space = interrupts::with_masked(|| {
            let backing = root.backing.lock();
            if root.retirement_claimed.load(Ordering::Acquire)
            {
                return Err(Error::Busy);
            }
            if !retirement_snapshot && root.closing.load(Ordering::Acquire)
            {
                return Err(Error::Closed);
            }
            backing.as_ref().cloned().ok_or(Error::Closed)
        });
        match space
        {
            Ok(space) =>
            {
                Ok(Self { space,
                          root,
                          _released: PinReleased })
            }
            Err(cause) =>
            {
                // This actual temporary Arc may have blocked a concurrent
                // claim. Notify only after it is really released.
                drop(root);
                drop(PinReleased);
                Err(cause)
            }
        }
    }

    pub(super) fn space(&self) -> &Space
    {
        &self.space
    }
}
impl Clone for Pin
{
    fn clone(&self) -> Self
    {
        Self { space:     self.space.clone(),
               root:      self.root.clone(),
               _released: PinReleased }
    }
}
impl core::ops::Deref for Pin
{
    type Target = Root;

    fn deref(&self) -> &Root
    {
        &self.root
    }
}
/// Count real linked roots, including closing owners, without pin/allocation.
/// Callers hold the actual translation lease across count and effects.
pub(super) fn registered_count(owner: Option<DomainId>) -> Result<usize, Error>
{
    interrupts::with_masked(|| {
        let head = ROOTS.lock();
        let mut row = head.as_ref().cloned();
        let mut count = 0usize;
        while let Some(root) = row
        {
            row = root.next.lock().as_ref().cloned();
            if owner.is_none_or(|owner| root.owner() == owner)
            {
                count = count.checked_add(1).ok_or(Error::Metadata)?;
            }
        }
        Ok(count)
    })
}

/// Use already admitted retirement capacity. Filtering before insertion avoids
/// charging a private Definition for unrelated roots. Pins remain in the caller
/// through the actual translation finish; partial failure never drops them here.
pub(super) fn snapshot_into(owner: Option<DomainId>, rows: &mut Vec<Pin>) -> Result<(), Error>
{
    let required = registered_count(owner)?;
    if !rows.is_empty() || required > rows.capacity()
    {
        return Err(Error::Workspace(super::modules::WorkspaceError::Invariant {
            part: "root pins", required: required.max(rows.len()), capacity: rows.capacity()
        }));
    }
    let mut row = interrupts::with_masked(|| ROOTS.lock().as_ref().cloned());
    while let Some(root) = row
    {
        // The caller's actual translation lease excludes link/unlink. Clone
        // the next canonical owner under its short link guard, then admit the
        // Pin outside ROOTS so even refusal's release notification is unguarded.
        row = interrupts::with_masked(|| root.next.lock().as_ref().cloned());
        if owner.is_none_or(|owner| root.owner() == owner)
        {
            if rows.len() == rows.capacity()
            {
                return Err(Error::Workspace(super::modules::WorkspaceError::Invariant {
                    part: "root pins", required: rows.len().saturating_add(1), capacity: rows.capacity()
                }));
            }
            rows.push(Pin::try_new(root, true)?);
        }
    }
    Ok(())
}
/// A snapshot pins every table owner through the real command ACK. Capacity is
/// prepared outside ROOTS; the locked turn only clones into admitted capacity.
pub(super) fn snapshot(publication: bool) -> Result<Vec<Pin>, Error>
{
    let mut rows = Vec::new();
    loop
    {
        let count = interrupts::with_masked(|| {
            let head = ROOTS.lock();
            let mut row = head.as_ref().cloned();
            let mut n = 0;
            while let Some(r) = row
            {
                n += 1;
                row = r.next.lock().as_ref().cloned();
            }
            n
        });
        rows.try_reserve(count).map_err(|_| Error::Metadata)?;
        let complete = interrupts::with_masked(|| {
            let head = ROOTS.lock();
            let mut row = head.as_ref().cloned();
            while let Some(r) = row
            {
                row = r.next.lock().as_ref().cloned();
                if !publication || (r.owner.accepting() && !r.closing.load(Ordering::Acquire))
                {
                    if rows.len() == rows.capacity()
                    {
                        return Ok(false);
                    }
                    rows.push(Pin::try_new(r, !publication)?);
                }
            }
            Ok::<_, Error>(true)
        })?;
        if complete
        {
            return Ok(rows);
        }
        rows.clear();
    }
}
fn link(root: &Arc<Root>)
{
    interrupts::with_masked(|| {
        let mut head = ROOTS.lock();
        *root.next.lock() = head.take();
        *head = Some(root.clone());
        root.registered.store(true, Ordering::Release);
    });
}
fn unlink(root: &Arc<Root>) -> Result<(), Error>
{
    // Moving links never drops a Root or backing while ROOTS is held.
    let removed = interrupts::with_masked(|| {
        let mut head = ROOTS.lock();
        if head.as_ref().is_some_and(|r| Arc::ptr_eq(r, root))
        {
            let old = head.take();
            *head = root.next.lock().take();
            return old;
        }
        let mut previous = head.as_ref().cloned();
        while let Some(r) = previous
        {
            let mut next = r.next.lock();
            if next.as_ref().is_some_and(|n| Arc::ptr_eq(n, root))
            {
                let old = next.take();
                *next = root.next.lock().take();
                return old;
            }
            previous = next.as_ref().cloned();
        }
        None
    });
    if removed.is_none()
    {
        return Err(Error::Authority);
    }
    root.registered.store(false, Ordering::Release);
    drop(removed);
    Ok(())
}

pub(crate) fn initialize() -> Result<(), Error>
{
    if !crate::context::current_thread_context().is_bootstrap() || MANAGER.get().is_some()
    {
        return Err(Error::Authority);
    }
    let kinds=crate::toolbox::handles::registry(|r|Ok::<_,mac_object_table::registry::Error>((r.get(lifetime::Kind::Root.specification().kind)?,r.get(lifetime::Kind::Storage.specification().kind)?))).map_err(|_|Error::Missing)?.map_err(|_|Error::Missing)?;
    let store = crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
                    Store::owned_prepared(kinds.0, kinds.1)
                }).map_err(Error::Pool)?;
    MANAGER.call_once(|| Spinlock::new(store));
    Ok(())
}
fn permit(owner: &OwnerRef)
          -> Result<mac_object_table::store::PublicationPermit<InvocationGate>, Error>
{
    let manager = MANAGER.get().ok_or(Error::Missing)?;
    loop
    {
        let plan=interrupts::with_masked(||manager.lock().plan_publication(owner)).map_err(Error::Pool)?;
        let prepared = plan.prepare().map_err(Error::Pool)?;
        match interrupts::with_masked(|| manager.lock().admit_prepared(prepared))
        {
            Ok((permit, old)) =>
            {
                old.dispose();
                return Ok(permit);
            }
            Err(failure) =>
            {
                let cause = failure.cause;
                drop(failure.preparation);
                if cause != PreparedError::CapacityChanged
                {
                    return Err(Error::Publication(cause));
                }
            }
        }
    }
}
/// The same real owner/image admitted to the initial THRD supplies this root.
/// Publication precedes THRD publication, so every failure remains owner-walkable.
pub(crate) fn admit(owner: &OwnerRef, allocation: &AllocationAuthority,
                    image: &crate::kernel::LoadedImage)
                    -> Result<Pin, Error>
{
    admit_with_source(owner, allocation, image, BackingSource::Ordinary)
}
/// The recovery scheduler supplies its actual retained kit capability. This
/// selects backing origin, never a synthetic Process or larger account budget.
pub(crate) fn admit_recovery(owner: &OwnerRef, allocation: &AllocationAuthority,
                             image: &crate::kernel::LoadedImage,
                             permit: crate::allocator::recovery::Permit)
                             -> Result<Pin, Error>
{
    admit_with_source(owner, allocation, image, BackingSource::Protected(permit))
}
fn admit_with_source(owner: &OwnerRef, allocation: &AllocationAuthority,
                     image: &crate::kernel::LoadedImage, source: BackingSource)
                     -> Result<Pin, Error>
{
    if owner.owner().kind() != DomainKind::Process
       || allocation.owner() != owner.owner()
       || image.binding().owner() != owner.owner().incarnation()
    {
        return Err(Error::Authority);
    }
    if !owner.accepting()
    {
        return Err(Error::Closed);
    }
    let manager = MANAGER.get().ok_or(Error::Missing)?;
    let existing = interrupts::with_masked(|| {
        let store = manager.lock();
        store.retained_values()
             .find(|root| root.owner.owner() == owner.owner())
             .map(|root| {
                 if root.closing.load(Ordering::Acquire)
                    || root.retirement_claimed.load(Ordering::Acquire)
                 {
                     return Err(Error::Closed);
                 }
                 Ok(root.clone())
             })
             .transpose()
    })?;
    if let Some(root) = existing
    {
        let root = Pin::try_new(root, false)?;
        if !root.matches(owner, image.binding())
        {
            return Err(Error::Authority);
        }
        if !source.accepts_existing(&root.space().tables.source)
        {
            return Err(Error::BackingOrigin);
        }
        return if root.registered.load(Ordering::Acquire) && !root.closing.load(Ordering::Acquire)
        {
            Ok(root)
        }
        else
        {
            Err(Error::Closed)
        };
    }
    let target = crate::allocator::Target::Owner { account: allocation.clone(),
                                                   class:
                                                       mac_kernel_model::policy::MemoryClass::Kernel };
    source.with_metadata(|| {
              crate::allocator::with_target(target, || {
                  let permit = permit(owner)?;
                  let lease = crate::machine::monitor::prepare_translation_invalidation()
                .map_err(Error::Monitor)?;
                  let space = clone_space(owner, allocation, image.range(), source.clone(), &lease)?;
                  let register = space.register;
                  // Move the sole backing Arc into canonical Root. No unobserved local
                  // Space clone survives publication or a failed execution-pin admission.
                  let space = Arc::try_new(space).map_err(|_| Error::Metadata)?;
                  let root = Arc::try_new(Root {
                backing: Spinlock::new(Some(space)),
                register,
                owner: owner.clone(),
                allocation: allocation.clone(),
                image: image.binding(),
                next: Spinlock::new(None),
                closing: AtomicBool::new(false),
                registered: AtomicBool::new(false),
                retired: AtomicBool::new(false),
                retirement_requested: AtomicBool::new(false),
                retirement_claimed: AtomicBool::new(false),
                retirement_failure: Spinlock::new(None)
            }).map_err(|_| Error::Metadata)?;
                  match interrupts::with_masked(|| {
                            manager.lock().publish_prepared(permit, root.clone())
                        })
                  {
                      Ok((publication, old)) =>
                      {
                          old.dispose();
                          let _ = publication.into_parts();
                          link(&root);
                          drop(lease);
                          Pin::try_new(root, false)
                      }
                      Err(failure) =>
                      {
                          let cause = failure.cause;
                          drop(failure);
                          Err(Error::Publication(cause))
                      }
                  }
              })
          })
}

fn clone_space(owner: &OwnerRef, allocation: &AllocationAuthority, image: core::ops::Range<u64>,
               source: BackingSource, lease: &crate::machine::monitor::TranslationLease)
               -> Result<Space, Error>
{
    let spaces = SPACES.get().ok_or(Error::Missing)?;
    let mut tables = Frames { head:    0,
                              count:   0,
                              account: Some(allocation.clone()),
                              source };
    let source = spaces.kernel.owner.root_physical();
    // admit retains the real translation lease across inventory, copying and
    // publication. No source hierarchy growth can invalidate this bound.
    let footprint = modules::clone_footprint().map_err(Error::CloneInventory)?;
    let mut definitions = crate::toolbox::resources::definitions::visibility_snapshot()
        .map_err(|cause| Error::DefinitionVisibility { status: cause.status() })?;
    definitions.sort_unstable_by_key(|range| range.base);
    let private_pages = definitions.iter()
                                   .filter(|range| range.owner == owner.owner())
                                   .map(|range| range.retirement_pages)
                                   .max()
                                   .unwrap_or(0);
    let registered = registered_count(None)?.checked_add(1).ok_or(Error::Metadata)?;
    let eligible = registered_count(Some(owner.owner()))?.checked_add(1).ok_or(Error::Metadata)?;
    // The same lease excludes source PTE changes and root link/unlink until the
    // new root is actually published/linked. No stale clone can escape this gate.
    modules::admit_process_workspace(lease, registered, private_pages, eligible)
        .map_err(Error::Workspace)?;
    let physical = copy_table(&mut tables, source, 2, 0, &image, owner.owner(), &definitions)?;
    if tables.count > footprint.tables
    {
        // Frames still owns every table and original charge. Refusal drops the
        // unpublished clone; no root extraction, linking or CPU activation ran.
        return Err(Error::CloneFootprint { expected: footprint.tables,
                                           observed: tables.count,
                                           revision: footprint.revision });
    }
    let (root, charge) = tables.take_root(physical).map_err(Error::Tables)?;
    let owner=AddressSpace::<Rva, PagingBacking>::reserve(owner.owner(),root,AsidTag::Untagged).map_err(|e|Error::Tables(super::Error::Policy(e)))?;
    let register = MODE | (physical.0 as usize >> 12);
    Ok(Space { owner,
               tables,
               register,
               leaves: 0,
               _root_charge: charge })
}
fn keep_low(base: u64, end: u64, image: &core::ops::Range<u64>, overlap: bool) -> bool
{
    let keep = |start: u64, limit: u64| {
        if overlap { base < limit && start < end } else { base >= start && end <= limit }
    };
    keep(image.start, image.end)
    || keep(DEFINITION_BASE, DEFINITION_LIMIT)
    || BOOT_MMIO_REGIONS.iter().any(|&(start, limit)| keep(start, limit))
    || crate::machine::power::platform::PLATFORM_MMIO_DEVICES.iter()
                                                             .any(|&(_, _, start, bytes, _, _)| {
                                                                 keep(start, start + bytes)
                                                             })
    || BOOT.get().is_some_and(|b| {
                     b.regions.iter().any(|r| {
                                         r.flags & FIRMWARE_ONLY == 0
                                         && keep(r.virtual_base, r.virtual_base + r.physical.length)
                                     })
                 })
}

fn copy_table(tables: &mut Frames, source: PhysicalAddress, level: u8, prefix: u64,
              image: &core::ops::Range<u64>, owner: DomainId,
              definitions: &[crate::toolbox::resources::definitions::Range])
              -> Result<PhysicalAddress, Error>
{
    let target=tables.allocate_zeroed().map_err(|e|Error::Tables(super::Error::Tables(mac_riscv_pages::mapping::MapError::Storage(e))))?;
    let entries =
        modules::table_snapshot(source).map_err(|_| Error::Template { physical: source.0 })?;
    for (index, bits) in entries.into_iter().enumerate()
    {
        let address = if level == 2
        {
            let a = (index as u64) << 30;
            if a & (1 << 38) != 0 { a | (!0u64 << 39) } else { a }
        }
        else
        {
            prefix | ((index as u64) << (12 + 9 * level))
        };
        let entry = match Entry::decode_raw(bits, level).map_err(Error::Encoding)?
        {
            Decoded::Invalid => continue,
            Decoded::Table { physical,
                             global,
                             .. } =>
            {
                let end =
                    address.saturating_add(LeafSize::from_level(level).map_err(Error::Encoding)?
                                                                      .bytes());
                if address >= VM_APPLICATION_START
                   && end <= VM_APPLICATION_END
                   && !keep_low(address, end, image, true)
                {
                    continue;
                }
                let child =
                    copy_table(tables, physical, level - 1, address, image, owner, definitions)?;
                Entry::table(child,global,mac_kernel_model::pages::PageOwnership::Automatic).map_err(Error::Encoding)?
            }
            Decoded::Leaf { physical,
                            size,
                            permissions,
                            ownership } =>
            {
                let end = address.checked_add(size.bytes()).unwrap_or(u64::MAX);
                let low = address < VM_APPLICATION_END && end > VM_APPLICATION_START;
                if low && !keep_low(address, end, image, false)
                {
                    continue;
                }
                if address < DEFINITION_LIMIT && end > DEFINITION_BASE
                {
                    let at = definitions.partition_point(|range| range.base <= address);
                    let range = at.checked_sub(1)
                                  .and_then(|i| definitions.get(i))
                                  .filter(|range| end <= range.limit)
                                  .ok_or(Error::DefinitionVisibility { status: mac_abi::BAD_ABI })?;
                    if range.owner.kind() == DomainKind::Process && range.owner != owner
                    {
                        continue;
                    }
                }
                if permissions.access != mac_kernel_model::pages::Access::Supervisor
                {
                    return Err(Error::Authority);
                }
                Entry::leaf(physical, size, permissions, ownership).map_err(Error::Encoding)?
            }
        };
        tables.store(target,index,entry).map_err(|e|Error::Tables(super::Error::Tables(mac_riscv_pages::mapping::MapError::Storage(e))))?;
    }
    Ok(target)
}

/// A canonical claim contains no cleanup ticket. The original ticket remains
/// in the owner walk until this real physical operation is settled.
pub(crate) struct Retirement
{
    root:      Option<Arc<Root>>,
    owner:     u64,
    not_run:   bool,
    completed: bool
}
impl Retirement
{
    pub(crate) fn owner(&self) -> u64
    {
        self.owner
    }

    pub(crate) fn run(mut self) -> Result<(), Error>
    {
        self.not_run = false;
        let root = self.root.as_ref().ok_or(Error::Authority)?;
        if !root.closing.load(Ordering::Acquire)
           || !root.retirement_requested.load(Ordering::Acquire)
           || !root.retirement_claimed.load(Ordering::Acquire)
        {
            return Err(Error::Authority);
        }
        if let Some(cause) = interrupts::with_masked(|| *root.retirement_failure.lock())
        {
            return Err(Error::Retirement(cause));
        }
        WAITING_PINS.store(true, Ordering::Release);
        if interrupts::with_masked(|| crate::machine::scheduler::has_threads(self.owner))
        {
            return Err(Error::Threads);
        }
        // Preflight before taking a monitor lease. A retained reader must
        // wait for its real release, not repeatedly wake itself by dropping a
        // freshly acquired lease on an unchanged Readers refusal.
        let space = interrupts::with_masked(|| {
            let _registry = ROOTS.lock();
            let backing = root.backing.lock();
            let expected = if root.registered.load(Ordering::Acquire) { 3 } else { 2 };
            let count = Arc::strong_count(root);
            if count != expected
            {
                return Err(Error::Readers(count.saturating_sub(expected)));
            }
            let space = backing.as_ref().ok_or(Error::Authority)?;
            if Arc::strong_count(space) != 1
            {
                return Err(Error::Readers(Arc::strong_count(space).saturating_sub(1)));
            }
            Ok(space.clone())
        })?;
        WAITING_MONITOR.store(true, Ordering::Release);
        let lease = match crate::machine::monitor::prepare_translation_invalidation()
        {
            Ok(lease) => lease,
            Err(cause) =>
            {
                if matches!(cause, crate::machine::monitor::Error::Busy)
                {
                    return Err(Error::Monitor(cause));
                }
                return Err(quarantine(root, cause));
            }
        };
        // The actual lease excludes PTE publishers. Claimed state and the
        // same registry/backing gates used by pin admission forbid new clones
        // after the preflight, so validate both real owner counts/identity
        // again before registry effects rather than treating an observation
        // as a grant. Unexpected sealed-reference changes retain the root.
        interrupts::with_masked(|| {
            let _registry = ROOTS.lock();
            let backing = root.backing.lock();
            let expected = if root.registered.load(Ordering::Acquire) { 3 } else { 2 };
            let held = backing.as_ref().ok_or(Error::Authority)?;
            if Arc::strong_count(root) != expected
               || !Arc::ptr_eq(held, &space)
               || Arc::strong_count(held) != 2
            {
                return Err(Error::Authority);
            }
            Ok(())
        }).map_err(|cause| retain_failure(root, cause.into()))?;
        if let Err(cause) = unlink(root)
        {
            return Err(retain_failure(root, cause.into()));
        }
        match lease.finish()
        {
            Ok(receipt) => drop(receipt),
            Err(cause) => return Err(quarantine(root, cause))
        }
        modules::retire_process_branches(space.owner.root_physical());
        let backing = interrupts::with_masked(|| {
                          let mut backing = root.backing.lock();
                          let held = backing.as_ref().ok_or(Error::Authority)?;
                          if !Arc::ptr_eq(held, &space) || Arc::strong_count(held) != 2
                          {
                              return Err(Error::Authority);
                          }
                          backing.take().ok_or(Error::Authority)
                      }).map_err(|cause| retain_failure(root, cause.into()))?;
        // No external Arc/Weak escape exists. New pins are gated out; these
        // are the two verified actual owning references. Synchronous Drop of
        // BOTH physical owners must finish before reporting retired.
        drop(space);
        drop(backing);
        root.retired.store(true, Ordering::Release);
        self.completed = true;
        Ok(())
    }
}
impl Drop for Retirement
{
    fn drop(&mut self)
    {
        if let Some(root) = self.root.take()
        {
            root.retirement_claimed.store(false, Ordering::Release);
            drop(root);
        }
        if self.not_run
        {
            retirement_changed();
        }
        if self.completed
        {
            // One physical root is the maintenance unit. Its real completion
            // admits a later checkpoint for another ready root (or one empty
            // followup), rather than extending this input-service interval.
            retirement_changed();
            modules::changed();
        }
        // A consumed Busy waits for actual Pin/monitor release. Its own
        // temporary Arc is not a new root-pending event.
        crate::toolbox::processes::retirement::resources_released();
    }
}
pub(crate) fn retirement() -> Option<Retirement>
{
    let manager = MANAGER.get()?;
    interrupts::with_masked(|| {
        let store = manager.lock();
        let _registry = ROOTS.lock();
        let root =
            store.retained_values()
                 .filter(|root| {
                     if !root.closing.load(Ordering::Acquire)
                        || !root.retirement_requested.load(Ordering::Acquire)
                        || root.retired.load(Ordering::Acquire)
                        || root.retirement_claimed.load(Ordering::Acquire)
                        || root.retirement_failure.lock().is_some()
                     {
                         return false;
                     }
                     // Readiness is observed before taking any command lease. Arm
                     // genuine pin releases before checking; skip each blocked row so
                     // one old owner cannot starve another ready canonical root.
                     WAITING_PINS.store(true, Ordering::Release);
                     if crate::machine::scheduler::has_threads(root.owner.owner().incarnation())
                     {
                         return false;
                     }
                     let expected = if root.registered.load(Ordering::Acquire) { 2 } else { 1 };
                     if Arc::strong_count(root) != expected
                     {
                         return false;
                     }
                     let backing = root.backing.lock();
                     backing.as_ref().is_some_and(|space| Arc::strong_count(space) == 1)
                 })
                 .min_by_key(|root| root.owner.owner().incarnation())?;
        let _backing = root.backing.lock();
        root.retirement_claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some(Retirement { root:      Some(root.clone()),
                          owner:     root.owner.owner().incarnation(),
                          not_run:   true,
                          completed: false })
    })
}
fn quarantine(root: &Root, cause: crate::machine::monitor::Error) -> Error
{
    retain_failure(root, CleanupCause::from(Error::Monitor(cause)))
}
fn retain_failure(root: &Root, cause: CleanupCause) -> Error
{
    let retained = interrupts::with_masked(|| {
        let mut held = root.retirement_failure.lock();
        *held.get_or_insert(cause)
    });
    Error::Retirement(retained)
}

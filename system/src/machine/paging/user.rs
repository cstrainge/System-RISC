//! Process-relative user mappings and checked copies. Physical owners, canonical
//! PTE transactions and actual translation ACK remain separate authorities.
#![allow(unsafe_code)]
use alloc::{sync::{Arc, Weak},
            vec::Vec};
use core::{fmt,
           sync::atomic::{AtomicBool, AtomicUsize, Ordering}};

use mac_abi::{ACCESS_DENIED, BAD_ABI, BAD_ARGUMENT, NO_MEMORY, SERVICE_BUSY, Status};
use mac_formats::native::image;
use mac_kernel_model::pages::PhysicalAddress;
use spinning_top::Spinlock;

use super::{modules, process};
use crate::machine::{interrupts, memory::*};

/// Native lifecycle errors keep their original cause. Status is only the wire
/// projection, never a source for reconstructing monitor/PTE failure identity.
#[derive(Debug)]
pub(crate) enum Error
{
    Rejected(Status),
    Readers(usize),
    Monitor
    {
        stage: modules::Stage,
        cause: crate::machine::monitor::Error
    },
    Paging(modules::Error),
    Quarantined(modules::CleanupCause)
}
impl Error
{
    pub(crate) fn status(&self) -> Status
    {
        match self
        {
            Self::Rejected(status) => *status,
            Self::Readers(_) => SERVICE_BUSY,
            Self::Monitor { cause,
                            .. } => cause.status(),
            Self::Paging(cause) => cause.status(),
            Self::Quarantined(_) => BAD_ABI
        }
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Rejected(status) => write!(out, "User mapping admission refused: {status}"),
            Self::Readers(count) => write!(out, "User mapping retains {count} actual reader pins"),
            Self::Monitor { stage,
                            cause } => write!(out, "User mapping {stage:?}: {cause}"),
            Self::Paging(cause) => cause.fmt(out),
            Self::Quarantined(cause) => cause.fmt(out)
        }
    }
}

pub(super) fn with_metadata<R>(space: &process::Pin, operation: impl FnOnce() -> R) -> R
{
    let target = crate::allocator::Target::Owner { account: space.allocation.clone(),
                                                   class:
                                                       mac_kernel_model::policy::MemoryClass::Kernel };
    let source = crate::context::recovery_allocation();
    match source
    {
        Some(permit) =>
        {
            crate::context::with_recovery_allocation(permit, || {
                crate::allocator::with_target(target, operation)
            })
        }
        None =>
        {
            crate::context::without_recovery(|| crate::allocator::with_target(target, operation))
        }
    }
}

/// # Safety
/// Implementors retain the real original physical lease AND its charge, expose
/// only its stable supervisor alias, and keep the complete span initialized.
/// The reported span cannot be resized, freed or reused until the final Arc.
pub(crate) unsafe trait Backing: Send + Sync
{
    fn owner(&self) -> mac_object_table::domain::DomainId;
    fn cpu_base(&self) -> usize;
    fn physical(&self) -> PhysicalAddress;
    fn bytes(&self) -> usize;
    fn heap_progress(&self) -> Option<crate::toolbox::application_heap::wait::Progress>
    {
        None
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Access
{
    Read,
    ReadWrite,
    Execute
}
impl Access
{
    fn flags(self) -> u32
    {
        match self
        {
            Self::Read => image::READ,
            Self::ReadWrite => image::READ | image::WRITE,
            Self::Execute => image::READ | image::EXECUTE
        }
    }

    fn permits(self, flags: u32) -> bool
    {
        flags & self.flags() == self.flags()
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind
{
    Heap,
    Stack,
    Descriptor
}
#[derive(Clone, Copy)]
pub(crate) struct Span
{
    pub(crate) address:  usize,
    pub(crate) cpu:      usize,
    pub(crate) physical: PhysicalAddress,
    pub(crate) bytes:    usize,
    pub(crate) flags:    u32
}
impl Span
{
    fn end(&self) -> Option<usize>
    {
        self.address.checked_add(self.bytes)
    }

    fn segment(&self) -> modules::SegmentMap
    {
        modules::SegmentMap { virtual_base: self.address as u64,
                              physical:     self.physical.0,
                              bytes:        self.bytes as u64,
                              memory:       self.bytes as u64,
                              flags:        self.flags }
    }
}
enum Owner
{
    Pages
    {
        _backing: Arc<dyn Backing>
    },
    Image
    {
        _source: crate::images::Read
    },
    Definition
    {
        _payload: Arc<modules::Payload>, _source: crate::images::Read
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase
{
    Prepared,
    Mapped,
    Acknowledged,
    Live,
    Cleared,
    Retired,
    Quarantined
}
struct State
{
    phase:   Phase,
    entries: Option<modules::UserEntries>,
    busy:    bool,
    failure: Option<modules::CleanupCause>
}
struct Readers
{
    count:   usize,
    waiting: bool
}
struct Signal
{
    readers:       Spinlock<Readers>,
    retiring:      AtomicBool,
    heap_progress: Option<crate::toolbox::application_heap::wait::Progress>
}
pub(super) struct Region
{
    spans:           Vec<Span>,
    reserved:        core::ops::Range<usize>,
    owner:           Spinlock<Option<Owner>>,
    // Image regions are owned by Root itself, so contain no Process pin.
    process:         Option<process::Pin>,
    closing:         AtomicBool,
    signal:          Arc<Signal>,
    state:           Spinlock<State>,
    quarantine_next: Spinlock<Option<Arc<Region>>>
}
/// The quarantine owns the actual payload and Process pin, allocation-free.
/// It never acknowledges an uncertain translation command or frees its frames.
static QUARANTINE: Spinlock<Option<Arc<Region>>> = Spinlock::new(None);
static PENDING: Spinlock<Option<Arc<Region>>> = Spinlock::new(None);
static ORPHANS: AtomicUsize = AtomicUsize::new(0);
static RETIRING: AtomicUsize = AtomicUsize::new(0);
static REAP_REQUESTED: AtomicBool = AtomicBool::new(false);
static REAPING: AtomicBool = AtomicBool::new(false);
struct Record
{
    region:   Weak<Region>,
    reserved: core::ops::Range<usize>
}
pub(super) struct Registry
{
    image:   Arc<Region>,
    dynamic: Vec<Record>
}
impl Registry
{
    pub(super) fn image(read: crate::images::Read, reservation: core::ops::Range<u64>)
                        -> Result<Self, Status>
    {
        let mut spans = read.user_spans()?;
        spans.sort_unstable_by_key(|span| span.address);
        if spans.is_empty()
        {
            return Err(BAD_ABI);
        }
        let region=Arc::try_new(Region {spans,
            reserved:reservation.start as usize..reservation.end as usize,
            owner:Spinlock::new(Some(Owner::Image { _source: read })), process:None,
            closing:AtomicBool::new(false),signal:Arc::try_new(Signal{readers:Spinlock::new(Readers{count:0,waiting:false}),retiring:AtomicBool::new(false),heap_progress:None}).map_err(|_|NO_MEMORY)?,
            state:Spinlock::new(State{phase:Phase::Prepared,entries:None,busy:false,failure:None}),
            quarantine_next:Spinlock::new(None)}).map_err(|_|NO_MEMORY)?;
        Ok(Self { image:   region,
                  dynamic: Vec::new() })
    }

    fn reader(&self, address: usize) -> Option<Arc<Region>>
    {
        if self.image.reserved.contains(&address)
        {
            return Some(self.image.clone());
        }
        let at = self.dynamic.partition_point(|r| r.reserved.start <= address);
        let record = at.checked_sub(1).and_then(|at| self.dynamic.get(at))?;
        if !record.reserved.contains(&address)
        {
            return None;
        }
        record.region.upgrade()
    }

    pub(super) fn image_region(&self) -> Arc<Region>
    {
        self.image.clone()
    }
}
pub(super) fn image_acknowledged(region: &Region) -> bool
{
    phase(region) == Phase::Acknowledged
}
pub(super) fn confirm_image(region: &Region)
{
    interrupts::with_masked(|| region.state.lock().phase = Phase::Live);
}
pub(super) fn acknowledge_image(region: &Region)
{
    interrupts::with_masked(|| region.state.lock().phase = Phase::Acknowledged);
}
pub(super) fn quarantine_image(region: &Region, error: &crate::machine::monitor::Error)
{
    quarantine_region(region,
                      modules::monitor_cause(region_base(region),
                                             modules::Stage::PublishTranslations,
                                             error));
}
pub(super) fn publish_image(region: &Arc<Region>, process: &process::Pin,
                            lease: &crate::machine::monitor::TranslationLease)
                            -> Result<(), Error>
{
    publish_region(region, process, lease)
}
fn phase(region: &Region) -> Phase
{
    interrupts::with_masked(|| region.state.lock().phase)
}
fn publish_region(region: &Arc<Region>, process: &process::Pin,
                  lease: &crate::machine::monitor::TranslationLease)
                  -> Result<(), Error>
{
    match phase(region)
    {
        Phase::Live => return Ok(()),
        Phase::Prepared => (),
        Phase::Quarantined => return Err(Error::Quarantined(region_cause(region))),
        _ => return Err(Error::Rejected(BAD_ABI))
    }
    let entries = with_metadata(process, || {
        let mut segments = Vec::new();
        segments.try_reserve_exact(region.spans.len()).map_err(|_| Error::Rejected(NO_MEMORY))?;
        for span in &region.spans
        {
            segments.push(span.segment());
        }
        modules::map_user(process, &segments, lease).map_err(Error::Paging)
    })?;
    interrupts::with_masked(|| {
        let mut state = region.state.lock();
        state.entries = Some(entries);
        state.phase = Phase::Mapped;
    });
    Ok(())
}
fn region_base(region: &Region) -> u64
{
    region.reserved.start as u64
}
fn region_cause(region: &Region) -> modules::CleanupCause
{
    interrupts::with_masked(||region.state.lock().failure)
        .unwrap_or(modules::CleanupCause::State{base:region_base(region)})
}
fn register_retiring(region: &Region) -> Result<(), Error>
{
    if region.signal.retiring.load(Ordering::Acquire)
    {
        return Ok(());
    }
    // The caller's exclusive lifecycle turn already owns Region.state.busy.
    RETIRING.try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
            .map_err(|_| {
                Error::Quarantined(modules::CleanupCause::State { base: region_base(region) })
            })?;
    region.signal.retiring.store(true, Ordering::Release);
    Ok(())
}
fn unregister_retiring(region: &Region)
{
    if region.signal.retiring.swap(false, Ordering::AcqRel)
    {
        RETIRING.fetch_sub(1, Ordering::AcqRel);
    }
}
fn quarantine_region(region: &Region, cause: modules::CleanupCause)
{
    interrupts::with_masked(|| {
        let mut state = region.state.lock();
        state.phase = Phase::Quarantined;
        state.failure = Some(cause);
    });
    unregister_retiring(region);
}
fn monitor_failure(region: &Region, stage: modules::Stage, cause: crate::machine::monitor::Error,
                   quarantine: bool)
                   -> Error
{
    if quarantine
    {
        quarantine_region(region, modules::monitor_cause(region_base(region), stage, &cause));
    }
    Error::Monitor { stage,
                     cause }
}

/// An affine mapping owner. Caller retains this value on every publication or
/// retirement refusal; it must settle retirement before dropping its payload.
#[must_use]
pub(crate) struct Mapping
{
    region: Option<Arc<Region>>
}
impl Mapping
{
    pub(crate) fn prepare(space: &process::Pin, backing: Arc<dyn Backing>, kind: Kind,
                          access: Access)
                          -> Result<Self, Status>
    {
        with_metadata(space, || Self::prepare_inner(space, backing, kind, access))
    }

    fn prepare_inner(space: &process::Pin, backing: Arc<dyn Backing>, kind: Kind, access: Access)
                     -> Result<Self, Status>
    {
        if backing.owner() != space.owner()
        {
            return Err(ACCESS_DENIED);
        }
        if !space.admitting()
        {
            return Err(ACCESS_DENIED);
        }
        if (kind == Kind::Descriptor && access != Access::Read)
           || (kind != Kind::Descriptor && access != Access::ReadWrite)
        {
            return Err(ACCESS_DENIED);
        }
        let bytes = backing.bytes();
        let page = PAGE_BYTES as usize;
        if bytes == 0
           || bytes as u64 > VM_APPLICATION_END - VM_APPLICATION_START
           || (kind == Kind::Heap && !bytes.is_power_of_two())
           || !bytes.is_multiple_of(page)
           || !backing.cpu_base().is_multiple_of(page)
           || !backing.physical().0.is_multiple_of(PAGE_BYTES)
           || backing.physical().0.checked_add(bytes as u64).is_none()
           || backing.cpu_base().checked_add(bytes).is_none()
        {
            return Err(BAD_ARGUMENT);
        }
        let guard = if kind == Kind::Stack { page } else { 0 };
        let length =
            bytes.checked_add(guard.checked_mul(2).ok_or(BAD_ARGUMENT)?).ok_or(BAD_ARGUMENT)?;
        // All metadata growth occurs before registration or any PTE effect.
        let mut spans = Vec::new();
        spans.try_reserve_exact(1).map_err(|_| NO_MEMORY)?;
        let region=Arc::try_new(Region {spans:Vec::new(),reserved:0..0,
            owner:Spinlock::new(Some(Owner::Pages { _backing: backing.clone() })),process:Some(space.clone()),
            closing:AtomicBool::new(false),signal:Arc::try_new(Signal{readers:Spinlock::new(Readers{count:0,waiting:false}),retiring:AtomicBool::new(false),heap_progress:backing.heap_progress()}).map_err(|_|NO_MEMORY)?,
            state:Spinlock::new(State{phase:Phase::Prepared,entries:None,busy:false,failure:None}),
            quarantine_next:Spinlock::new(None)}).map_err(|_|NO_MEMORY)?;
        let mut region = region;
        loop
        {
            let required = interrupts::with_masked(|| {
                let registry = space.user.lock();
                registry.dynamic.len().checked_add(1).ok_or(NO_MEMORY)
            })?;
            let mut spare = Vec::new();
            spare.try_reserve_exact(required).map_err(|_| NO_MEMORY)?;
            let result = interrupts::with_masked(|| -> Result<bool, Status> {
                let mut registry = space.user.lock();
                if registry.dynamic.len() >= spare.capacity()
                {
                    return Ok(false);
                }
                let mut base = VM_APPLICATION_START as usize;
                let alignment = if kind == Kind::Heap { bytes } else { page };
                let residue = backing.cpu_base() % alignment;
                loop
                {
                    let visible = base.checked_add(guard).ok_or(BAD_ARGUMENT)?;
                    let adjusted = visible.checked_add(alignment - 1)
                                          .ok_or(BAD_ARGUMENT)?
                                          .checked_sub(residue)
                                          .ok_or(BAD_ARGUMENT)?
                                   / alignment
                                   * alignment
                                   + residue;
                    base = adjusted.checked_sub(guard).ok_or(BAD_ARGUMENT)?;
                    let end = base.checked_add(length)
                                  .filter(|e| *e <= VM_APPLICATION_END as usize)
                                  .ok_or(NO_MEMORY)?;
                    let overlap = |start: usize, limit: usize| base < limit && start < end;
                    let mut next = 0;
                    if overlap(registry.image.reserved.start, registry.image.reserved.end)
                    {
                        next = registry.image.reserved.end;
                    }
                    if overlap(DEFINITION_BASE as usize, DEFINITION_LIMIT as usize)
                    {
                        next = next.max(DEFINITION_LIMIT as usize);
                    }
                    for &(start, limit) in BOOT_MMIO_REGIONS
                    {
                        if overlap(start as usize, limit as usize)
                        {
                            next = next.max(limit as usize);
                        }
                    }
                    for index in 0..crate::machine::platform_devices::SLOTS
                    {
                        let Some(device) = crate::machine::platform_devices::get(index)
                        else
                        {
                            continue;
                        };
                        let (start, end) =
                            (device.registers as usize,
                             (device.registers + device.register_bytes) as usize);
                        if overlap(start, end)
                        {
                            next = next.max(end);
                        }
                    }
                    if let Some(boot) = super::BOOT.get()
                    {
                        for entry in boot.regions
                        {
                            let limit = entry.virtual_base
                                             .checked_add(entry.physical.length)
                                             .ok_or(BAD_ABI)?;
                            if overlap(entry.virtual_base as usize, limit as usize)
                            {
                                next = next.max(limit as usize);
                            }
                        }
                    }
                    for held in
                        registry.dynamic
                                .iter()
                                .skip(registry.dynamic.partition_point(|r| r.reserved.end <= base))
                                .take_while(|r| r.reserved.start < end)
                                .filter(|r| r.region.strong_count() != 0)
                    {
                        if overlap(held.reserved.start, held.reserved.end)
                        {
                            next = next.max(held.reserved.end);
                        }
                    }
                    if next == 0
                    {
                        break;
                    }
                    base = next.checked_add(page - 1).ok_or(BAD_ARGUMENT)? / page * page;
                }
                let unique = Arc::get_mut(&mut region).ok_or(BAD_ABI)?;
                spans.push(Span { address: base + guard,
                                  cpu: backing.cpu_base(),
                                  physical: backing.physical(),
                                  bytes,
                                  flags: access.flags() });
                unique.spans = core::mem::take(&mut spans);
                unique.reserved = base..base + length;
                for held in registry.dynamic.iter().filter(|r| r.region.strong_count() != 0)
                {
                    spare.push(Record { region:   held.region.clone(),
                                        reserved: held.reserved.clone() });
                }
                let at = spare.partition_point(|r: &Record| r.reserved.start < base);
                spare.insert(at,
                             Record { region:   Arc::downgrade(&region),
                                      reserved: base..base + length });
                core::mem::swap(&mut registry.dynamic, &mut spare);
                Ok(true)
            })?;
            drop(spare);
            if result
            {
                return Ok(Self { region: Some(region) });
            }
        }
    }

    pub(super) fn definition(space: &process::Pin, payload: Arc<modules::Payload>,
                             source: crate::images::Read, mut spans: Vec<Span>)
                             -> Result<Self, Status>
    {
        spans.sort_unstable_by_key(|span| span.address);
        if spans.is_empty() || source.owner().owner() != space.owner()
        {
            return Err(ACCESS_DENIED);
        }
        let start = spans.iter().map(|s| s.address).min().ok_or(BAD_ABI)?;
        let end = spans.iter().filter_map(Span::end).max().ok_or(BAD_ABI)?;
        if start < DEFINITION_BASE as usize
           || end > DEFINITION_LIMIT as usize
           || start < VM_APPLICATION_START as usize
           || end > VM_APPLICATION_END as usize
        {
            return Err(BAD_ARGUMENT);
        }
        let region=Arc::try_new(Region{spans,reserved:start..end,
            owner:Spinlock::new(Some(Owner::Definition{_payload:payload,_source:source})),
            process:Some(space.clone()),closing:AtomicBool::new(false),
            signal:Arc::try_new(Signal{readers:Spinlock::new(Readers{count:0,waiting:false}),retiring:AtomicBool::new(false),heap_progress:None}).map_err(|_|NO_MEMORY)?,
            state:Spinlock::new(State{phase:Phase::Prepared,entries:None,busy:false,failure:None}),
            quarantine_next:Spinlock::new(None)}).map_err(|_|NO_MEMORY)?;
        loop
        {
            let required =
                interrupts::with_masked(|| space.user.lock().dynamic.len()).checked_add(1)
                                                                           .ok_or(NO_MEMORY)?;
            let mut spare = Vec::new();
            spare.try_reserve_exact(required).map_err(|_| NO_MEMORY)?;
            let admitted = interrupts::with_masked(|| {
                let mut registry = space.user.lock();
                if spare.capacity() < registry.dynamic.len() + 1
                {
                    return Ok(false);
                }
                if registry.dynamic.iter().any(|r| {
                                              r.region.strong_count() != 0
                                              && start < r.reserved.end
                                              && r.reserved.start < end
                                          })
                {
                    return Err(ACCESS_DENIED);
                }
                for held in registry.dynamic.iter().filter(|r| r.region.strong_count() != 0)
                {
                    spare.push(Record { region:   held.region.clone(),
                                        reserved: held.reserved.clone() });
                }
                let at = spare.partition_point(|r: &Record| r.reserved.start < start);
                spare.insert(at,
                             Record { region:   Arc::downgrade(&region),
                                      reserved: start..end });
                core::mem::swap(&mut registry.dynamic, &mut spare);
                Ok(true)
            })?;
            drop(spare);
            if admitted
            {
                return Ok(Self { region: Some(region) });
            }
        }
    }

    pub(crate) fn descriptor(space: &process::Pin, bytes: &[u8]) -> Result<Self, Status>
    {
        if bytes.is_empty()
        {
            return Err(BAD_ARGUMENT);
        }
        with_metadata(space, || {
            let planned =
                crate::machine::pages::Backing::planned_bytes(bytes.len()).map_err(|e| e.status())?;
            let reservation = space.allocation
                                   .reserve_kernel(planned)
                                   .map_err(|e| crate::allocation::account_status(&e))?;
            let backing =
                crate::machine::pages::Backing::allocate(bytes.len()).map_err(|e| e.status())?;
            if backing.bytes() != planned
            {
                return Err(BAD_ABI);
            }
            // SAFETY: backing is fully zeroed and exclusively unpublished.
            // Input is the resident's fieldwise descriptor byte encoding.
            unsafe {
                core::ptr::copy_nonoverlapping(bytes.as_ptr(),
                                               backing.cpu_base() as *mut u8,
                                               bytes.len());
            }
            let owner =
                Arc::try_new(DescriptorBacking { backing,
                                                 _charge: reservation.commit() }).map_err(|_| {
                                                                                     NO_MEMORY
                                                                                 })?;
            Self::prepare(space, owner, Kind::Descriptor, Access::Read)
        })
    }

    pub(crate) fn user_base(&self) -> usize
    {
        self.region.as_ref().map_or(0, |r| r.spans[0].address)
    }

    pub(crate) fn user_top(&self) -> usize
    {
        self.region.as_ref().map_or(0, |r| r.spans[0].address + r.spans[0].bytes)
    }

    pub(crate) fn cpu_base(&self) -> usize
    {
        self.region.as_ref().map_or(0, |r| r.spans[0].cpu)
    }

    pub(crate) fn quarantine_cause(&self) -> Option<modules::CleanupCause>
    {
        self.region.as_ref().and_then(|region| {
                                interrupts::with_masked(|| {
                                    let state = region.state.lock();
                                    (state.phase == Phase::Quarantined).then(|| state.failure)
                                }).flatten()
                            })
    }

    pub(crate) fn publish(&self) -> Result<(), Error>
    {
        let region = self.region.as_ref().ok_or(Error::Rejected(BAD_ABI))?;
        if phase(region) == Phase::Live
        {
            return Ok(());
        }
        let process = region.process.as_ref().ok_or(Error::Rejected(BAD_ABI))?;
        let lease = crate::machine::monitor::prepare_translation_invalidation().map_err(|cause| {
                        Error::Monitor { stage: modules::Stage::PublishTranslations,
                                         cause }
                    })?;
        publish_region(region, process, &lease)?;
        if let Err(cause) = lease.finish()
        {
            return Err(monitor_failure(region, modules::Stage::PublishTranslations, cause, true));
        }
        interrupts::with_masked(|| region.state.lock().phase = Phase::Live);
        Ok(())
    }

    pub(crate) fn retire(&mut self) -> Result<(), Error>
    {
        let Some(region) = self.region.as_ref()
        else
        {
            return Ok(());
        };
        let ready = interrupts::with_masked(|| {
            region.closing.store(true, Ordering::Release);
            let mut state = region.state.lock();
            if state.busy
            {
                return Err(Error::Rejected(SERVICE_BUSY));
            }
            if state.phase == Phase::Quarantined
            {
                return Err(Error::Quarantined(state.failure.unwrap_or(modules::CleanupCause::State{base:region_base(region)})));
            }
            if !matches!(state.phase, Phase::Prepared | Phase::Live | Phase::Retired)
            {
                return Err(Error::Quarantined(modules::CleanupCause::State { base:
                                                                                 region_base(region) }));
            }
            register_retiring(region)?;
            let mut readers = region.signal.readers.lock();
            if readers.count != 0
            {
                readers.waiting = true;
                return Err(Error::Readers(readers.count));
            }
            state.busy = true;
            Ok(state.phase)
        });
        let ready = match ready
        {
            Ok(ready) => ready,
            Err(Error::Quarantined(cause)) =>
            {
                quarantine_region(region, cause);
                return Err(Error::Quarantined(cause));
            }
            Err(cause) => return Err(cause)
        };
        let result = (|| {
            if ready == Phase::Live
            {
                let lease =
                    crate::machine::monitor::prepare_translation_invalidation().map_err(|cause| {
                        let terminal = !matches!(cause, crate::machine::monitor::Error::Busy);
                        monitor_failure(region, modules::Stage::RetireAdmission, cause, terminal)
                    })?;
                let unmap = interrupts::with_masked(|| {
                    let state = region.state.lock();
                    state.entries
                         .as_ref()
                         .ok_or(Error::Rejected(BAD_ABI))?
                         .unmap(&lease)
                         .map_err(Error::Paging)
                });
                if let Err(error) = unmap
                {
                    let cause = match &error
                    {
                        Error::Paging(modules::Error::Conflict { address,
                                                                 expected,
                                                                 observed }) =>
                        {
                            modules::CleanupCause::Pte { base:     region_base(region),
                                                         address:  *address,
                                                         expected: *expected,
                                                         observed: *observed }
                        }
                        Error::Paging(modules::Error::Revision) =>
                        {
                            modules::CleanupCause::Revision { base: region_base(region) }
                        }
                        _ => modules::CleanupCause::State { base: region_base(region) }
                    };
                    quarantine_region(region, cause);
                    return Err(error);
                }
                interrupts::with_masked(|| region.state.lock().phase = Phase::Cleared);
                if let Err(cause) = lease.finish()
                {
                    return Err(monitor_failure(region,
                                               modules::Stage::RetireTranslations,
                                               cause,
                                               true));
                }
            }
            let retired = interrupts::with_masked(|| {
                let mut state = region.state.lock();
                state.phase = Phase::Retired;
                (state.entries.take(), region.owner.lock().take())
            });
            drop(retired);
            unregister_retiring(region);
            Ok(())
        })();
        interrupts::with_masked(|| region.state.lock().busy = false);
        result?;
        drop(self.region.take());
        Ok(())
    }
}
fn park(list: &Spinlock<Option<Arc<Region>>>, region: Arc<Region>)
{
    interrupts::with_masked(|| {
        let mut head = list.lock();
        *region.quarantine_next.lock() = head.take();
        *head = Some(region);
    });
}
fn request_reap()
{
    if ORPHANS.load(Ordering::Acquire) != 0
    {
        REAP_REQUESTED.store(true, Ordering::Release);
        crate::machine::scheduler::wake::request();
    }
}
pub(super) fn monitor_released()
{
    if RETIRING.load(Ordering::Acquire) != 0
    {
        crate::toolbox::application_heap::wait::monitor_released();
        released();
    }
}
impl Drop for Mapping
{
    fn drop(&mut self)
    {
        let Some(region) = self.region.take()
        else
        {
            return;
        };
        match phase(&region)
        {
            Phase::Prepared | Phase::Retired =>
            {
                unregister_retiring(&region);
            }
            Phase::Live =>
            {
                region.closing.store(true, Ordering::Release);
                if register_retiring(&region).is_err()
                   || ORPHANS.try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
                             .is_err()
                {
                    quarantine_region(&region,
                                      modules::CleanupCause::State { base: region_base(&region) });
                    park(&QUARANTINE, region);
                    return;
                }
                park(&PENDING, region);
                request_reap();
            }
            _ =>
            {
                let cause = region_cause(&region);
                quarantine_region(&region, cause);
                park(&QUARANTINE, region);
            }
        }
    }
}
/// Actual guard-free maintenance. Capture once per real notice; no allocation,
/// manager guard, fake ACK or unchanged-refusal self-request occurs here.
pub(crate) fn pump()
{
    if REAPING.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err()
    {
        return;
    }
    struct Turn;
    impl Drop for Turn
    {
        fn drop(&mut self)
        {
            REAPING.store(false, Ordering::Release);
            if REAP_REQUESTED.load(Ordering::Acquire)
            {
                crate::machine::scheduler::wake::request();
            }
        }
    }
    let _turn = Turn;
    if !REAP_REQUESTED.swap(false, Ordering::AcqRel)
    {
        return;
    }
    let mut work = interrupts::with_masked(|| PENDING.lock().take());
    while let Some(region) = work
    {
        work = interrupts::with_masked(|| region.quarantine_next.lock().take());
        let mut mapping = Mapping { region: Some(region) };
        match mapping.retire()
        {
            Ok(()) =>
            {
                ORPHANS.fetch_sub(1, Ordering::AcqRel);
            }
            Err(_) =>
            {
                if let Some(region) = mapping.region.take()
                {
                    if phase(&region) == Phase::Quarantined
                    {
                        ORPHANS.fetch_sub(1, Ordering::AcqRel);
                        park(&QUARANTINE, region);
                    }
                    else
                    {
                        park(&PENDING, region);
                    }
                }
            }
        }
    }
}

/// Empty reusable metadata, admitted from the actual bounded request length.
/// No Region reader/pin is present until the canonical pin walk owns this value.
pub(crate) struct ReaderStorage
{
    more:  Vec<Reader>,
    bytes: usize
}
impl ReaderStorage
{
    pub(crate) fn is_empty(&self) -> bool
    {
        self.more.is_empty()
    }

    pub(crate) fn prepare(bytes: usize) -> Result<Self, Status>
    {
        if bytes > isize::MAX as usize
        {
            return Err(BAD_ARGUMENT);
        }
        let page = usize::try_from(PAGE_BYTES).map_err(|_| BAD_ABI)?;
        if !page.is_power_of_two()
        {
            return Err(BAD_ABI);
        }
        // Arbitrary byte offset may cross an extra page. Regions own disjoint
        // page-aligned extents. First Reader remains inline in Range.
        let extra = if bytes == 0
        {
            0
        }
        else
        {
            bytes.checked_sub(1).and_then(|n| n.checked_add(page - 1)).ok_or(BAD_ARGUMENT)? / page
        };
        let mut more = Vec::new();
        more.try_reserve_exact(extra).map_err(|_| NO_MEMORY)?;
        Ok(Self { more,
                  bytes })
    }
}

pub(crate) struct Memory
{
    space: process::Pin
}
impl Memory
{
    pub(crate) fn owner(&self) -> mac_object_table::domain::DomainId
    {
        self.space.owner()
    }

    pub(crate) fn image_id(&self) -> u64
    {
        self.space.image_id()
    }

    pub(crate) fn current() -> Result<Self, Status>
    {
        Ok(Self { space: crate::machine::scheduler::current_user_space()? })
    }

    pub(crate) fn pin(&self, address: usize, len: usize, access: Access, align: usize)
                      -> Result<Range, Status>
    {
        self.pin_inner(address,
                       len,
                       access,
                       align,
                       ReaderStorage { more:  Vec::new(),
                                       bytes: len },
                       false)
            .map_err(|(status, storage)| {
                drop(storage);
                status
            })
    }

    pub(crate) fn pin_prepared(&self, address: usize, len: usize, access: Access, align: usize,
                               storage: ReaderStorage)
                               -> Result<Range, (Status, ReaderStorage)>
    {
        self.pin_inner(address, len, access, align, storage, true)
    }

    fn pin_inner(&self, address: usize, len: usize, access: Access, align: usize,
                 mut storage: ReaderStorage, prepaid: bool)
                 -> Result<Range, (Status, ReaderStorage)>
    {
        // Both ordinary and prepaid requests use this sole validation/reader
        // walk; prepayment changes only how extra metadata is admitted.
        let mut first = None;
        let result = (|| -> Result<(), Status> {
            if !storage.more.is_empty()
            {
                return Err(BAD_ABI);
            }
            if prepaid && len > storage.bytes
            {
                return Err(BAD_ARGUMENT);
            }
            if !align.is_power_of_two()
               || address % align != 0
               || (len != 0 && address == 0)
               || len > isize::MAX as usize
            {
                return Err(BAD_ARGUMENT);
            }
            if !self.space.admitting()
            {
                return Err(ACCESS_DENIED);
            }
            let end = address.checked_add(len).ok_or(BAD_ARGUMENT)?;
            if !(address == 0 && len == 0)
               && (address < VM_APPLICATION_START as usize || end > VM_APPLICATION_END as usize)
            {
                return Err(ACCESS_DENIED);
            }
            let mut cursor = address;
            while cursor < end
            {
                let selected=interrupts::with_masked(||self.space.user.lock().reader(cursor)).ok_or(ACCESS_DENIED)?;
                let reader = Reader::admit(selected)?;
                let region = reader.region().ok_or(BAD_ABI)?;
                let limit = coverage(region, cursor, end, access)?;
                if first.is_none()
                {
                    first = Some(reader);
                }
                else
                {
                    if prepaid
                    {
                        if storage.more.len() == storage.more.capacity()
                        {
                            return Err(BAD_ABI);
                        }
                    }
                    else
                    {
                        with_metadata(&self.space, || storage.more.try_reserve(1)).map_err(|_| {
                                                                                      NO_MEMORY
                                                                                  })?;
                    }
                    storage.more.push(reader);
                }
                cursor = limit;
            }
            Ok(())
        })();
        if let Err(status) = result
        {
            // Actual Reader release/notifications occur outside every metadata
            // gate. The same now-empty storage returns on refusal.
            drop(first.take());
            storage.more.clear();
            return Err((status, storage));
        }
        Ok(Range { first,
                   more: storage.more,
                   reader_bytes: storage.bytes,
                   address,
                   bytes: len,
                   access,
                   _space: self.space.clone() })
    }
}
fn coverage(region: &Region, address: usize, end: usize, access: Access) -> Result<usize, Status>
{
    let at = region.spans.partition_point(|span| span.address <= address);
    let first = at.checked_sub(1).ok_or(ACCESS_DENIED)?;
    let mut cursor = address;
    for span in &region.spans[first..]
    {
        if cursor < span.address
           || cursor >= span.end().ok_or(BAD_ABI)?
           || !access.permits(span.flags)
        {
            break;
        }
        cursor = span.end().ok_or(BAD_ABI)?.min(end);
        if cursor == end
        {
            break;
        }
    }
    if cursor == address
    {
        return Err(ACCESS_DENIED);
    }
    Ok(cursor)
}

struct Reader(Option<Arc<Region>>);
impl Reader
{
    fn admit(region: Arc<Region>) -> Result<Self, Status>
    {
        let result = interrupts::with_masked(|| {
            let state = region.state.lock();
            if region.closing.load(Ordering::Acquire) || state.phase != Phase::Live
            {
                return Err(ACCESS_DENIED);
            }
            let mut readers = region.signal.readers.lock();
            readers.count = readers.count.checked_add(1).ok_or(NO_MEMORY)?;
            Ok(())
        });
        // Refused metadata upgrades release outside all gates, never becoming
        // a user-memory permit. Accepted permits retain the actual owner Arc.
        result?;
        Ok(Self(Some(region)))
    }

    fn region(&self) -> Option<&Region>
    {
        self.0.as_deref()
    }
}
impl Drop for Reader
{
    fn drop(&mut self)
    {
        let Some(region) = self.0.take()
        else
        {
            return;
        };
        let signal = region.signal.clone();
        drop(region);
        let wake = interrupts::with_masked(|| {
            let mut readers = signal.readers.lock();
            let Some(count) = readers.count.checked_sub(1)
            else
            {
                return false;
            };
            readers.count = count;
            if count == 0 { core::mem::take(&mut readers.waiting) } else { false }
        });
        let notice = if wake { signal.heap_progress.clone() } else { None };
        drop(signal);
        if let Some(notice) = notice
        {
            notice.notify();
        }
        if wake
        {
            released();
        }
    }
}
fn released()
{
    request_reap();
    modules::user_readers_released();
    crate::toolbox::processes::retirement::resources_released();
    crate::machine::scheduler::context_released();
}
pub(crate) struct Range
{
    // The common stack/image/heap request pins one actual owner inline.
    // Cross-Region requests consume actual additional reader storage; ordinary
    // pin grows it, prepaid pin uses only its previously admitted capacity.
    first:        Option<Reader>,
    more:         Vec<Reader>,
    reader_bytes: usize,
    address:      usize,
    bytes:        usize,
    access:       Access,
    _space:       process::Pin
}
impl Range
{
    /// Consume the actual pin before returning reusable empty bookkeeping.
    /// Callers invoke this outside scheduler, mapping and manager gates.
    pub(crate) fn release_storage(mut self) -> ReaderStorage
    {
        drop(self.first.take());
        self.more.clear();
        ReaderStorage { more:  core::mem::take(&mut self.more),
                        bytes: self.reader_bytes }
    }

    fn pieces(&self, mut transfer: impl FnMut(usize, usize, usize)) -> Result<(), Status>
    {
        let end = self.address.checked_add(self.bytes).ok_or(BAD_ABI)?;
        let mut cursor = self.address;
        for reader in self.first.iter().chain(self.more.iter())
        {
            let region = reader.region().ok_or(BAD_ABI)?;
            let at = region.spans.partition_point(|span| span.address <= cursor);
            let first = at.checked_sub(1).ok_or(BAD_ABI)?;
            for span in &region.spans[first..]
            {
                if cursor < span.address || cursor >= span.end().ok_or(BAD_ABI)?
                {
                    break;
                }
                let bytes = (span.end().ok_or(BAD_ABI)? - cursor).min(end - cursor);
                transfer(span.cpu + (cursor - span.address), cursor - self.address, bytes);
                cursor += bytes;
                if cursor == end
                {
                    return Ok(());
                }
            }
        }
        if cursor == end { Ok(()) } else { Err(BAD_ABI) }
    }

    pub(crate) fn copy_in(&self, destination: &mut [u8]) -> Result<(), Status>
    {
        if destination.len() != self.bytes || self.access == Access::Execute
        {
            return Err(BAD_ARGUMENT);
        }
        self.pieces(|cpu, offset, bytes| {
                // SAFETY: entire immutable span sequence was permission/range
                // validated before Range construction; retained actual owners exclude
                // unmap/reuse. Destination is exclusive resident-owned byte storage.
                unsafe {
                    copy_aliases(cpu, destination.as_mut_ptr().add(offset) as usize, bytes);
                }
            })
    }

    pub(crate) fn copy_out(&self, source: &[u8]) -> Result<(), Status>
    {
        if source.len() != self.bytes || self.access != Access::ReadWrite
        {
            return Err(ACCESS_DENIED);
        }
        self.pieces(|cpu, offset, bytes| {
                // SAFETY: checked writable pinned bytes, no user Rust references.
                unsafe {
                    copy_aliases(source.as_ptr().add(offset) as usize, cpu, bytes);
                }
            })
    }
}
impl Drop for Range
{
    fn drop(&mut self)
    {
        drop(self.first.take());
        self.more.clear();
    }
}

// SAFETY: Charged owns the actual Stack before its original Committed receipt;
// Stack provides factory-derived stable backing and never resizes after birth.
unsafe impl Backing for crate::machine::scheduler::accounting::Charged<crate::machine::pages::Stack>
{
    fn owner(&self) -> mac_object_table::domain::DomainId
    {
        crate::machine::scheduler::accounting::Charged::owner(self)
    }

    fn cpu_base(&self) -> usize
    {
        let stack: &crate::machine::pages::Stack = &**self;
        stack.top() - stack.bytes() as usize
    }

    fn physical(&self) -> PhysicalAddress
    {
        let stack: &crate::machine::pages::Stack = &**self;
        stack.physical()
    }

    fn bytes(&self) -> usize
    {
        let stack: &crate::machine::pages::Stack = &**self;
        stack.bytes() as usize
    }
}

/// # Safety
/// Caller retains exclusive logical span ownership and actual mapped physical
/// backing for the complete ranges. This never constructs a user Rust reference.
pub(crate) unsafe fn copy_aliases(source: usize, destination: usize, bytes: usize)
{
    let mut offset = 0;
    while offset < bytes
    {
        if (source + offset).is_multiple_of(8)
           && (destination + offset).is_multiple_of(8)
           && bytes - offset >= 8
        {
            // SAFETY: the caller owns both spans; the alignment test above
            // selects the word path only where a word access is valid.
            let value = unsafe { super::super::mmu::read_word(source + offset) };
            unsafe { super::super::mmu::write_word(destination + offset, value) };
            offset += 8;
        }
        else
        {
            // SAFETY: as above; the byte path is valid at any alignment.
            let value = unsafe { super::super::mmu::read_byte(source + offset) };
            unsafe { super::super::mmu::write_byte(destination + offset, value) };
            offset += 1;
        }
    }
}

/// # Safety
/// Caller retains the complete writable physical span; no metadata is inside it.
pub(crate) unsafe fn zero_alias(destination: usize, bytes: usize)
{
    let mut offset = 0;
    while offset < bytes
    {
        if (destination + offset).is_multiple_of(8) && bytes - offset >= 8
        {
            // SAFETY: the caller owns the span; alignment is checked above.
            unsafe { super::super::mmu::zero_word(destination + offset) };
            offset += 8;
        }
        else
        {
            // SAFETY: as above; a byte store is valid at any alignment.
            unsafe { super::super::mmu::zero_byte(destination + offset) };
            offset += 1;
        }
    }
}

struct DescriptorBacking
{
    backing: crate::machine::pages::Backing,
    _charge: mac_kernel_model::account::Committed<crate::machine::interrupts::NativeInvocationGate>
}
// SAFETY: exclusive factory backing and original Kernel charge are inseparable;
// descriptor is initialized before publication and never exposes mutable bytes.
unsafe impl Backing for DescriptorBacking
{
    fn owner(&self) -> mac_object_table::domain::DomainId
    {
        self._charge.owner()
    }

    fn cpu_base(&self) -> usize
    {
        self.backing.cpu_base()
    }

    fn physical(&self) -> PhysicalAddress
    {
        self.backing.physical()
    }

    fn bytes(&self) -> usize
    {
        self.backing.bytes() as usize
    }
}

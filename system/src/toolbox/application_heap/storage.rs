//! Safe owner/extent metadata. Every allocation is prepared outside guards;
//! final disposal moves actual owners out before destruction or canonical ACK.
#![forbid(unsafe_code)]
use alloc::{boxed::Box, sync::Arc};
use core::{alloc::Layout,
           sync::atomic::{AtomicBool, AtomicUsize, Ordering}};

use hashbrown::HashMap;
use spinning_top::Spinlock;

use super::*;

pub(super) enum HeldSpan
{
    Reserved(Reservation),
    Live(Span)
}
pub(super) enum Work
{
    Empty,
    Retiring
    {
        extent:   Empty,
        detached: Option<Arc<Extent>>,
        region:   Option<Arc<super::wait::Region>>
    },
    Quarantined
    {
        cause: heap::Error,
        _span: HeldSpan
    },
    Unindexed
    {
        _extent: Empty
    }
}
/// One node prepaid before any new allocation. It has no HeapArc backreference;
/// linking it into its actual root cannot create a normal ownership cycle.
pub(super) struct Deferred
{
    pub(super) work: Work,
    next:            Option<Box<Deferred>>
}
impl Deferred
{
    pub(super) fn prepare() -> Result<Box<Self>, Error>
    {
        Box::try_new(Self { work: Work::Empty,
                            next: None }).map_err(|_| {
                                             Error::Metadata { stage:     "allocation rollback \
                                                                           anchor",
                                                               requested: 1 }
                                         })
    }
}
struct ExtentEntry
{
    extent: Arc<Extent>,
    anchor: Box<Deferred>,
    region: Arc<super::wait::Region>
}
pub(super) struct State
{
    pub(super) addresses: HashMap<usize, CompletionKey>,
    pub(super) credits:   usize,
    extents:              HashMap<usize, ExtentEntry>,
    retiring:             Option<Box<Deferred>>,
    quarantine:           Option<Box<Deferred>>,
    pub(super) fault:     Option<heap::Error>
}
pub(super) struct Heap
{
    pub(super) owner:      OwnerRef,
    pub(super) allocation: AllocationAuthority,
    pub(super) state:      Spinlock<State>,
    pub(super) closing:    AtomicBool,
    pending_retirement:    AtomicBool,
    unused_extent:         AtomicBool,
    reap_running:          AtomicBool,
    pub(super) wait_dirty: AtomicBool,
    retiring_count:        AtomicUsize
}
impl Heap
{
    pub(super) fn new(owner: OwnerRef, allocation: AllocationAuthority) -> Self
    {
        Self { owner,
               allocation,
               state: Spinlock::new(State { addresses:  HashMap::new(),
                                            credits:    0,
                                            extents:    HashMap::new(),
                                            retiring:   None,
                                            quarantine: None,
                                            fault:      None }),
               closing: AtomicBool::new(false),
               pending_retirement: AtomicBool::new(false),
               unused_extent: AtomicBool::new(false),
               reap_running: AtomicBool::new(false),
               wait_dirty: AtomicBool::new(false),
               retiring_count: AtomicUsize::new(0) }
    }

    /// Actual extent lookup; no pointer or allocation-liveness grant is made.
    pub(super) fn wait_extent(this: &Arc<Self>, address: usize)
                              -> Result<Option<(Arc<Extent>, Arc<super::wait::Region>)>, Error>
    {
        this.admission()?;
        this.with(|state| {
            if !this.owner.accepting() || this.closing.load(Ordering::Acquire)
            {
                return Err(Error::Account(account::Error::Closed { owner: this.owner.owner() }));
            }
            if let Some(cause) = state.fault
            {
                return Err(Error::Span(cause));
            }
            pages::application_wait_extent(address, |base, bytes| {
                let entry = state.extents.get(&base)?;
                let end =
                    address.checked_add(core::mem::size_of::<core::sync::atomic::AtomicU32>())?;
                let limit = base.checked_add(usize::try_from(bytes).ok()?)?;
                (entry.region.bytes() == bytes
                 && address >= base
                 && end <= limit
                 && entry.region.accepting()).then(|| (entry.extent.clone(), entry.region.clone()))
            }).map_err(|cause| Error::Extent(pages::ApplicationExtentError::Pages(cause)))
        })
    }

    pub(super) fn wait_accepting(&self) -> bool
    {
        self.owner.accepting() && !self.closing.load(Ordering::Acquire)
    }

    /// Mark only real deferred physical retirement; never scan/reap in Drop.
    pub(super) fn notify_wait_release(&self)
    {
        if self.retiring_count.load(Ordering::Acquire) != 0
        {
            self.wait_dirty.store(true, Ordering::Release);
            super::wait::request_reap();
        }
    }

    pub(super) fn kernel<R>(&self, operation: impl FnOnce() -> R) -> R
    {
        crate::allocator::with_target(crate::allocator::Target::Owner{account:self.allocation.clone(),
            class:mac_kernel_model::policy::MemoryClass::Kernel},operation)
    }

    pub(super) fn admission(&self) -> Result<(), Error>
    {
        if !self.owner.accepting() || self.closing.load(Ordering::Acquire)
        {
            return Err(Error::Account(account::Error::Closed { owner: self.owner.owner() }));
        }
        if let Some(cause) = self.with(|state| state.fault)
        {
            return Err(Error::Span(cause));
        }
        Ok(())
    }

    pub(super) fn with<R>(&self, operation: impl FnOnce(&mut State) -> R) -> R
    {
        interrupts::with_masked(|| operation(&mut self.state.lock()))
    }

    pub(super) fn index_credit(this: &Arc<Self>) -> Result<IndexCredit, Error>
    {
        let mut prepared = HashMap::new();
        loop
        {
            this.admission()?;
            let attempt = this.with(|state| {
                                  if let Some(cause) = state.fault
                                  {
                                      return Err(Error::Span(cause));
                                  }
                                  let required = state.addresses
                                                      .len()
                                                      .checked_add(state.credits)
                                                      .and_then(|n| n.checked_add(1))
                                                      .ok_or(Error::Layout)?;
                                  if state.addresses.capacity() >= required
                                  {
                                      state.credits += 1;
                                      return Ok(Ok(()));
                                  }
                                  if prepared.capacity() >= required
                                  {
                                      for (pointer, key) in state.addresses.drain()
                                      {
                                          prepared.insert(pointer, key);
                                      }
                                      core::mem::swap(&mut state.addresses, &mut prepared);
                                      state.credits += 1;
                                      return Ok(Ok(()));
                                  }
                                  Ok(Err(required))
                              })?;
            match attempt
            {
                Ok(()) =>
                {
                    drop(prepared);
                    return Ok(IndexCredit { heap:   this.clone(),
                                            active: true });
                }
                Err(required) =>
                {
                    let mut replacement = HashMap::new();
                    this.kernel(||replacement.try_reserve(required))
                        .map_err(|_|Error::Metadata{stage:"address index",requested:required})?;
                    prepared = replacement;
                }
            }
        }
    }

    pub(super) fn reserve(this: &Arc<Self>, layout: Layout) -> Result<Reservation, Error>
    {
        loop
        {
            this.admission()?;
            enum Search
            {
                Span(Reservation),
                Metadata(Arc<Extent>, usize, usize),
                Grow
            }
            let search =
                this.with(|state| {
                        if let Some(cause) = state.fault
                        {
                            return Err(Error::Span(cause));
                        }
                        let mut metadata = None;
                        for entry in state.extents.values()
                        {
                            let extent = &entry.extent;
                            match Extent::reserve(extent, layout)
                            {
                                Ok(span) => return Ok(Search::Span(span)),
                                Err(heap::Error::NeedMetadata { level,
                                                                slots }) =>
                                {
                                    metadata.get_or_insert((extent.base(), level, slots));
                                }
                                Err(heap::Error::NoFit { .. } | heap::Error::Retiring) => (),
                                Err(cause) => return Err(Error::Span(cause))
                            }
                        }
                        if let Some((base, level, slots)) = metadata
                        {
                            let entry = state.extents.get(&base).ok_or(Error::Root)?;
                            return Ok(Search::Metadata(entry.extent.clone(), level, slots));
                        }
                        Ok(Search::Grow)
                    })?;
            match search
            {
                Search::Span(span) => return Ok(span),
                Search::Metadata(extent, level, slots) =>
                {
                    let nodes = this.kernel(|| heap::PreparedNodes::prepare(level))
                                    .map_err(|_| {
                                        Error::Metadata { stage:     "subpage nodes",
                                                          requested: slots }
                                    })?;
                    this.admission()?;
                    match extent.install_nodes(nodes)
                    {
                        Ok(()) => (),
                        Err(failure) =>
                        {
                            let cause = failure.cause;
                            drop(failure.nodes);
                            if !matches!(cause,
                                         heap::Error::MetadataSuperseded { .. }
                                         | heap::Error::Retiring)
                            {
                                return Err(Error::Span(cause));
                            }
                        }
                    }
                    drop(extent);
                }
                Search::Grow => Self::grow(this, layout)?
            }
        }
    }

    fn grow(this: &Arc<Self>, layout: Layout) -> Result<(), Error>
    {
        this.admission()?;
        let minimum=usize::try_from(crate::machine::memory::APPLICATION_HEAP_MINIMUM_EXTENT_BYTES)
            .map_err(|_|Error::Layout)?;
        // Padding uses the actual visible base in the core. No claim that all
        // discovered buddy-bank bases satisfy an arbitrary Rust alignment.
        let requested =
            layout.size().checked_add(layout.align() - 1).ok_or(Error::Layout)?.max(minimum);
        let bytes=pages::Backing::planned_bytes(requested).map_err(|cause|Error::Extent(pages::ApplicationExtentError::Pages(cause)))?;
        let mut anchor = Some(this.kernel(Deferred::prepare)?);
        let charge = this.allocation.reserve_application(bytes)?;
        let nodes=this.kernel(||heap::PreparedNodes::prepare(0))
            .map_err(|_|Error::Metadata{stage:"first extent node",requested:1})?;
        let extent = match pages::prepare_application_extent(&this.owner, charge, requested, nodes)
        {
            Ok(extent) => extent,
            Err(failure) =>
            {
                let cause = failure.dispose();
                return Err(Error::Extent(cause));
            }
        };
        let extent = this.kernel(|| Arc::try_new(extent)).map_err(|_| {
                                                              Error::Metadata { stage:
                                                                                    "extent owner",
                                                                                requested: 1 }
                                                          })?;
        let region = this.kernel(|| super::wait::Region::prepare(bytes))?;
        let mut spare = HashMap::new();
        loop
        {
            let result = this.with(|state| {
                                 if state.extents.contains_key(&extent.base())
                                 {
                                     return Err(Error::Root);
                                 }
                                 let required =
                                     state.extents.len().checked_add(1).ok_or(Error::Layout)?;
                                 if state.extents.capacity() < required
                                 {
                                     if spare.capacity() < required
                                     {
                                         return Ok(Err(required));
                                     }
                                     for (base, owner) in state.extents.drain()
                                     {
                                         spare.insert(base, owner);
                                     }
                                     core::mem::swap(&mut state.extents, &mut spare);
                                 }
                                 let Some(anchor) = anchor.take()
                                 else
                                 {
                                     return Err(Error::Span(heap::Error::State));
                                 };
                                 state.extents.insert(extent.base(),
                                                      ExtentEntry { extent: extent.clone(),
                                                                    anchor,
                                                                    region: region.clone() });
                                 this.unused_extent.store(true, Ordering::Release);
                                 Ok(Ok(()))
                             })?;
            match result
            {
                Ok(()) =>
                {
                    drop(spare);
                    drop(extent);
                    return Ok(());
                }
                Err(required) =>
                {
                    let mut replacement = HashMap::new();
                    this.kernel(||replacement.try_reserve(required))
                        .map_err(|_|Error::Metadata{stage:"extent directory",requested:required})?;
                    spare = replacement;
                }
            }
        }
    }

    /// Consume an already-released empty span's prepaid anchor. No allocation,
    /// owning destructor or page refund occurs under this metadata guard.
    pub(super) fn retire(&self, empty: Empty) -> Result<(), Empty>
    {
        let base = empty.base();
        self.with(|state| {
                match state.extents.entry(base)
                {
                    hashbrown::hash_map::Entry::Vacant(_) =>
                    {
                        state.fault.get_or_insert(heap::Error::State);
                        Err(empty)
                    }
                    hashbrown::hash_map::Entry::Occupied(entry) =>
                    {
                        let ExtentEntry { extent,
                                          mut anchor,
                                          region } = entry.remove();
                        region.close();
                        anchor.work = Work::Retiring { extent:   empty,
                                                       detached: Some(extent),
                                                       region:   Some(region) };
                        anchor.next = state.retiring.take();
                        state.retiring = Some(anchor);
                        self.retiring_count.fetch_add(1, Ordering::AcqRel);
                        self.pending_retirement.store(true, Ordering::Release);
                        Ok(())
                    }
                }
            })
    }

    /// Called only in the actual exclusive HeapState/Cell commit turn. The
    /// occupied extent entry is retained BEFORE the first core release effect.
    pub(super) fn release_checked(&self, state: &mut State, span: Span)
                                  -> Result<(), heap::Failure<Span>>
    {
        let entry = match state.extents.entry(span.base())
        {
            hashbrown::hash_map::Entry::Occupied(entry) => entry,
            hashbrown::hash_map::Entry::Vacant(_) =>
            {
                state.fault.get_or_insert(heap::Error::State);
                return Err(heap::Failure { cause: heap::Error::State,
                                           owned: span });
            }
        };
        match span.release()
        {
            Err(failure) => Err(failure),
            Ok(heap::Released::Reusable) => Ok(()),
            Ok(heap::Released::Empty(empty)) =>
            {
                let ExtentEntry { extent,
                                  mut anchor,
                                  region } = entry.remove();
                region.close();
                anchor.work = Work::Retiring { extent:   empty,
                                               detached: Some(extent),
                                               region:   Some(region) };
                anchor.next = state.retiring.take();
                state.retiring = Some(anchor);
                self.retiring_count.fetch_add(1, Ordering::AcqRel);
                self.pending_retirement.store(true, Ordering::Release);
                Ok(())
            }
        }
    }

    pub(super) fn quarantine(&self, cause: heap::Error, span: HeldSpan, mut anchor: Box<Deferred>)
    {
        self.with(|state| {
                state.fault.get_or_insert(cause);
                anchor.work = Work::Quarantined { cause,
                                                  _span: span };
                anchor.next = state.quarantine.take();
                state.quarantine = Some(anchor);
            });
    }

    pub(super) fn quarantine_empty(&self, empty: Empty, mut anchor: Box<Deferred>)
    {
        anchor.work = Work::Unindexed { _extent: empty };
        self.with(|state| {
                state.fault = Some(heap::Error::State);
                anchor.next = state.quarantine.take();
                state.quarantine = Some(anchor);
            });
    }

    pub(super) fn reap(&self)
    {
        if self.reap_running
               .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
               .is_err()
        {
            // A concurrent turn owns the detached list. Preserve this real
            // release's notification until that turn has returned its owners.
            self.notify_wait_release();
            return;
        }
        struct Turn<'a>(&'a AtomicBool);
        impl Drop for Turn<'_>
        {
            fn drop(&mut self)
            {
                self.0.store(false, Ordering::Release);
            }
        }
        let _turn = Turn(&self.reap_running);
        if self.unused_extent.load(Ordering::Acquire)
           && self.unused_extent.swap(false, Ordering::AcqRel)
        {
            // One pass only after a real growth change. Core transition and
            // actual entry/anchor detachment occur in the SAME root turn.
            self.with(|state| {
                    let State { extents,
                                retiring,
                                fault,
                                .. } = state;
                    for (_,
                         ExtentEntry { extent,
                                       mut anchor,
                                       region }) in
                        extents.extract_if(|_, entry| {
                                   match Extent::retire_empty(&entry.extent)
                                   {
                                       Ok(empty) =>
                                       {
                                           entry.anchor.work = Work::Retiring { extent:   empty,
                                                                                detached: None,
                                                                                region:   None };
                                           true
                                       }
                                       Err(heap::Error::NotEmpty | heap::Error::Retiring) => false,
                                       Err(cause) =>
                                       {
                                           fault.get_or_insert(cause);
                                           false
                                       }
                                   }
                               })
                    {
                        region.close();
                        if let Work::Retiring { detached,
                                                region: held_region,
                                                .. } = &mut anchor.work
                        {
                            *detached = Some(extent);
                            *held_region = Some(region);
                        }
                        anchor.next = retiring.take();
                        *retiring = Some(anchor);
                        self.retiring_count.fetch_add(1, Ordering::AcqRel);
                        self.pending_retirement.store(true, Ordering::Release);
                    }
                });
        }
        if !self.pending_retirement.load(Ordering::Acquire)
           || !self.pending_retirement.swap(false, Ordering::AcqRel)
        {
            return;
        }
        // One captured list per actual progress change. A still-pinned node is
        // returned once; never spin waiting for another active operation.
        let mut nodes = self.with(|state| state.retiring.take());
        while let Some(mut node) = nodes
        {
            nodes = node.next.take();
            let (extent, detached, region) = match core::mem::replace(&mut node.work, Work::Empty)
            {
                Work::Retiring { extent,
                                 detached,
                                 region } => (extent, detached, region),
                other =>
                {
                    node.work = other;
                    self.with(|s| {
                            s.fault.get_or_insert(heap::Error::State);
                        });
                    self.quarantine_node(node);
                    continue;
                }
            };
            drop(detached);
            match extent.try_retire()
            {
                Ok(retired) =>
                {
                    drop(retired);
                    drop(region);
                    self.retiring_count.fetch_sub(1, Ordering::AcqRel);
                    drop(node)
                }
                Err(extent) =>
                {
                    node.work = Work::Retiring { extent,
                                                 detached: None,
                                                 region };
                    self.with(|state| {
                            node.next = state.retiring.take();
                            state.retiring = Some(node);
                        });
                    self.pending_retirement.store(true, Ordering::Release);
                }
            }
        }
    }

    fn quarantine_node(&self, mut node: Box<Deferred>)
    {
        self.with(|state| {
                node.next = state.quarantine.take();
                state.quarantine = Some(node);
            });
    }

    pub(super) fn cleanup_refusal(&self) -> Option<CleanupCause>
    {
        self.with(|state| {
                if let Some(node) = state.quarantine.as_ref()
                {
                    let cause = match &node.work
                    {
                        Work::Quarantined { cause,
                                            .. } => *cause,
                        _ => heap::Error::State
                    };
                    return Some(CleanupCause::Fault(cause));
                }
                if let Some(cause) = state.fault
                {
                    return Some(CleanupCause::Fault(cause));
                }
                if state.addresses.is_empty()
                   && state.credits == 0
                   && state.extents.is_empty()
                   && state.retiring.is_none()
                {
                    None
                }
                else
                {
                    Some(CleanupCause::Remaining { allocations: state.addresses.len(),
                                                   extents:     state.extents.len(),
                                                   preparing:   state.credits })
                }
            })
    }
}

pub(super) struct IndexCredit
{
    pub(super) heap:   Arc<Heap>,
    pub(super) active: bool
}
impl Drop for IndexCredit
{
    fn drop(&mut self)
    {
        if self.active
        {
            self.heap.with(|state| {
                         match state.credits.checked_sub(1)
                         {
                             Some(n) => state.credits = n,
                             None => state.fault = Some(heap::Error::Counters)
                         }
                     });
        }
    }
}

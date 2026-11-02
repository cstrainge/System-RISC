//! Canonical allocation ownership and exclusive logical mutation leases. No
//! metadata lock spans byte copying; rollback always retains its actual owner.
#![forbid(unsafe_code)]
use alloc::{boxed::Box, sync::Arc};
use core::alloc::Layout;

use spinning_top::Spinlock;

use super::{storage::{Deferred, HeldSpan, IndexCredit},
            *};

pub(super) struct SpanValue
{
    pub(super) span:    Span,
    pub(super) pointer: usize,
    pub(super) layout:  Layout
}
pub(super) struct Live
{
    pub(super) value:  SpanValue,
    pub(super) anchor: Box<Deferred>
}
pub(super) struct Cell
{
    pub(super) live:     Option<Live>,
    pub(super) orphan:   Option<HeldSpan>,
    pub(super) fault:    Option<heap::Error>,
    pub(super) busy:     bool,
    pub(super) released: bool
}
pub(super) struct Allocation
{
    pub(super) heap: Arc<Heap>,
    pub(super) cell: Spinlock<Cell>
}

struct Released
{
    owner: Option<OwnerRef>
}
impl Drop for Released
{
    fn drop(&mut self)
    {
        // This is foundation ledger authority, not an AccountUse/heap pin.
        // All actual allocation/heap owners precede this final marker.
        if let Some(owner) = self.owner.take()
        {
            let stopped = !owner.accepting();
            drop(owner);
            if stopped
            {
                crate::toolbox::processes::retirement::resources_released();
            }
        }
    }
}

fn cleanup(
    span: HeldSpan)
    -> Result<heap::Released<'static, 'static, NativeInvocationGate>, (heap::Error, HeldSpan)>
{
    match span
    {
        HeldSpan::Reserved(span) =>
        {
            span.abort().map_err(|f| (f.cause, HeldSpan::Reserved(f.owned)))
        }
        HeldSpan::Live(span) => span.release().map_err(|f| (f.cause, HeldSpan::Live(f.owned)))
    }
}
fn settle(heap: &Heap, span: HeldSpan, anchor: Box<Deferred>) -> Result<(), heap::Error>
{
    match cleanup(span)
    {
        Ok(heap::Released::Reusable) =>
        {
            drop(anchor);
            Ok(())
        }
        Ok(heap::Released::Empty(empty)) =>
        {
            match heap.retire(empty)
            {
                Ok(()) =>
                {
                    drop(anchor);
                    Ok(())
                }
                Err(empty) =>
                {
                    heap.quarantine_empty(empty, anchor);
                    Err(heap::Error::State)
                }
            }
        }
        Err((cause, span)) =>
        {
            heap.quarantine(cause, span, anchor);
            Err(cause)
        }
    }
}

/// One actual unpublished span plus inseparable prepaid failure anchor.
pub(super) struct Draft
{
    heap:               Arc<Heap>,
    held:               Option<(HeldSpan, Box<Deferred>)>,
    pub(super) pointer: usize,
    pub(super) layout:  Layout,
    _released:          Released
}
impl Drop for Draft
{
    fn drop(&mut self)
    {
        if let Some((span, anchor)) = self.held.take()
        {
            let _ = settle(&self.heap, span, anchor);
        }
        self.heap.reap();
    }
}
impl Draft
{
    fn refuse(&mut self, original: Error) -> Error
    {
        let rollback = self.held.take().map(|(span, anchor)| settle(&self.heap, span, anchor));
        self.heap.reap();
        match rollback
        {
            Some(Err(cause)) =>
            {
                Error::Rollback { original: original.status(),
                                  cause }
            }
            _ => original
        }
    }

    pub(super) fn prepare(heap: &Arc<Heap>, layout: Layout) -> Result<Self, Error>
    {
        heap.admission()?;
        let anchor = heap.kernel(Deferred::prepare)?;
        let reservation = Heap::reserve(heap, layout)?;
        // Establish the rollback owner before fallible information checks.
        let mut draft = Self { heap: heap.clone(),
                               held: Some((HeldSpan::Reserved(reservation), anchor)),
                               pointer: 0,
                               layout,
                               _released: Released { owner: Some(heap.owner.clone()) } };
        let (span, _) = draft.held.as_ref().ok_or(Error::Root)?;
        let HeldSpan::Reserved(span) = span
        else
        {
            return Err(Error::Root);
        };
        let info = span.info()?;
        draft.pointer = span.base().checked_add(info.offset).ok_or(Error::Layout)?;
        if info.layout != layout || draft.pointer == 0 || draft.pointer % layout.align() != 0
        {
            return Err(Error::Pointer);
        }
        Ok(draft)
    }

    fn make_live(&mut self) -> Result<(), Error>
    {
        let Some((span, anchor)) = self.held.take()
        else
        {
            return Err(Error::Root);
        };
        match span
        {
            HeldSpan::Live(span) =>
            {
                self.held = Some((HeldSpan::Live(span), anchor));
                Ok(())
            }
            HeldSpan::Reserved(span) =>
            {
                match span.commit()
                {
                    Ok(span) =>
                    {
                        self.held = Some((HeldSpan::Live(span), anchor));
                        Ok(())
                    }
                    Err(f) =>
                    {
                        self.held = Some((HeldSpan::Reserved(f.owned), anchor));
                        Err(Error::Span(f.cause))
                    }
                }
            }
        }
    }

    pub(super) fn publish(mut self, permit: PublicationPermit<NativeInvocationGate>,
                          mut credit: IndexCredit)
                          -> Result<usize, Error>
    {
        self.make_live()?;
        let record = match self.heap.kernel(|| {
                                        Arc::try_new(Allocation {heap:self.heap.clone(),
            cell:Spinlock::new(Cell{live:None,orphan:None,fault:None,busy:true,released:false})})
                                    })
        {
            Ok(record) => record,
            Err(_) =>
            {
                return Err(self.refuse(Error::Metadata { stage:     "allocation record",
                                                         requested: 1 }));
            }
        };
        let manager = MANAGER.get().ok_or(Error::Unavailable)?;
        let pair = self.held.take().ok_or(Error::Root)?;
        let live = match pair
        {
            (HeldSpan::Live(span), anchor) =>
            {
                Live { value: SpanValue { span,
                                          pointer: self.pointer,
                                          layout: self.layout },
                       anchor }
            }
            pair =>
            {
                self.held = Some(pair);
                return Err(Error::Root);
            }
        };
        // Every failure returns the SAME owning payload/permit; no cleanup runs
        // under Root/Cell/Store guards. All kind/capacity checks precede birth.
        let result =
            self.heap.with(|state| {
                         let mut cell = record.cell.lock();
                         let required = state.addresses.len().checked_add(state.credits);
                         if !credit.active
                            || !Arc::ptr_eq(&credit.heap, &self.heap)
                            || state.credits == 0
                            || state.addresses.contains_key(&self.pointer)
                            || required.is_none_or(|required| state.addresses.capacity() < required)
                            || cell.live.is_some()
                            || cell.orphan.is_some()
                            || !cell.busy
                            || cell.released
                         {
                             return Err((Error::Root, live, permit, None));
                         }
                         match manager.lock().allocations.publish_prepared(permit, record.clone())
                         {
                             Err(refused) =>
                             {
                                 Err((Error::Publication(refused.cause),
                                      live,
                                      refused.permit,
                                      Some(refused.value)))
                             }
                             Ok((publication, retired)) =>
                             {
                                 let (_, key) = publication.into_parts();
                                 cell.live = Some(live);
                                 cell.busy = false;
                                 state.addresses.insert(self.pointer, key);
                                 state.credits -= 1;
                                 credit.active = false;
                                 Ok(retired)
                             }
                         }
                     });
        match result
        {
            Ok(retired) =>
            {
                retired.dispose();
                drop(credit);
                drop(record);
                Ok(self.pointer)
            }
            Err((cause,
                 Live { value,
                        anchor },
                 permit,
                 failed_record)) =>
            {
                self.held = Some((HeldSpan::Live(value.span), anchor));
                drop(permit);
                drop(failed_record);
                drop(credit);
                drop(record);
                Err(self.refuse(cause))
            }
        }
    }
}

pub(super) struct Lease
{
    pub(super) record: Arc<Allocation>,
    pub(super) key:    CompletionKey,
    live:              Option<Live>,
    _released:         Released
}
impl Drop for Lease
{
    fn drop(&mut self)
    {
        if let Some(live) = self.live.take()
        {
            let conflict = interrupts::with_masked(|| {
                let mut cell = self.record.cell.lock();
                cell.busy = false;
                cell.live.replace(live)
            });
            if let Some(Live { value,
                               anchor }) = conflict
            {
                // Private exclusive-lease contradiction: retain both owners,
                // mark this resource/root uncertain, never drop or ACK a span.
                interrupts::with_masked(|| {
                    self.record.cell.lock().fault = Some(heap::Error::State)
                });
                self.record.heap.quarantine(heap::Error::State, HeldSpan::Live(value.span), anchor);
            }
        }
        self.record.heap.reap();
    }
}
impl Lease
{
    pub(super) fn take(heap: &Arc<Heap>, pointer: usize, layout: Layout) -> Result<Self, Error>
    {
        let key = heap.with(|s| s.addresses.get(&pointer).copied()).ok_or(Error::Pointer)?;
        let record =
            with_manager(|m| m.allocations.completing(&key).cloned())?.ok_or(Error::Pointer)?;
        if !Arc::ptr_eq(&record.heap, heap)
        {
            return Err(Error::Authority);
        }
        let live = interrupts::with_masked(|| {
                       let mut cell = record.cell.lock();
                       if let Some(cause) = cell.fault
                       {
                           return Err(Error::Span(cause));
                       }
                       if cell.busy
                       {
                           return Err(Error::Busy);
                       }
                       let value = cell.live.as_ref().ok_or(Error::Pointer)?;
                       if value.value.pointer != pointer || value.value.layout != layout
                       {
                           return Err(Error::Pointer);
                       }
                       cell.busy = true;
                       Ok(cell.live.take())
                   })?.ok_or(Error::Root)?;
        Ok(Self { record,
                  key,
                  live: Some(live),
                  _released: Released { owner: Some(heap.owner.clone()) } })
    }

    pub(super) fn pointer(&self) -> Result<usize, Error>
    {
        self.live.as_ref().map(|s| s.value.pointer).ok_or(Error::Root)
    }

    pub(super) fn layout(&self) -> Result<Layout, Error>
    {
        self.live.as_ref().map(|s| s.value.layout).ok_or(Error::Root)
    }

    pub(super) fn dispose(mut self) -> Result<(), Error>
    {
        let heap = self.record.heap.clone();
        let mut old = self.live.take();
        let mut anchor = None;
        let result = heap.with(|state| {
                             let mut cell = self.record.cell.lock();
                             if !cell.busy || cell.live.is_some() || cell.released
                             {
                                 return Err(Error::Root);
                             }
                             let Some(live) = old.as_ref()
                             else
                             {
                                 return Err(Error::Root);
                             };
                             if state.addresses.get(&live.value.pointer).copied() != Some(self.key)
                             {
                                 return Err(Error::Root);
                             }
                             let Some(Live { value:
                                                 SpanValue { span,
                                                             pointer,
                                                             layout },
                                             anchor: node }) = old.take()
                             else
                             {
                                 return Err(Error::Root);
                             };
                             match heap.release_checked(state, span)
                             {
                                 Ok(()) =>
                                 {
                                     state.addresses.remove(&pointer);
                                     cell.released = true;
                                     cell.busy = false;
                                     anchor = Some(node);
                                     Ok(())
                                 }
                                 Err(f) =>
                                 {
                                     cell.fault = Some(f.cause);
                                     old = Some(Live { value:  SpanValue { span: f.owned,
                                                                           pointer,
                                                                           layout },
                                                       anchor: node });
                                     Err(Error::Span(f.cause))
                                 }
                             }
                         });
        self.live = old;
        result?;
        drop(anchor);
        let manager = MANAGER.get().ok_or(Error::Unavailable)?;
        // The walk may have won invalidation. Its SAME pending ticket retains
        // this released record; do not manufacture another ticket or ACK.
        let removed = interrupts::with_masked(|| {
            manager.lock().allocations.remove_with_backing(self.key.id())
        });
        heap.reap();
        drop(heap);
        drop(self);
        drop(removed);
        Ok(())
    }
}

/// Old canonical record plus one private replacement. New storage is never
/// published separately and original allocation identity remains unchanged.
pub(super) struct Replacement
{
    // Drop this pin before Lease's post-field-release progress marker.
    credit:  IndexCredit,
    old:     Lease,
    new:     Option<HeldSpan>,
    pointer: usize,
    layout:  Layout
}
impl Drop for Replacement
{
    fn drop(&mut self)
    {
        let Some(new) = self.new.take()
        else
        {
            return;
        };
        let Some(old) = self.old.live.take()
        else
        {
            // No cleanup is attempted without the original restoration owner.
            // This exclusive lease admitted with no orphan; retain the actual
            // new span in its canonical Cell and refuse further mutation.
            interrupts::with_masked(|| {
                let mut cell = self.old.record.cell.lock();
                cell.orphan = Some(new);
                cell.fault = Some(heap::Error::State);
                cell.busy = false;
            });
            self.old.record.heap.with(|state| {
                                    state.fault.get_or_insert(heap::Error::State);
                                });
            return;
        };
        match cleanup(new)
        {
            Ok(heap::Released::Reusable) => self.old.live = Some(old),
            Ok(heap::Released::Empty(empty)) =>
            {
                match self.old.record.heap.retire(empty)
                {
                    Ok(()) => self.old.live = Some(old),
                    Err(empty) =>
                    {
                        let Live { value,
                                   anchor } = old;
                        self.old.record.heap.quarantine_empty(empty, anchor);
                        interrupts::with_masked(|| {
                            let mut cell = self.old.record.cell.lock();
                            cell.orphan = Some(HeldSpan::Live(value.span));
                            cell.fault = Some(heap::Error::State);
                            cell.busy = false;
                        });
                    }
                }
            }
            Err((cause, span)) =>
            {
                interrupts::with_masked(|| {
                    let mut cell = self.old.record.cell.lock();
                    cell.orphan = Some(span);
                    cell.fault = Some(cause);
                });
                self.old.live = Some(old);
            }
        }
    }
}
impl Replacement
{
    pub(super) fn prepare(old: Lease, layout: Layout) -> Result<Self, Error>
    {
        old.record.heap.admission()?;
        // Erasing an old hash does not guarantee free insertion capacity:
        // tombstones may retain growth debt. Reserve actual headroom before
        // new backing/copy and the irreversible old-span release.
        let credit = Heap::index_credit(&old.record.heap)?;
        let reservation = Heap::reserve(&old.record.heap, layout)?;
        let mut this = Self { credit,
                              old,
                              new: Some(HeldSpan::Reserved(reservation)),
                              pointer: 0,
                              layout };
        let Some(HeldSpan::Reserved(span)) = this.new.as_ref()
        else
        {
            return Err(Error::Root);
        };
        let info = span.info()?;
        this.pointer = span.base().checked_add(info.offset).ok_or(Error::Layout)?;
        if info.layout != layout || this.pointer == 0 || this.pointer % layout.align() != 0
        {
            return Err(Error::Pointer);
        }
        let span = match this.new.take()
        {
            Some(HeldSpan::Reserved(span)) => span,
            other =>
            {
                this.new = other;
                return Err(Error::Root);
            }
        };
        match span.commit()
        {
            Ok(span) => this.new = Some(HeldSpan::Live(span)),
            Err(f) =>
            {
                this.new = Some(HeldSpan::Reserved(f.owned));
                return Err(Error::Span(f.cause));
            }
        }
        Ok(this)
    }

    pub(super) fn copy_bounds(&self) -> Result<(usize, usize, usize), Error>
    {
        Ok((self.old.pointer()?, self.pointer, self.old.layout()?.size().min(self.layout.size())))
    }

    pub(super) fn commit(mut self) -> Result<usize, Error>
    {
        let heap = self.old.record.heap.clone();
        let old = self.old.live.take().ok_or(Error::Root)?;
        let new = match self.new.take()
        {
            Some(HeldSpan::Live(span)) => span,
            other =>
            {
                self.new = other;
                self.old.live = Some(old);
                return Err(Error::Root);
            }
        };
        // The caller already copied under both actual span pins outside locks.
        // In-flight stop settlement keeps the original canonical record/ticket;
        // owner-walk cannot remove a Cell while this actual lease is busy.
        let result =
            heap.with(|state| {
                    let mut cell = self.old.record.cell.lock();
                    let required = state.addresses.len().checked_add(state.credits);
                    if !self.credit.active
                       || !Arc::ptr_eq(&self.credit.heap, &heap)
                       || state.credits == 0
                       || required.is_none_or(|required| state.addresses.capacity() < required)
                       || state.addresses.get(&old.value.pointer).copied() != Some(self.old.key)
                       || state.addresses.contains_key(&self.pointer)
                       || !cell.busy
                       || cell.live.is_some()
                       || cell.orphan.is_some()
                       || cell.released
                    {
                        return Err((Error::Root, old, new));
                    }
                    let Live { value:
                                   SpanValue { span,
                                               pointer,
                                               layout },
                               anchor } = old;
                    // Fresh checked release, not observational preflight, is the FIRST
                    // irreversible effect. Every later operation is a prepared move.
                    match heap.release_checked(state, span)
                    {
                        Err(f) =>
                        {
                            cell.fault = Some(f.cause);
                            Err((Error::Span(f.cause),
                                 Live { value: SpanValue { span: f.owned,
                                                           pointer,
                                                           layout },
                                        anchor },
                                 new))
                        }
                        Ok(()) =>
                        {
                            state.addresses.remove(&pointer);
                            state.addresses.insert(self.pointer, self.old.key);
                            state.credits -= 1;
                            self.credit.active = false;
                            cell.live = Some(Live { value: SpanValue { span:    new,
                                                                       pointer: self.pointer,
                                                                       layout:  self.layout },
                                                    anchor });
                            cell.busy = false;
                            Ok(())
                        }
                    }
                });
        match result
        {
            Err((cause, old, new)) =>
            {
                self.old.live = Some(old);
                self.new = Some(HeldSpan::Live(new));
                Err(cause)
            }
            Ok(()) =>
            {
                heap.reap();
                Ok(self.pointer)
            }
        }
    }
}

/// The invalidate-first walk supplies this original ticket; it is never
/// reconstructed from a process number or an author-supplied allocation ID.
pub(super) fn release_for_cleanup(record: &Arc<Allocation>,
                                  ticket: &mac_object_table::domain::ReleasedResource)
                                  -> Result<(), CleanupCause>
{
    let heap = &record.heap;
    if ticket.key().owner() != heap.owner.owner()
    {
        return Err(CleanupCause::Fault(heap::Error::State));
    }
    let pointer = interrupts::with_masked(|| {
        let cell = record.cell.lock();
        if let Some(cause) = cell.fault
        {
            return Err(CleanupCause::Fault(cause));
        }
        if cell.orphan.is_some()
        {
            return Err(CleanupCause::Fault(heap::Error::Quarantined));
        }
        if cell.busy
        {
            return Err(CleanupCause::Mutation);
        }
        if cell.released && cell.live.is_none()
        {
            return Ok(None);
        }
        cell.live.as_ref().map(|live| Some(live.value.pointer)).ok_or(CleanupCause::Mutation)
    })?;
    let Some(pointer) = pointer
    else
    {
        return Ok(());
    };
    let key = heap.with(|state| state.addresses.get(&pointer).copied())
                  .ok_or(CleanupCause::Fault(heap::Error::State))?;
    let genuine =
        with_manager(|manager| {
            manager.allocations.completing(&key).is_some_and(|actual| Arc::ptr_eq(actual, record))
        }).map_err(|_| CleanupCause::Unavailable)?;
    if !genuine
    {
        return Err(CleanupCause::Fault(heap::Error::State));
    }
    let live = interrupts::with_masked(|| {
        let mut cell = record.cell.lock();
        if cell.busy || cell.fault.is_some() || cell.orphan.is_some()
        {
            return Err(CleanupCause::Mutation);
        }
        let live = cell.live.take().ok_or(CleanupCause::Mutation)?;
        cell.busy = true;
        Ok(live)
    })?;
    let lease = Lease { record: record.clone(),
                        key,
                        live: Some(live),
                        _released: Released { owner: Some(heap.owner.clone()) } };
    lease.dispose().map_err(|cause| {
                       match cause
                       {
                           Error::Span(cause) => CleanupCause::Fault(cause),
                           Error::Busy => CleanupCause::Mutation,
                           _ => CleanupCause::Fault(heap::Error::State)
                       }
                   })
}

pub(super) fn allocation_permit(heap: &Heap)
                                -> Result<PublicationPermit<NativeInvocationGate>, Error>
{
    loop
    {
        heap.admission()?;
        let plan = with_manager(|m| m.allocations.plan_publication(&heap.owner))??;
        let prepared = heap.kernel(|| plan.prepare())?;
        match with_manager(|m| m.allocations.admit_prepared(prepared))?
        {
            Ok((permit, retired)) =>
            {
                retired.dispose();
                return Ok(permit);
            }
            Err(refused) =>
            {
                let cause = refused.cause;
                drop(refused.preparation);
                if cause != PreparedError::CapacityChanged
                {
                    return Err(Error::Publication(cause));
                }
            }
        }
    }
}

//! Permanent allocation-free System authority. Heap receipts describe ordinary
//! heap occupancy; physical Foundation receipts attribute separate page leases.
//! Neither origin creates a synthetic domain account or augments the other.
#![forbid(unsafe_code)]
use spinning_top::Spinlock;

use super::{Error, SystemClass};
use crate::machine::interrupts::with_masked;

#[derive(Clone, Copy)]
pub(super) enum Category
{
    System(SystemClass),
    Domain
}
struct State
{
    capacity:            u64,
    admitted:            u64,
    foundation:          u64,
    ordinary:            u64,
    domains:             u64,
    // Attribution of separate page-pool leases, never heap occupancy/capacity.
    foundation_physical: u64
}
impl State
{
    fn counter(&mut self, category: Category) -> &mut u64
    {
        match category
        {
            Category::System(SystemClass::Foundation) => &mut self.foundation,
            Category::System(SystemClass::Ordinary) => &mut self.ordinary,
            Category::Domain => &mut self.domains
        }
    }
}
static STATE: Spinlock<State> = Spinlock::new(State { capacity:            0,
                                                      admitted:            0,
                                                      foundation:          0,
                                                      ordinary:            0,
                                                      domains:             0,
                                                      foundation_physical: 0 });

#[derive(Clone, Copy)]
pub(crate) struct Snapshot
{
    pub(crate) capacity:            u64,
    pub(crate) admitted:            u64,
    pub(crate) foundation:          u64,
    pub(crate) ordinary:            u64,
    pub(crate) domains:             u64,
    pub(crate) foundation_physical: u64
}
pub(super) fn snapshot() -> Snapshot
{
    with_masked(|| {
        let state = STATE.lock();
        Snapshot { capacity:            state.capacity,
                   admitted:            state.admitted,
                   foundation:          state.foundation,
                   ordinary:            state.ordinary,
                   domains:             state.domains,
                   foundation_physical: state.foundation_physical }
    })
}

pub(super) fn initialize(capacity: u64)
{
    // CPU storage is admitted before the heap. Its existing physical receipts
    // remain live; heap initialization changes only the heap capacity.
    with_masked(|| STATE.lock().capacity = capacity);
}
pub(super) fn reserve(category: Category, bytes: u64) -> Result<Receipt, Error>
{
    with_masked(|| {
        let mut state = STATE.lock();
        let next = state.admitted
                        .checked_add(bytes)
                        .filter(|&total| total <= state.capacity)
                        .ok_or(Error::SystemLimit { used:      state.admitted,
                                                    limit:     state.capacity,
                                                    requested: bytes })?;
        // Each category is a subset of the checked aggregate, so its addition
        // is representable. Nothing allocates or drops capabilities in here.
        *state.counter(category) += bytes;
        state.admitted = next;
        Ok(Receipt { category,
                     bytes })
    })
}
pub(super) struct Receipt
{
    category: Category,
    bytes:    u64
}
impl Receipt
{
    pub(super) fn category(&self) -> Category
    {
        self.category
    }
}
impl Drop for Receipt
{
    fn drop(&mut self)
    {
        let valid = with_masked(|| {
            let mut state = STATE.lock();
            let Some(total) = state.admitted.checked_sub(self.bytes)
            else
            {
                return false;
            };
            let Some(category) = state.counter(self.category).checked_sub(self.bytes)
            else
            {
                return false;
            };
            state.admitted = total;
            *state.counter(self.category) = category;
            true
        });
        if !valid
        {
            // A unique receipt refund exceeding admission is System accounting
            // corruption, not an ordinary cleanup refusal. Report after unlock
            // through the independent terminal path; never wrap or lose it.
            crate::machine::emergency::enter(crate::emergency::Kind::SystemPanic,
                                             format_args!("Resident allocation receipt exceeds \
                                                           its original admission"));
        }
    }
}

/// Unique attribution of an actual rounded page-pool lease. Only the sealed
/// Foundation owner issues this; it is not a heap reservation or a page token.
pub(super) struct FoundationPhysicalReceipt
{
    bytes: u64
}
pub(super) fn reserve_foundation_physical(
    bytes: u64)
    -> Result<FoundationPhysicalReceipt, super::foundation::Error>
{
    with_masked(|| {
        let mut state = STATE.lock();
        let next =
            state.foundation_physical
                 .checked_add(bytes)
                 .ok_or(super::foundation::Error::CounterOverflow { used:
                                                                        state.foundation_physical,
                                                                    requested: bytes })?;
        state.foundation_physical = next;
        Ok(FoundationPhysicalReceipt { bytes })
    })
}
impl Drop for FoundationPhysicalReceipt
{
    fn drop(&mut self)
    {
        let valid = with_masked(|| {
            let mut state = STATE.lock();
            let Some(next) = state.foundation_physical.checked_sub(self.bytes)
            else
            {
                return false;
            };
            state.foundation_physical = next;
            true
        });
        if !valid
        {
            // No allocator/counter lock is held across the terminal path.
            crate::machine::emergency::enter(crate::emergency::Kind::SystemPanic,
                                             format_args!("System Foundation physical refund \
                                                           exceeds its original attribution"));
        }
    }
}

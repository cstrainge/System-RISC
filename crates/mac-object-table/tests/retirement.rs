//! Retirement wakes are change-driven, after real destruction and metadata
//! unlock. They are neither periodic polling nor fabricated I/O completion.
use std::cell::{Cell, RefCell};

use mac_object_table::{domain::{CapacityPolicy, DomainId, DomainKind, KindCode, OwnerIndex},
                       shared::{InvocationGate, OwnerRef},
                       store::Store};
thread_local! {
    static NOTICES: Cell<usize> = const { Cell::new(0) };
    static PAYLOAD: Cell<bool> = const { Cell::new(false) };
    static OWNER: RefCell<Option<OwnerRef<Gate>>> = const { RefCell::new(None) };
}
struct Gate;
impl InvocationGate for Gate
{
    fn run<R>(run: impl FnOnce() -> R) -> R
    {
        run()
    }

    fn released(owner: DomainId)
    {
        assert!(!PAYLOAD.get(), "retirement notification preceded actual payload destruction");
        OWNER.with_borrow(|own| {
                 let own = own.as_ref().unwrap();
                 assert_eq!(owner, own.owner());
                 assert!(!own.accepting());
                 assert_eq!(own.summary().pending,
                            0,
                            "callback must run after ACK and outside the metadata lock");
             });
        NOTICES.set(NOTICES.get() + 1);
    }
}
struct Payload;
impl Drop for Payload
{
    fn drop(&mut self)
    {
        PAYLOAD.set(false);
    }
}
const KIND: KindCode = KindCode(*b"WAKE");
fn owner() -> OwnerRef<Gate>
{
    let owner = OwnerRef::try_new(OwnerIndex::issue(DomainKind::Process).unwrap()).unwrap();
    NOTICES.set(0);
    OWNER.with_borrow_mut(|slot| *slot = Some(owner.clone()));
    owner
}
#[test]
fn live_owner_completion_does_not_wake_the_reaper()
{
    let owner = owner();
    let mut store = Store::new(KIND, CapacityPolicy::GrowableUntilMemory);
    PAYLOAD.set(true);
    let id = store.insert(&owner, Payload).unwrap();
    drop(store.remove(id));
    assert!(!PAYLOAD.get());
    assert_eq!(NOTICES.get(), 0);
    OWNER.with_borrow_mut(|slot| *slot = None);
}
#[test]
fn stopped_owner_wakes_once_after_actual_guard_destruction()
{
    let owner = owner();
    let mut store = Store::new(KIND, CapacityPolicy::GrowableUntilMemory);
    PAYLOAD.set(true);
    let _id = store.insert(&owner, Payload).unwrap();
    owner.stop_admission();
    let ticket = owner.invalidate_next().unwrap();
    let guard = store.release(ticket).unwrap();
    assert_eq!(NOTICES.get(), 0, "invalidation is not cleanup completion");
    assert!(PAYLOAD.get());
    drop(guard);
    assert_eq!(NOTICES.get(), 1);
    OWNER.with_borrow_mut(|slot| *slot = None);
}

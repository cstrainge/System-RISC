//! Live account metadata is fallible; admitted reserve/settle/refund never allocates.
#[path = "../../mac-toolbox/tests/support/allocations.rs"]
mod allocations;
use core::sync::atomic::{AtomicUsize, Ordering};

use allocations::Observe;
use mac_kernel_model::{account::{Account, Error},
                       policy::{ConfiguredRange, MemoryClass}};
use mac_object_table::{domain::{DomainKind, OwnerIndex},
                       shared::{InvocationGate, OwnerRef}};
static WAKES: AtomicUsize = AtomicUsize::new(0);
struct HostGate;
impl InvocationGate for HostGate
{
    fn run<R>(operation: impl FnOnce() -> R) -> R
    {
        operation()
    }

    fn released(_: mac_kernel_model::DomainId)
    {
        WAKES.fetch_add(1, Ordering::Relaxed);
    }
}
fn owner() -> OwnerRef<HostGate>
{
    OwnerRef::try_new(OwnerIndex::issue(DomainKind::Process).unwrap()).unwrap()
}
fn limit(bytes: u64) -> ConfiguredRange
{
    ConfiguredRange::new(0, bytes, bytes).unwrap()
}
#[test]
fn refused_account_metadata_preserves_the_real_owner_and_retries()
{
    let owner = owner();
    let observed = Observe::fail_at(1);
    let refused = Account::try_new(owner.clone(), limit(128), limit(128));
    let count = observed.finish();
    assert_eq!(refused.err(), Some(Error::Metadata { owner: owner.owner() }));
    assert_eq!(count, 1, "account admission owns exactly one fallible Arc allocation");
    assert!(owner.accepting());
    assert!(Error::Metadata { owner: owner.owner() }.to_string()
                                                    .contains("metadata allocation refused"));
    let account = Account::try_new(owner.clone(), limit(128), limit(128)).unwrap();
    assert_eq!(account.owner(), owner.owner());
    assert!(account.close_and_ready());
    owner.stop_admission();
    let observed = Observe::begin(true, None);
    let stopped = Account::try_new(owner.clone(), limit(128), limit(128));
    let count = observed.finish();
    assert_eq!(stopped.err(), Some(Error::Closed { owner: owner.owner() }));
    assert_eq!(count, 0, "stopped authority must refuse before metadata admission");
    assert!(Error::Closed { owner: owner.owner() }.to_string().contains("stopping"));
}
#[test]
fn admitted_receipts_settle_close_and_refund_without_heap_or_current_selection()
{
    let account = Account::try_new(owner(), limit(128), limit(128)).unwrap();
    let observed = Observe::begin(true, None);
    let exercised = match account.reserve_kernel(64)
    {
        Ok(reserved) =>
        {
            let committed = reserved.commit();
            let snapshot = account.kernel();
            let identity = committed.owner();
            let class = committed.class();
            let bytes = committed.bytes();
            let budget = account.reserve_kernel(65).err();
            let before = account.close_and_ready();
            let closed = account.reserve_application(0).err();
            drop(committed);
            let after = account.close_and_ready();
            Ok((snapshot, identity, class, bytes, budget, before, closed, after))
        }
        Err(error) => Err(error)
    };
    let count = observed.finish();
    let (snapshot, identity, class, bytes, budget, before, closed, after) = exercised.unwrap();
    assert_eq!(count, 0,
               "reserve, settlement, snapshots, close and Drop must reuse admitted backing");
    assert_eq!(snapshot.reserved(), 0);
    assert_eq!(snapshot.committed(), 64);
    assert_eq!(identity, account.owner());
    assert_eq!(class, MemoryClass::Kernel);
    assert_eq!(bytes, 64);
    assert!(matches!(budget, Some(Error::Budget(error)) if error.used == 64 && error.requested == 65 && error.limit == 128));
    assert!(!before);
    assert_eq!(closed, Some(Error::Closed { owner: account.owner() }));
    assert!(after);
    assert_eq!(account.kernel().committed(), 0);
    assert!(WAKES.load(Ordering::Relaxed) > 0);
}

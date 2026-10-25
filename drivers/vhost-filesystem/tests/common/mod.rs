//! Resident-token test issuer uses the shared native store, never a local ID counter.
use mac_object_table::{domain::{CapacityPolicy, DomainKind, KindCode, OwnerIndex, PoolError},
                       shared::{InvocationGate, OwnerRef},
                       store::Store};
struct Gate;
impl InvocationGate for Gate
{
    fn run<R>(f: impl FnOnce() -> R) -> R
    {
        f()
    }
}
pub struct Tokens
{
    owner: OwnerRef<Gate>,
    store: Store<(), Gate>
}
impl Tokens
{
    pub fn new() -> Self
    {
        Self { owner: OwnerRef::try_new(OwnerIndex::issue(DomainKind::Process).unwrap()).unwrap(),
               store: Store::new(KindCode(*b"FDWR"), CapacityPolicy::GrowableUntilMemory) }
    }

    pub fn remove(&mut self, id: u64)
    {
        drop(self.store.remove(id));
    }

    pub fn insert(&mut self, value: ()) -> Result<u64, PoolError>
    {
        self.store.insert(&self.owner, value)
    }
}

//! Test transport correlation uses the real native identity facility. This is
//! only an adapter for isolated driver peers, not another request implementation.
use mac_object_table::{domain::{CapacityPolicy, DomainKind, KindCode, OwnerIndex, PoolError},
                       shared::{InvocationGate, OwnerRef},
                       store::Store};
pub struct Gate;
impl InvocationGate for Gate
{
    fn run<R>(call: impl FnOnce() -> R) -> R
    {
        call()
    }
}
pub struct Requests<T = ()>
{
    owner: OwnerRef<Gate>,
    store: Store<T, Gate>
}
impl<T> Requests<T>
{
    pub fn new() -> Self
    {
        Self::with_kind(KindCode(*b"DREQ"))
    }

    pub fn with_kind(kind: KindCode) -> Self
    {
        Self { owner: OwnerRef::try_new(OwnerIndex::issue(DomainKind::Module).unwrap()).unwrap(),
               store: Store::new(kind, CapacityPolicy::GrowableUntilMemory) }
    }

    pub fn insert(&mut self, value: T) -> Result<u64, PoolError>
    {
        self.store.insert(&self.owner, value)
    }

    pub fn remove(&mut self, id: u64) -> Option<()>
    {
        self.store.remove(id).map(drop)
    }
}

impl<T> core::ops::Deref for Requests<T>
{
    type Target = Store<T, Gate>;

    fn deref(&self) -> &Self::Target
    {
        &self.store
    }
}

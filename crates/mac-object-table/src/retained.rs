//! Read ownership which wakes retirement after its strong reference is gone.
#![forbid(unsafe_code)]
use alloc::sync::{Arc, Weak};

/// The backing owner retains one strong reference. These guards notify when a
/// read drops and only that backing owner remains. Other backing references must
/// have their own retirement checkpoint; an arbitrary unobserved Arc clone is
/// not a substitute for this guard.
pub struct Read<T>
{
    value:     Arc<T>,
    // Rust drops fields in order. Notifying before dropping value would lose
    // the last-reader wake if reclamation inspected the count immediately.
    _released: Released<T>
}
struct Released<T>
{
    value:  Weak<T>,
    notify: fn()
}
impl<T> Read<T>
{
    /// This does not allocate: it takes an existing strong reference and makes
    /// a weak observation of the same allocation.
    pub fn new(value: Arc<T>, notify: fn()) -> Self
    {
        let released = Released { value: Arc::downgrade(&value),
                                  notify };
        Self { value,
               _released: released }
    }

    /// Transfer another backing reference into its authoritative owner. That
    /// owner must account for its own release checkpoint, as described above.
    pub fn share(&self) -> Arc<T>
    {
        self.value.clone()
    }
}
impl<T> core::ops::Deref for Read<T>
{
    type Target = T;

    fn deref(&self) -> &T
    {
        &self.value
    }
}
impl<T> Drop for Released<T>
{
    fn drop(&mut self)
    {
        if self.value.strong_count() == 1
        {
            (self.notify)();
        }
    }
}

#[cfg(test)]
mod tests
{
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    #[test]
    fn last_read_notifies_after_release_and_shared_backing_remains_owned()
    {
        static NOTICES: AtomicUsize = AtomicUsize::new(0);
        fn notify()
        {
            NOTICES.fetch_add(1, Ordering::SeqCst);
        }
        let backing = Arc::new([7, 19, 41]);
        let first = Read::new(backing.clone(), notify);
        let second = Read::new(backing.clone(), notify);
        assert_eq!(Arc::strong_count(&backing), 3);
        drop(first);
        assert_eq!(NOTICES.load(Ordering::SeqCst), 0);
        assert_eq!(second[2], 41);
        let transferred = second.share();
        drop(transferred);
        drop(second);
        assert_eq!(NOTICES.load(Ordering::SeqCst), 1);
        assert_eq!(Arc::strong_count(&backing), 1);
        assert_eq!(*backing, [7, 19, 41]);
    }

    #[test]
    fn retained_read_keeps_payload_alive_after_backing_owner_leaves()
    {
        static DROPS: AtomicUsize = AtomicUsize::new(0);
        static NOTICES: AtomicUsize = AtomicUsize::new(0);
        struct Payload;
        impl Drop for Payload
        {
            fn drop(&mut self)
            {
                DROPS.fetch_add(1, Ordering::SeqCst);
            }
        }
        fn notify()
        {
            NOTICES.fetch_add(1, Ordering::SeqCst);
        }
        let backing = Arc::new(Payload);
        let read = Read::new(backing.clone(), notify);
        drop(backing);
        assert_eq!(DROPS.load(Ordering::SeqCst), 0);
        drop(read);
        assert_eq!(DROPS.load(Ordering::SeqCst), 1);
        // No backing owner remains to reclaim; this is actual destruction,
        // not a fabricated last-reader acknowledgement to a departed owner.
        assert_eq!(NOTICES.load(Ordering::SeqCst), 0);
    }
}

#[cfg(test)]
mod completion_tests
{
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::{domain::{CapacityPolicy, DomainKind, KindCode, OwnerIndex},
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
    /// A read pin is real backing ownership, not an invalidate/ACK shortcut.
    /// The Store's removed guard acknowledges only after the actual payload
    /// destructor; cleanup can also inspect the receipt after public invalidation.
    #[test]
    fn read_quiescence_precedes_payload_disposal_and_owner_completion()
    {
        static NOTICES: AtomicUsize = AtomicUsize::new(0);
        fn notify()
        {
            NOTICES.fetch_add(1, Ordering::SeqCst);
        }
        struct Payload
        {
            owner:   OwnerRef<Gate>,
            dropped: Arc<AtomicUsize>
        }
        impl Drop for Payload
        {
            fn drop(&mut self)
            {
                assert_eq!(self.owner.summary().pending, 1, "ACK preceded payload destruction");
                self.dropped.fetch_add(1, Ordering::SeqCst);
            }
        }
        let owner =
            OwnerRef::<Gate>::try_new(OwnerIndex::issue(DomainKind::Process).unwrap()).unwrap();
        let dropped = Arc::new(AtomicUsize::new(0));
        let core = Arc::new(Payload { owner:   owner.clone(),
                                      dropped: dropped.clone() });
        let read = Read::new(core.clone(), notify);
        let mut records = Store::new(KindCode(*b"THRD"), CapacityPolicy::GrowableUntilMemory);
        let publication = records.insert_with_receipt(&owner, |_| core).unwrap();
        let (id, receipt) = publication.into_parts();
        let key = records.key(id).unwrap();
        let ticket = owner.invalidate(key).unwrap();
        assert!(records.get(id).is_none());
        let backing = records.completing(&receipt).unwrap();
        assert_eq!(Arc::strong_count(backing), 2);
        assert_eq!(owner.summary().pending, 1);
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        drop(read);
        assert_eq!(NOTICES.load(Ordering::SeqCst), 1);
        assert_eq!(Arc::strong_count(records.completing(&receipt).unwrap()), 1);
        let removed = records.release(ticket).unwrap();
        assert_eq!(owner.summary().pending, 1);
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        drop(removed);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert_eq!(owner.summary().pending, 0);
    }
}

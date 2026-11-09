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



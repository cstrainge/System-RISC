//! Module mutexes exclude native preemption/interrupt callbacks before taking
//! the lock. Guards never escape the synchronous closure. Parking or entering
//! another provider while held is refused by the resident's actual thread depth.
use spinning_top::Spinlock;

use crate::{Error, Result};

pub struct Mutex<T>
{
    storage: Spinlock<T>
}
impl<T> Mutex<T>
{
    pub const fn new(value: T) -> Self
    {
        Self { storage: Spinlock::new(value) }
    }

    /// Return SERVICE_BUSY on contention without running the closure. Keep work
    /// bounded; never wait or call foreign code inside it. Native exclusion
    /// precedes lock acquisition; recursive acquisition never spins with IRQs off.
    pub fn with<R>(&self, operation: impl FnOnce(&mut T) -> R) -> Result<R>
    {
        crate::runtime::critical_section(|| {
            let mut guard = self.storage.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
            Ok(operation(&mut guard))
        })?
    }

    /// Return None when already locked. Neither the guard nor its borrow can
    /// escape the authenticated native critical section.
    pub fn try_with<R>(&self, operation: impl FnOnce(&mut T) -> R) -> Result<Option<R>>
    {
        crate::runtime::critical_section(|| {
            self.storage.try_lock().map(|mut guard| operation(&mut guard))
        })
    }
}
impl<T: Default> Default for Mutex<T>
{
    fn default() -> Self
    {
        Self::new(T::default())
    }
}

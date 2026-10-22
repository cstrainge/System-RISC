//! Bounded app-local wake registrations. No resident function pointers and no
//! allocation; event decoding wakes futures on the receiver's own OS thread.
use core::task::Waker;

use super::*;
struct Waiting
{
    id:     u32,
    item:   u32,
    flavor: FlavorType,
    waker:  Waker
}
static WAITERS: spinning_top::Spinlock<[Option<Waiting>; 32]> =
    spinning_top::Spinlock::new([const { None }; 32]);
pub(super) fn register(id: u32, item: u32, flavor: FlavorType, waker: &Waker) -> Result<()>
{
    let replacement = Waiting { id,
                                item,
                                flavor,
                                waker: waker.clone() };
    let old = {
        let mut waiters = WAITERS.lock();
        let index = waiters.iter()
                           .position(|w| {
                               w.as_ref()
                                .is_some_and(|w| w.id == id && w.item == item && w.flavor == flavor)
                           })
                           .or_else(|| waiters.iter().position(Option::is_none))
                           .ok_or(DragError::Unknown(mac_abi::NO_MEMORY))?;
        waiters[index].replace(replacement)
    };
    drop(old);
    Ok(())
}
pub(super) fn remove(id: u32, item: u32, flavor: FlavorType)
{
    let old = {
        let mut waiters = WAITERS.lock();
        waiters.iter_mut()
               .find(|w| {
                   w.as_ref().is_some_and(|w| w.id == id && w.item == item && w.flavor == flavor)
               })
               .and_then(Option::take)
    };
    drop(old);
}
pub(super) fn notify(id: u32)
{
    let mut ready: [Option<Waiting>; 32] = [const { None }; 32];
    {
        let mut waiters = WAITERS.lock();
        for (index, slot) in waiters.iter_mut().enumerate()
        {
            if slot.as_ref().is_some_and(|w| w.id == id)
            {
                ready[index] = slot.take();
            }
        }
    }
    for waiter in ready.into_iter().flatten()
    {
        waiter.waker.wake();
    }
}

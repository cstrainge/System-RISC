use crate::{sync::atomic::Atomic, sys::pal::runtime, time::Duration};

pub type Primitive = u32;
pub type Futex = Atomic<Primitive>;
pub type SmallPrimitive = u32;
pub type SmallFutex = Atomic<SmallPrimitive>;

pub fn futex_wait(futex: &Futex, expected: u32, timeout: Option<Duration>) -> bool
{
    let duration = timeout.unwrap_or(Duration::ZERO);
    unsafe {
        runtime::__system_risc_wait_u32_v1(futex.as_ptr(),
                                           expected,
                                           timeout.is_some(),
                                           duration.as_secs(),
                                           duration.subsec_nanos())
    }
}
pub fn futex_wake(futex: &Futex) -> bool
{
    unsafe { runtime::__system_risc_wake_u32_v1(futex.as_ptr(), false) }
}
pub fn futex_wake_all(futex: &Futex)
{
    unsafe {
        runtime::__system_risc_wake_u32_v1(futex.as_ptr(), true);
    }
}

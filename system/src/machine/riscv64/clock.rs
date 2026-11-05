//! QEMU virt Goldfish RTC. TIME_LOW latches TIME_HIGH; serialize both reads.
//! Sources: qemu hw/riscv/virt.c VIRT_RTC and hw/rtc/goldfish_rtc.c.
use mac_abi::{SERVICE_BUSY, Status, dates::Timestamp};
static RTC: spinning_top::Spinlock<()> = spinning_top::Spinlock::new(());
pub(crate) fn now() -> Result<Timestamp, Status>
{
    let _guard = RTC.try_lock().ok_or(SERVICE_BUSY)?;
    // SAFETY: the virt board maps this read-only RTC register pair, 32-bit aligned.
    let nanos = unsafe {
        let low = core::ptr::read_volatile(super::memory::RTC_BASE as *const u32);
        let high = core::ptr::read_volatile((super::memory::RTC_BASE + 4) as *const u32);
        (u64::from(high) << 32) | u64::from(low)
    };
    Timestamp::from_unix_parts((nanos / 1_000_000_000) as i64,
                               ((nanos % 1_000_000_000) / 1000) as u32).ok_or(mac_abi::BAD_ARGUMENT)
}

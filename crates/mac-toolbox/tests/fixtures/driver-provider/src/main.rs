#![no_std]
#![no_main]
#![cfg_attr(feature = "scheduler", feature(alloc_error_handler))]
#[cfg(all(feature = "scheduler", not(feature = "scheduler-smp")))]
mod scheduler;
#[cfg(feature = "scheduler-smp")]
mod scheduler_smp;
#[cfg(feature = "scheduler")]
#[path = "../../scheduler_support.rs"]
mod scheduler_support;
#[cfg(feature = "scheduler")]
#[global_allocator]
static ALLOCATOR: mac_driver_alloc::ForwardingAllocator = mac_driver_alloc::ForwardingAllocator;
#[cfg(feature = "scheduler")]
#[alloc_error_handler]
fn allocation_failed(layout: core::alloc::Layout) -> !
{
    mac_driver::runtime::allocation_failed(layout)
}
#[cfg(not(any(feature = "pressure", feature = "scheduler")))]
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

#[cfg(not(any(feature = "pressure", feature = "scheduler")))]
use mac_driver::{ClassId, Client, Completion, Driver, Error, Phase, Request, Result};
#[cfg(not(any(feature = "pressure", feature = "scheduler")))]
static ACTIVE: AtomicU64 = AtomicU64::new(0);
#[cfg(not(any(feature = "pressure", feature = "scheduler")))]
static CANCELS: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "pressure")]
mod pressure;
struct Device;
static DEVICE: Device = Device;
#[cfg(not(any(feature = "workers", feature = "scheduler-malformed")))]
mac_driver::export_driver!(DEVICE);
#[cfg(any(feature = "workers", feature = "scheduler-malformed"))]
mod worker_probe;
mac_driver::driver_panic_handler!();
#[cfg(not(any(feature = "pressure", feature = "scheduler")))]
impl Driver for Device
{
    fn initialize(&self, host: Client) -> Result<()>
    {
        let devices = mac_driver::devices::DeviceService::new(host)?;
        for ordinal in 0..8
        {
            match devices.probe(ordinal)
            {
                Ok(info) if info.key[..4] == 2u32.to_le_bytes() =>
                {
                    // This fixture declares the bus match, but firmware still
                    // owns boot storage. Resident ownership is an independent
                    // admission rule and must reject the real service call.
                    if info.flags & mac_driver::devices::interface::RESIDENT_OWNED == 0
                       || devices.claim(ordinal) != Err(Error(-12))
                    {
                        return Err(Error(-1));
                    }
                    return Ok(());
                }
                Ok(_) | Err(Error(-3)) => (),
                Err(error) => return Err(error)
            }
        }
        Err(Error(-3))
    }

    fn request(&self, call: Request<'_>, output: &mut [u8]) -> Result<Completion>
    {
        #[cfg(feature = "workers")]
        if call.class == ClassId(*b"TEST") && call.operation == 11 && call.phase == Phase::Start
        {
            let bytes: [u8; 8] = call.input.try_into().map_err(|_| Error(-2))?;
            let status = worker_probe::foreign_status(u64::from_le_bytes(bytes));
            assert_eq!(status,
                       mac_abi::ACCESS_DENIED,
                       "foreign worker token reached provider ownership");
            output.get_mut(..4).ok_or(Error(-2))?.copy_from_slice(&status.to_le_bytes());
            return Ok(Completion::Complete(4));
        }
        if call.class == ClassId(*b"TEST") && matches!(call.operation, 8..=10)
        {
            if call.phase == Phase::Start
            {
                if ACTIVE.compare_exchange(0, call.ticket, Ordering::AcqRel, Ordering::Acquire)
                         .is_err()
                {
                    return Err(Error(-7));
                }
                CANCELS.store(0, Ordering::Release);
                return Ok(Completion::Pending);
            }
            if ACTIVE.load(Ordering::Acquire) != call.ticket
            {
                return Err(Error(-3));
            }
            if call.phase != Phase::Cancel
            {
                return Ok(Completion::Pending);
            }
            if call.operation == 9 || CANCELS.fetch_add(1, Ordering::AcqRel) < 2
            {
                return Ok(Completion::Pending);
            }
            ACTIVE.store(0, Ordering::Release);
            if call.operation == 10
            {
                output.get_mut(..8).ok_or(Error(-2))?.copy_from_slice(b"COMMIT!!");
                return Ok(Completion::Complete(8));
            }
            // The test provider has stopped the operation before this terminal
            // error. Its error is an actual ACK, unlike absence or a timeout.
            return Err(Error(-4));
        }
        if call.class != ClassId(*b"TEST")
           || call.operation != 7
           || call.phase != Phase::Start
           || output.len() < call.input.len()
        {
            return Err(Error(-2));
        }
        for (target, source) in output.iter_mut().zip(call.input.iter().rev())
        {
            *target = *source;
        }
        Ok(Completion::Complete(call.input.len()))
    }
}

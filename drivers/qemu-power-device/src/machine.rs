//! Audited MMIO boundary for an exclusively claimed SiFive test controller.
//! Hardware protocol: QEMU v10.1.0 hw/misc/sifive_test.c and
//! include/hw/misc/sifive_test.h (FINISHER_PASS / FINISHER_RESET).
#![allow(unsafe_code)]
use mac_driver::{Client, Error, Result,
                 devices::{DeviceService, interface}};
use mac_power_interface::Action;

use super::Hardware;
pub struct Device
{
    service:   DeviceService,
    handle:    u64,
    registers: usize
}
impl Device
{
    pub fn initialize(host: Client) -> Result<Self>
    {
        let service = DeviceService::new(host)?;
        for ordinal in 0..service.count()?
        {
            let description = match service.probe(ordinal)
            {
                Ok(description) => description,
                Err(Error(mac_abi::NOT_FOUND)) => continue,
                Err(error) => return Err(error)
            };
            if description.bus.0 != *b"MMIO" || description.key != *b"sifive,test1\0\0\0\0"
            {
                continue;
            }
            if description.flags != interface::MMIO
               || description.register_bytes < 4
               || description.registers == 0
               || description.registers % 4 != 0
            {
                return Err(Error(mac_abi::BAD_ABI));
            }
            let registers =
                usize::try_from(description.registers).map_err(|_| Error(mac_abi::BAD_ABI))?;
            // All fallible format checks precede the claim. System re-probes
            // and validates the declared match before issuing its unique handle.
            let claimed = service.claim(ordinal)?;
            return Ok(Self { service,
                             handle: claimed.handle,
                             registers });
        }
        Err(Error(mac_abi::NOT_FOUND))
    }

    pub fn release(&self) -> Result<()>
    {
        self.service.release(self.handle)
    }
}
impl Hardware for Device
{
    fn trigger(&mut self, action: Action) -> Result<()>
    {
        let value = match action
        {
            Action::Shutdown => 0x5555,
            Action::Restart => 0x7777
        };
        // SAFETY: System admitted this module's hardware match and exclusively
        // claimed this aligned register window. No references to MMIO exist.
        unsafe {
            core::ptr::write_volatile(self.registers as *mut u32, value);
            core::arch::asm!("fence iorw, iorw", options(nostack));
        }
        Ok(())
    }
}

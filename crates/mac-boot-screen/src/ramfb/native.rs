//! The single audited QEMU fw_cfg DMA binding. DMA storage remains pinned even
//! on timeout; the device never receives a pointer into a departing Rust frame.
use core::{arch::asm,
           ptr::{addr_of_mut, read_volatile, write_volatile}};

use super::{Configuration, Error, bus_address};

struct Device(usize);
impl Device
{
    unsafe fn select(&self, selector: u16)
    {
        unsafe {
            write_volatile((self.0 + 8) as *mut u16, selector.to_be());
        }
    }

    fn byte(&self) -> u8
    {
        unsafe { read_volatile(self.0 as *const u8) }
    }

    fn word(&self) -> u16
    {
        u16::from_be_bytes([self.byte(), self.byte()])
    }

    fn long(&self) -> u32
    {
        u32::from_be_bytes([self.byte(), self.byte(), self.byte(), self.byte()])
    }

    fn find_file(&self, name: &[u8]) -> Result<Option<(u16, u32)>, Error>
    {
        unsafe {
            self.select(0x19);
        }
        let count = self.long();
        if count > 4096
        {
            return Err(Error::DirectoryCount { observed: count });
        }
        for _ in 0..count
        {
            let length = self.long();
            let selector = self.word();
            let _reserved = self.word();
            let mut filename = [0; 56];
            for byte in &mut filename
            {
                *byte = self.byte();
            }
            if filename.split(|b| *b == 0).next() == Some(name)
            {
                return Ok(Some((selector, length)));
            }
        }
        Ok(None)
    }
}
#[repr(C, align(8))]
struct Dma
{
    control: u32,
    length:  u32,
    address: u64
}
struct Scratch
{
    configuration: [u8; 28],
    dma:           Dma,
    busy:          bool
}
static mut SCRATCH: Scratch = Scratch { configuration: [0; 28],
                                        dma:           Dma { control: 0,
                                                             length:  0,
                                                             address: 0 },
                                        busy:          false };

/// # Safety
/// `registers` names a valid, mapped fw_cfg register window of at least24 bytes.
/// `framebuffer` is a device-visible physical address backed by pinned writable
/// guest RAM for width*height*4 bytes throughout the display lifetime. `translate`
/// proves contiguous bus addresses for the specified pinned CPU-buffer spans;
/// it must not mistake virtual addresses for physical addresses. The caller
/// serializes this bootstrap operation on one hart, before applications start.
/// Internal DMA scratch is permanent, including after a timed-out transfer.
pub unsafe fn configure(registers: usize, framebuffer: u64, width: usize, height: usize,
                        mut translate: impl FnMut(*const u8, usize) -> Option<u64>)
                        -> Result<(), Error>
{
    let configuration = Configuration::new(framebuffer, width, height)?;
    if unsafe { read_volatile(addr_of_mut!(SCRATCH.busy)) }
    {
        return Err(Error::InFlight);
    }
    let device = Device(registers);
    unsafe {
        device.select(0);
    }
    if [device.byte(), device.byte(), device.byte(), device.byte()] != *b"QEMU"
    {
        return Err(Error::Signature);
    }
    if u64::from_be(unsafe { read_volatile((registers + 16) as *const u64) })
       != 0x5145_4d55_2043_4647
    {
        return Err(Error::DmaSignature);
    }
    let (selector, length) = device.find_file(b"etc/ramfb")?.ok_or(Error::MissingFile)?;
    if length != 28
    {
        return Err(Error::ConfigurationSize { observed: length });
    }
    // SAFETY: this serialized bootstrap binding exclusively owns SCRATCH;
    // forming these raw addresses creates no reference into device-owned data.
    let (data, command) =
        unsafe { (addr_of_mut!(SCRATCH.configuration).cast::<u8>(), addr_of_mut!(SCRATCH.dma)) };
    let data_bus = bus_address(data, 28, 1, &mut translate)?;
    let command_bus = bus_address(command.cast::<u8>(), size_of::<Dma>(), 8, &mut translate)?;
    // SAFETY: the bootstrap caller exclusively owns this permanent scratch.
    // Both translations are proven before publishing any device-visible work.
    unsafe {
        write_volatile(addr_of_mut!(SCRATCH.configuration), configuration.0);
        command.write(Dma { control: ((u32::from(selector) << 16) | 8 | 16).to_be(),
                            length:  28u32.to_be(),
                            address: data_bus.to_be() });
        write_volatile(addr_of_mut!(SCRATCH.busy), true);
        asm!("fence iorw, iorw", options(nostack));
        write_volatile((registers + 16) as *mut u64, command_bus.to_be());
        for _ in 0..100_000
        {
            let control = u32::from_be(read_volatile(addr_of_mut!((*command).control)));
            if control == 0 || control & 1 != 0
            {
                asm!("fence iorw, iorw", options(nostack));
                write_volatile(addr_of_mut!(SCRATCH.busy), false);
                return if control == 0 { Ok(()) } else { Err(Error::TransferFailed { control }) };
            }
        }
    }
    // A timeout does not prove DMA quiescence. Retain the bytes and refuse any
    // later reconfiguration rather than reusing memory the device may still own.
    Err(Error::Timeout)
}

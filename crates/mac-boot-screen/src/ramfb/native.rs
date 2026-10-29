//! The single audited QEMU fw_cfg DMA binding. DMA storage remains pinned even
//! on timeout; the device never receives a pointer into a departing Rust frame.
use core::{arch::asm,
           ptr::{addr_of_mut, read_volatile, write_volatile}};

use super::{Configuration, Dma, Error, Ownership, bus_address, completion, configuration_size,
            directory_count};

static OWNER: Ownership = Ownership::new();

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
        directory_count(count)?;
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

    fn configuration_file(&self) -> Result<u16, Error>
    {
        unsafe {
            self.select(0);
        }
        if [self.byte(), self.byte(), self.byte(), self.byte()] != *b"QEMU"
        {
            return Err(Error::Signature);
        }
        if u64::from_be(unsafe { read_volatile((self.0 + 16) as *const u64) })
           != 0x5145_4d55_2043_4647
        {
            return Err(Error::DmaSignature);
        }
        let (selector, length) = self.find_file(b"etc/ramfb")?.ok_or(Error::MissingFile)?;
        configuration_size(length)?;
        Ok(selector)
    }

    /// # Safety
    /// The caller owns the complete fw_cfg device and the pinned initialized
    /// descriptor/data until reset or proven DMA completion. Both bus addresses
    /// came from the caller's admitted mapping, and descriptor alignment is8.
    unsafe fn transfer(&self, command: *mut Dma, command_bus: u64) -> Result<(), Error>
    {
        unsafe {
            asm!("fence iorw, iorw", options(nostack));
            write_volatile((self.0 + 16) as *mut u64, command_bus.to_be());
            for _ in 0..100_000
            {
                let control = u32::from_be(read_volatile(addr_of_mut!((*command).control)));
                if let Some(result) = completion(control)
                {
                    asm!("fence iorw, iorw", options(nostack));
                    return result;
                }
            }
        }
        Err(Error::Timeout)
    }
}
struct Scratch
{
    configuration: [u8; 28],
    dma:           Dma
}
static mut SCRATCH: Scratch = Scratch { configuration: [0; 28],
                                        dma:           Dma::empty() };
struct Terminal
{
    scratch:     Scratch,
    registers:   usize,
    command_bus: u64
}
static mut TERMINAL: Terminal = Terminal { scratch:     Scratch { configuration: [0; 28],
                                                                  dma:           Dma::empty() },
                                           registers:   0,
                                           command_bus: 0 };

/// Boot-prepared authority to replace the normal scanout after abandoning all
/// ordinary execution. The System pins this capability and its framebuffer;
/// ownership of all terminal DMA storage remains internal until reset.
pub struct PreparedTerminal
{
    _sealed: ()
}

impl PreparedTerminal
{
    /// Actual permanent terminal descriptor, configuration and binding storage,
    /// including Rust layout padding. The ownership byte shared with normal
    /// configuration is excluded; callers must not charge it to both paths.
    pub const fn storage_layout() -> core::alloc::Layout
    {
        core::alloc::Layout::new::<Terminal>()
    }

    /// Activate the prevalidated terminal scanout exactly once. No discovery,
    /// translation, allocation, scheduler context or lock is consulted here.
    ///
    /// # Safety
    /// All ordinary execution has been permanently abandoned. The caller is
    /// the sole terminal winner with SIE clear; all other CPUs have acknowledged
    /// irreversible stop and cannot access fw_cfg, scratch or framebuffer. The
    /// caller's permanent root maps the prepared register/static/framebuffer
    /// spans at their original CPU addresses, with coherent device-visible DMA
    /// backing pinned until reset. Activation polls direct MMIO and uses no
    /// boot-hart interrupt route, translation callback or per-CPU storage, so an
    /// admitted peer may execute the same exclusive takeover. Normal rendering
    /// never resumes, even when activation fails. InFlight requires loud shutdown
    /// rather than bypassing
    /// device ownership or retrying DMA. Completed activation retains the
    /// device and dedicated scanout; neither may be released before reset.
    pub unsafe fn activate(&self) -> Result<(), Error>
    {
        OWNER.terminal()?;
        // SAFETY: the successful phase claim acquires the immutable preparation
        // and permanently excludes normal configuration and further activation.
        // Device writes only this dedicated descriptor, never normal SCRATCH.
        unsafe {
            let registers = read_volatile(addr_of_mut!(TERMINAL.registers));
            let command_bus = read_volatile(addr_of_mut!(TERMINAL.command_bus));
            Device(registers).transfer(addr_of_mut!(TERMINAL.scratch.dma), command_bus)
        }
    }
}

/// Prepare the terminal's dedicated permanent DMA storage before ordinary
/// services. This performs discovery only; it does not change the scanout.
/// Normal configuration may proceed afterwards until terminal activation.
///
/// # Safety
/// registers names a valid mapped fw_cfg window of at least24 bytes, accessible
/// until reset. framebuffer is pinned device-readable physical guest RAM for
/// width*height*4 bytes, dedicated exclusively to the terminal until reset.
/// translate proves contiguous device-visible addresses for the supplied pinned
/// static CPU spans, never treating a virtual pointer as a physical address.
/// Preparation executes on the boot hart before ordinary services/interrupts;
/// all access to this fw_cfg device uses this binding's ownership protocol.
pub unsafe fn prepare_terminal(registers: usize, framebuffer: u64, width: usize, height: usize,
                               mut translate: impl FnMut(*const u8, usize) -> Option<u64>)
                               -> Result<PreparedTerminal, Error>
{
    let configuration = Configuration::new(framebuffer, width, height)?;
    let claim = OWNER.prepare()?;
    let selector = Device(registers).configuration_file()?;
    let (data, command) = unsafe {
        (addr_of_mut!(TERMINAL.scratch.configuration).cast::<u8>(),
         addr_of_mut!(TERMINAL.scratch.dma))
    };
    let data_bus = bus_address(data, 28, 1, &mut translate)?;
    let command_bus = bus_address(command.cast::<u8>(), size_of::<Dma>(), 8, &mut translate)?;
    // SAFETY: both translations and all device discovery succeeded while the
    // preparation claim exclusively owns this never-submitted static storage.
    unsafe {
        write_volatile(addr_of_mut!(TERMINAL.scratch.configuration), configuration.0);
        command.write(Dma::configuration(selector, data_bus));
        write_volatile(addr_of_mut!(TERMINAL.registers), registers);
        write_volatile(addr_of_mut!(TERMINAL.command_bus), command_bus);
    }
    claim.publish();
    Ok(PreparedTerminal { _sealed: () })
}

/// # Safety
/// `registers` names a valid, mapped fw_cfg register window of at least24 bytes.
/// `framebuffer` is a device-visible physical address backed by pinned writable
/// guest RAM for width*height*4 bytes throughout the display lifetime. `translate`
/// proves contiguous bus addresses for the specified pinned CPU-buffer spans;
/// it must not mistake virtual addresses for physical addresses. The caller
/// serializes this bootstrap operation on one hart, before applications start.
/// All accesses to the same fw_cfg window must obey this binding's ownership
/// protocol; arbitrary external MMIO use cannot be synchronized by this API.
/// Internal DMA scratch is permanent, including after a timed-out transfer.
pub unsafe fn configure(registers: usize, framebuffer: u64, width: usize, height: usize,
                        mut translate: impl FnMut(*const u8, usize) -> Option<u64>)
                        -> Result<(), Error>
{
    let configuration = Configuration::new(framebuffer, width, height)?;
    let mut claim = OWNER.normal()?;
    let device = Device(registers);
    let selector = device.configuration_file()?;
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
        command.write(Dma::configuration(selector, data_bus));
    }
    claim.submitted();
    let result = unsafe { device.transfer(command, command_bus) };
    if matches!(result, Ok(()) | Err(Error::TransferFailed { .. }))
    {
        claim.completed();
    }
    // Timeout retains ownership and scratch. All other outcomes prove command
    // quiescence, releasing normal configuration while preserving preparation.
    result
}

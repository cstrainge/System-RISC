//! Polling modern virtio-mmio input driver for QEMU virt, event queue zero.
use core::{arch::asm,
           marker::PhantomData,
           ptr::{addr_of, addr_of_mut, read_volatile, write_volatile},
           sync::atomic::{AtomicU8, Ordering}};

mod validation;
pub(crate) use validation::Error;
use validation::{DEVICE_COUNT, EVENT_BYTES, QUEUE_LEN as N};
// Bounded MMIO work, including on the terminal path; never wait on a timer or
// an interrupt while replacing device-owned rings.
const RESET_READS: usize = 64;

#[derive(Clone, Copy)]
struct Dma
{
    events:      u64,
    descriptors: u64,
    available:   u64,
    used:        u64
}
#[derive(Clone, Copy)]
#[repr(C)]
struct Descriptor
{
    address: u64,
    length:  u32,
    flags:   u16,
    next:    u16
}
#[derive(Clone, Copy)]
#[repr(C)]
struct InputEvent
{
    kind:  u16,
    code:  u16,
    value: u32
}
#[repr(C)]
struct Available
{
    flags:      u16,
    index:      u16,
    ring:       [u16; N],
    used_event: u16
}
#[derive(Clone, Copy)]
#[repr(C)]
struct UsedElement
{
    id:     u32,
    length: u32
}
#[repr(C)]
struct Used
{
    flags:           u16,
    index:           u16,
    ring:            [UsedElement; N],
    available_event: u16
}
#[repr(C, align(4096))]
struct Queue
{
    descriptors:    [Descriptor; N],
    available:      Available,
    used:           Used,
    events:         [InputEvent; N],
    base:           usize,
    last_used:      u16,
    next_available: u16,
    dma:            Option<Dma>
}
impl Queue
{
    const fn new() -> Self
    {
        Self { descriptors:    [Descriptor { address: 0,
                                            length:  0,
                                            flags:   0,
                                            next:    0 }; N],
               available:      Available { flags:      0,
                                           index:      0,
                                           ring:       [0; N],
                                           used_event: 0 },
               used:           Used { flags:           0,
                                      index:           0,
                                      ring:            [UsedElement { id:     0,
                                                                     length: 0 };
                                                        N],
                                      available_event: 0 },
               events:         [InputEvent { kind:  0,
                                            code:  0,
                                            value: 0 }; N],
               base:           0,
               last_used:      0,
               next_available: 0,
               dma:            None }
    }

    fn read(&self, offset: usize) -> u32
    {
        unsafe { u32::from_le(read_volatile((self.base + offset) as *const u32)) }
    }

    fn write(&self, offset: usize, value: u32)
    {
        unsafe {
            write_volatile((self.base + offset) as *mut u32, value.to_le());
        }
    }

    fn address(&self, offset: usize, address: u64)
    {
        self.write(offset, address as u32);
        self.write(offset + 4, (address >> 32) as u32);
    }

    fn prepare(&mut self, base: usize) -> Result<(), Error>
    {
        self.base = base;
        self.validate_device()?;
        let translate = |cpu: usize, bytes: usize, buffer| {
            super::paging::bus_address(cpu as u64, bytes as u64).ok_or(Error::Dma { buffer,
                                                                                    cpu,
                                                                                    bytes })
        };
        let events =
            translate(addr_of_mut!(self.events) as usize, size_of::<[InputEvent; N]>(), "events")?;
        let descriptors = translate(addr_of_mut!(self.descriptors) as usize,
                                    size_of::<[Descriptor; N]>(),
                                    "descriptors")?;
        let available = translate(addr_of_mut!(self.available) as usize,
                                  size_of::<Available>(),
                                  "available ring")?;
        let used = translate(addr_of_mut!(self.used) as usize, size_of::<Used>(), "used ring")?;
        self.dma = Some(Dma { events,
                              descriptors,
                              available,
                              used });
        Ok(())
    }

    fn validate_device(&self) -> Result<(), Error>
    {
        validation::identity(self.base, self.read(0), self.read(4), self.read(8))?;
        self.write(0x14, 1);
        validation::version_feature(self.base, self.read(0x10))?;
        self.write(0x30, 0);
        // No readiness constraint here: preparation merely reads capabilities.
        validation::queue(self.base, 0, self.read(0x34))?;
        self.validate_axes()
    }

    fn reset(&self) -> Result<(), Error>
    {
        self.write(0x70, 0);
        let mut status = 0;
        for _ in 0..RESET_READS
        {
            status = self.read(0x70);
            if status == 0
            {
                fence();
                return Ok(());
            }
        }
        Err(Error::Reset { base: self.base,
                           status })
    }

    fn activate(&mut self) -> Result<(), Error>
    {
        let dma = self.dma.ok_or(Error::NotPrepared)?;
        validation::identity(self.base, self.read(0), self.read(4), self.read(8))?;
        // No old/new ring is overwritten until the device ACKs actual reset.
        self.reset()?;
        self.write(0x70, 3); // ACKNOWLEDGE | DRIVER
        self.write(0x14, 1);
        validation::version_feature(self.base, self.read(0x10))?;
        self.write(0x24, 0);
        self.write(0x20, 0);
        self.write(0x24, 1);
        self.write(0x20, 1);
        self.write(0x70, 11);
        validation::features(self.base, self.read(0x70))?;
        self.write(0x30, 0);
        validation::queue(self.base, self.read(0x44), self.read(0x34))?;
        self.validate_axes()?;
        self.write(0x38, N as u32);
        self.available.flags = 1;
        self.available.index = 0;
        self.available.used_event = 0;
        self.used.flags = 0;
        self.used.index = 0;
        self.used.available_event = 0;
        self.used.ring.fill(UsedElement { id:     0,
                                          length: 0 });
        self.events.fill(InputEvent { kind:  0,
                                      code:  0,
                                      value: 0 });
        self.last_used = 0;
        for index in 0..N
        {
            self.descriptors[index] =
                Descriptor { address: dma.events + (index * size_of::<InputEvent>()) as u64,
                             length:  EVENT_BYTES,
                             flags:   2,
                             next:    0 };
            self.available.ring[index] = index as u16;
        }
        self.next_available = N as u16;
        self.address(0x80, dma.descriptors);
        self.address(0x90, dma.available);
        self.address(0xa0, dma.used);
        fence();
        unsafe {
            write_volatile(addr_of_mut!(self.available.index), self.next_available.to_le());
        }
        fence();
        self.write(0x44, 1);
        self.write(0x70, 15);
        self.write(0x50, 0);
        Ok(())
    }

    fn validate_axes(&self) -> Result<(), Error>
    {
        // Query optional tablet ABS_INFO. This board's device uses 0..32767.
        for axis in [0u8, 1]
        {
            unsafe {
                write_volatile((self.base + 0x100) as *mut u8, 0x12);
                write_volatile((self.base + 0x101) as *mut u8, axis);
            }
            let size = unsafe { read_volatile((self.base + 0x102) as *const u8) };
            if size >= 20
            {
                let minimum = self.read(0x108);
                let maximum = self.read(0x10c);
                validation::axis(self.base, axis, minimum, maximum)?;
            }
        }
        Ok(())
    }

    fn retire(&mut self)
    {
        if self.base != 0
        {
            self.write(0x70, self.read(0x70) | 128);
            // Buffers remain pinned even if a failed device ignores FAILED.
            // Do not reuse this slot or touch its rings again.
            self.base = 0;
        }
    }

    fn poll(&mut self, emit: &mut impl FnMut(u16, u16, u32)) -> Result<(), Error>
    {
        if self.base == 0
        {
            return Ok(());
        }
        let mut recycled = false;
        let mut recycled_ids = 0;
        for _ in 0..N
        {
            let used = unsafe { u16::from_le(read_volatile(addr_of_mut!(self.used.index))) };
            validation::used_count(self.base, self.last_used, used)?;
            if self.last_used == used
            {
                break;
            }
            fence();
            let entry =
                unsafe { read_volatile(addr_of_mut!(self.used.ring[self.last_used as usize % N])) };
            let index = validation::completion(self.base,
                                               u32::from_le(entry.id),
                                               u32::from_le(entry.length),
                                               recycled_ids)?;
            recycled_ids |= 1u64 << index;
            let event = unsafe { read_volatile(addr_of_mut!(self.events[index])) };
            emit(u16::from_le(event.kind), u16::from_le(event.code), u32::from_le(event.value));
            unsafe {
                write_volatile(addr_of_mut!(self.available.ring[self.next_available as usize % N]),
                               (index as u16).to_le());
            }
            self.next_available = self.next_available.wrapping_add(1);
            self.last_used = self.last_used.wrapping_add(1);
            recycled = true;
        }
        if recycled
        {
            fence();
            unsafe {
                write_volatile(addr_of_mut!(self.available.index), self.next_available.to_le());
            }
            fence();
            self.write(0x50, 0);
        }
        let interrupts = self.read(0x60) & 3;
        if interrupts != 0
        {
            self.write(0x64, interrupts);
        }
        // NEEDS_RESET is a configuration-change notification. Do not add a
        // status-register MMIO exit to every healthy input/idle poll.
        if interrupts & 2 != 0
        {
            let status = self.read(0x70);
            if status & (64 | 128) != 0
            {
                return Err(Error::DeviceFailed { base: self.base,
                                                 status });
            }
        }
        Ok(())
    }
}
fn fence()
{
    unsafe {
        asm!("fence iorw, iorw", options(nostack));
    }
}
static mut QUEUES: [Queue; DEVICE_COUNT] = [Queue::new(), Queue::new()];
static mut COUNT: usize = 0;

fn devices(mut visit: impl FnMut(usize))
{
    for slot in 0..super::memory::VIRTIO_MMIO_SLOTS
    {
        let base =
            (super::memory::VIRTIO_MMIO_BASE + slot * super::memory::VIRTIO_MMIO_STRIDE) as usize;
        let read = |offset| unsafe { u32::from_le(read_volatile((base + offset) as *const u32)) };
        if read(0) == 0x7472_6976 && read(8) == 18
        {
            visit(base);
        }
    }
}

pub fn initialize() -> usize
{
    let queues = addr_of_mut!(QUEUES) as *mut Queue;
    let mut count = 0;
    let mut ready = 0;
    devices(|base| {
        if count < DEVICE_COUNT
        {
            // Each discovered endpoint owns one permanent slot, even when it
            // refuses reset. Never lend possibly DMA-owned buffers to another.
            let endpoint = count;
            count += 1;
            let queue = unsafe { &mut *queues.add(endpoint) };
            match queue.prepare(base).and_then(|()| queue.activate())
            {
                Ok(()) => ready += 1,
                Err(error) =>
                {
                    queue.retire();
                    serial!("VIRTIO_INPUT_REJECTED endpoint={endpoint}: {error}");
                }
            }
        }
    });
    unsafe {
        COUNT = count;
    }
    ready
}
pub fn poll(mut emit: impl FnMut(usize, u16, u16, u32))
{
    let queues = addr_of_mut!(QUEUES) as *mut Queue;
    for index in 0..unsafe { COUNT }
    {
        unsafe {
            let queue = &mut *queues.add(index);
            if let Err(error) = queue.poll(&mut |kind, code, value| emit(index, kind, code, value))
            {
                queue.retire();
                serial!("VIRTIO_INPUT_RETIRED endpoint={index}: {error}");
            }
        }
    }
}

const UNPREPARED: u8 = 0;
const PREPARING: u8 = 1;
const PREPARED: u8 = 2;
const TAKEN: u8 = 3;

// This storage shares neither normal queue bytes nor normal queue references.
// Its real aligned extent (including control metadata/padding) is reported to
// the bootstrap's emergency manifest. It is pinned for the whole boot.
struct TerminalStorage
{
    queues: [Queue; DEVICE_COUNT],
    state:  AtomicU8
}
static mut TERMINAL: TerminalStorage = TerminalStorage { queues: [Queue::new(), Queue::new()],
                                                         state:  AtomicU8::new(UNPREPARED) };
const _: () = {
    assert!(size_of::<InputEvent>() == EVENT_BYTES as usize);
    assert!(size_of::<Queue>() == 4096);
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct TerminalLayout
{
    pub bytes:     usize,
    pub alignment: usize,
    pub devices:   usize
}

/// Admit the platform's fixed terminal input storage after paging bootstrap,
/// before ordinary input starts. No queue address or DRIVER_OK is published.
/// The exact preparation stays private; copied layout facts grant no takeover.
pub(crate) fn prepare_terminal() -> Result<TerminalLayout, Error>
{
    super::interrupts::with_masked(|| {
        // SAFETY: only this atomic field is borrowed, never the queue storage.
        let state = unsafe { &*addr_of!(TERMINAL.state) };
        match state.compare_exchange(UNPREPARED, PREPARING, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => (),
            Err(TAKEN) => return Err(Error::AlreadyTaken),
            Err(_) => return Err(Error::AlreadyPrepared)
        }
        // SAFETY: PREPARING exclusively owns the never-published terminal
        // queues; the raw projection creates no whole-storage reference.
        let queues = unsafe { addr_of_mut!(TERMINAL.queues) as *mut Queue };
        let mut count = 0;
        let mut error = None;
        devices(|base| {
            if count < DEVICE_COUNT
            {
                let endpoint = count;
                count += 1;
                // SAFETY: the unique preparation state excludes all terminal
                // handles. Dedicated rings have never been device-published.
                let result = unsafe { (&mut *queues.add(endpoint)).prepare(base) };
                if error.is_none()
                {
                    error = result.err();
                }
            }
        });
        if error.is_none() && count != DEVICE_COUNT
        {
            error = Some(Error::DeviceCount { found: count });
        }
        if let Some(error) = error
        {
            state.store(UNPREPARED, Ordering::Release);
            return Err(error);
        }
        state.store(PREPARED, Ordering::Release);
        Ok(TerminalLayout { bytes:     size_of::<TerminalStorage>(),
                            alignment: align_of::<TerminalStorage>(),
                            devices:   DEVICE_COUNT })
    })
}

/// Exclusive terminal-only access. Raw-pointer phantom ownership prevents Send
/// and Sync, so the handle cannot transfer the hart's device ownership.
pub(crate) struct TerminalInput
{
    endpoints: validation::TerminalEndpoints,
    _hart:     PhantomData<*mut ()>
}

/// Abandon normal input and consume the one boot-admitted terminal preparation.
///
/// # Safety
/// The caller is the sole authenticated terminal winner, with SIE disabled,
/// on its dedicated stack, after every other discovered CPU acknowledged actual
/// irreversible M-mode stop. It never resumes normal input or interrupted code.
/// Register windows, dedicated queues and their pretranslated DMA backing stay
/// pinned and mapped in this admitted CPU's permanent root until reset. Polling
/// uses no boot-hart interrupt route or firmware callback: an admitted peer may
/// own this one-shot takeover under the same exclusive device contract.
/// Ordinary Queue references may be suspended: they are never borrowed here.
/// Each device must ACK reset before any terminal ring is modified/published;
/// reset/refusal errors retain the backing and never authorize ring reuse.
pub(crate) unsafe fn terminal_takeover() -> Result<TerminalInput, Error>
{
    let state = unsafe { &*addr_of!(TERMINAL.state) };
    match state.compare_exchange(PREPARED, TAKEN, Ordering::AcqRel, Ordering::Acquire)
    {
        Ok(_) => (),
        Err(TAKEN) => return Err(Error::AlreadyTaken),
        Err(_) => return Err(Error::NotPrepared)
    }
    // SAFETY: the successful one-shot state transition gives this invocation
    // exclusive terminal queue access under the caller's takeover contract.
    let queues = unsafe { addr_of_mut!(TERMINAL.queues) as *mut Queue };
    let mut admission = [Ok(()); DEVICE_COUNT];
    for endpoint in 0..DEVICE_COUNT
    {
        // SAFETY: unique terminal takeover, dedicated storage, no normal queue
        // borrow. activate requires reset ACK before modifying/publishing rings.
        let queue = unsafe { &mut *queues.add(endpoint) };
        admission[endpoint] = queue.activate();
        if admission[endpoint].is_err()
        {
            queue.retire();
        }
    }
    let mut input = TerminalInput { endpoints:
                                        validation::TerminalEndpoints::from_admission(admission),
                                    _hart:     PhantomData };
    if !input.has_live_device()
    {
        return Err(input.endpoints
                        .take_errors()
                        .into_iter()
                        .flatten()
                        .next()
                        .unwrap_or(Error::DeviceCount { found: 0 }));
    }
    Ok(input)
}

impl TerminalInput
{
    pub(crate) fn has_live_device(&self) -> bool
    {
        self.endpoints.has_live_device()
    }

    /// Poll both endpoints even if one fails. Each retired endpoint reports one
    /// typed cause; the next call can still service the other endpoint. The
    /// callback must remain allocation-free and must not resume normal services.
    pub(crate) fn poll(&mut self, mut emit: impl FnMut(usize, u16, u16, u32))
                       -> [Option<Error>; DEVICE_COUNT]
    {
        // SAFETY: this non-Send handle exclusively owns the terminal queues;
        // neither ordinary polling nor another takeover can access them.
        let queues = unsafe { addr_of_mut!(TERMINAL.queues) as *mut Queue };
        for endpoint in 0..DEVICE_COUNT
        {
            if !self.endpoints.is_live(endpoint)
            {
                continue;
            }
            // SAFETY: this unique, non-Send handle is the only terminal poller.
            let queue = unsafe { &mut *queues.add(endpoint) };
            if let Err(error) =
                queue.poll(&mut |kind, code, value| emit(endpoint, kind, code, value))
            {
                queue.retire();
                self.endpoints.retire(endpoint, error);
            }
        }
        self.endpoints.take_errors()
    }
}

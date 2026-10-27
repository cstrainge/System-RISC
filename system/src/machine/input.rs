//! Polling modern virtio-mmio input driver for QEMU virt, event queue zero.
use core::{arch::asm,
           ptr::{addr_of_mut, read_volatile, write_volatile}};

const N: usize = 64;
struct DmaError
{
    buffer: &'static str,
    cpu:    usize,
    bytes:  usize
}
impl core::fmt::Display for DmaError
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        write!(f,
               "input {} CPU buffer {:#x}+{} bytes has no contiguous bus mapping; device was not \
                published",
               self.buffer, self.cpu, self.bytes)
    }
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
    next_available: u16
}
impl Queue
{
    const fn new() -> Self
    {
        Self { descriptors:    [Descriptor { address: 0,
                                            length:  0,
                                            flags:   0,
                                            next:    0 }; N],
               available:      Available { flags:      1,
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
               next_available: 0 }
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

    fn initialize(&mut self, base: usize) -> Result<(), DmaError>
    {
        let translate = |cpu: usize, bytes: usize, buffer| {
            super::paging::bus_address(cpu as u64, bytes as u64).ok_or(DmaError { buffer,
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
        self.base = base;
        self.write(0x70, 0);
        assert_eq!(self.read(0x70), 0, "virtio reset");
        self.write(0x70, 3); // ACKNOWLEDGE | DRIVER
        self.write(0x14, 1);
        assert_ne!(self.read(0x10) & 1, 0, "VIRTIO_F_VERSION_1 required");
        self.write(0x24, 0);
        self.write(0x20, 0);
        self.write(0x24, 1);
        self.write(0x20, 1);
        self.write(0x70, 11);
        assert_ne!(self.read(0x70) & 8, 0, "virtio features rejected");
        self.write(0x30, 0);
        assert_eq!(self.read(0x44), 0);
        assert!(self.read(0x34) >= N as u32);
        self.write(0x38, N as u32);
        for index in 0..N
        {
            self.descriptors[index] =
                Descriptor { address: events + (index * size_of::<InputEvent>()) as u64,
                             length:  8,
                             flags:   2,
                             next:    0 };
            self.available.ring[index] = index as u16;
        }
        self.next_available = N as u16;
        self.address(0x80, descriptors);
        self.address(0x90, available);
        self.address(0xa0, used);
        fence();
        unsafe {
            write_volatile(addr_of_mut!(self.available.index), self.next_available.to_le());
        }
        fence();
        self.write(0x44, 1);
        self.write(0x70, 15);
        self.write(0x50, 0);
        // Query optional tablet ABS_INFO. This board's device uses 0..32767.
        for axis in [0u8, 1]
        {
            unsafe {
                write_volatile((base + 0x100) as *mut u8, 0x12);
                write_volatile((base + 0x101) as *mut u8, axis);
            }
            let size = unsafe { read_volatile((base + 0x102) as *const u8) };
            if size >= 20
            {
                assert_eq!(self.read(0x108), 0);
                assert_eq!(self.read(0x10c), 32767);
            }
        }
        Ok(())
    }

    fn poll(&mut self, emit: &mut impl FnMut(u16, u16, u32))
    {
        let mut recycled = false;
        for _ in 0..N
        {
            let used = unsafe { u16::from_le(read_volatile(addr_of_mut!(self.used.index))) };
            if self.last_used == used
            {
                break;
            }
            fence();
            let entry =
                unsafe { read_volatile(addr_of_mut!(self.used.ring[self.last_used as usize % N])) };
            let index = u32::from_le(entry.id) as usize;
            assert!(index < N && u32::from_le(entry.length) == 8,
                    "invalid virtio input completion");
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
    }
}
fn fence()
{
    unsafe {
        asm!("fence iorw, iorw", options(nostack));
    }
}
static mut QUEUES: [Queue; 2] = [Queue::new(), Queue::new()];
static mut COUNT: usize = 0;

pub fn initialize() -> usize
{
    let queues = addr_of_mut!(QUEUES) as *mut Queue;
    let mut count = 0;
    for slot in 0..super::memory::VIRTIO_MMIO_SLOTS
    {
        let base =
            (super::memory::VIRTIO_MMIO_BASE + slot * super::memory::VIRTIO_MMIO_STRIDE) as usize;
        let read = |offset| unsafe { read_volatile((base + offset) as *const u32) };
        if read(0) == 0x74726976 && read(4) == 2 && read(8) == 18 && count < 2
        {
            match unsafe { (*queues.add(count)).initialize(base) }
            {
                Ok(()) => count += 1,
                Err(error) =>
                {
                    serial!("VIRTIO_INPUT_DMA_REJECTED: {error}");
                }
            }
        }
    }
    unsafe {
        COUNT = count;
    }
    count
}
pub fn poll(mut emit: impl FnMut(usize, u16, u16, u32))
{
    let queues = addr_of_mut!(QUEUES) as *mut Queue;
    for index in 0..unsafe { COUNT }
    {
        unsafe {
            (*queues.add(index)).poll(&mut |kind, code, value| emit(index, kind, code, value));
        }
    }
}

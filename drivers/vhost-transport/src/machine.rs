//! Audited modern virtio-console MMIO/DMA adapter. Queue storage is pinned static
//! memory inside this module's admitted load reservation. CPU never creates a
//! Rust reference to a DMA ring/buffer; every device-shared access uses raw
//! pointers, ownership bookkeeping and the virtio publication fences.
#![allow(unsafe_code)]
use core::{cell::UnsafeCell,
           mem::{offset_of, size_of},
           ptr::{addr_of_mut, copy_nonoverlapping, read_volatile, write_volatile},
           sync::atomic::{AtomicBool, Ordering}};

use mac_abi::{BAD_ABI, BAD_ARGUMENT, IO_ERROR, NOT_FOUND, NOT_SUPPORTED, SERVICE_BUSY};
use mac_driver::{Client, Completion, Error, Phase, Result,
                 devices::{DeviceService, interface}};

use crate::engine::{Channel, FRAGMENT};
const N: usize = 8;
#[derive(Clone, Copy)]
#[repr(C)]
struct Descriptor
{
    address: u64,
    length:  u32,
    flags:   u16,
    next:    u16
}
#[repr(C)]
struct Available
{
    flags: u16,
    index: u16,
    ring:  [u16; N],
    event: u16
}
#[derive(Clone, Copy)]
#[repr(C)]
struct UsedEntry
{
    id:     u32,
    length: u32
}
#[repr(C)]
struct Used
{
    flags: u16,
    index: u16,
    ring:  [UsedEntry; N],
    event: u16
}
#[repr(C, align(4096))]
struct QueueMemory
{
    descriptors: [Descriptor; N],
    available:   Available,
    used:        Used,
    buffers:     [[u8; FRAGMENT]; N]
}
impl QueueMemory
{
    const fn new() -> Self
    {
        Self { descriptors: [Descriptor { address: 0,
                                         length:  0,
                                         flags:   0,
                                         next:    0 }; N],
               available:   Available { flags: 0,
                                        index: 0,
                                        ring:  [0; N],
                                        event: 0 },
               used:        Used { flags: 0,
                                   index: 0,
                                   ring:  [UsedEntry { id:     0,
                                                      length: 0 };
                                           N],
                                   event: 0 },
               buffers:     [[0; FRAGMENT]; N] }
    }
}
struct Memory(UnsafeCell<[QueueMemory; 4]>);
// SAFETY: the single Device token and outer module mutex serialize software
// access. Hardware-shared cells never acquire Rust references. Device owns RX
// buffers until used publication; CPU owns TX until available publication.
unsafe impl Sync for Memory
{
}
static MEMORY: Memory = Memory(UnsafeCell::new([const { QueueMemory::new() }; 4]));
static TAKEN: AtomicBool = AtomicBool::new(false);
#[derive(Clone, Copy)]
struct Queue
{
    owned:     [bool; N],
    available: u16,
    used:      u16
}
impl Queue
{
    const fn new() -> Self
    {
        Self { owned:     [false; N],
               available: 0,
               used:      0 }
    }
}
pub(crate) struct Device
{
    service:         DeviceService,
    claim:           u32,
    mapping:         u32,
    registers:       usize,
    dma:             u64,
    queues:          [Queue; 4],
    running:         bool,
    pending_control: u8,
    connected:       bool,
    lost:            bool,
    reset_ticket:    u32,
    live:            bool
}
fn fence()
{
    unsafe {
        core::arch::asm!("fence iorw, iorw", options(nostack));
    }
}
impl Device
{
    pub(crate) fn initialize(client: Client) -> Result<Self>
    {
        if TAKEN.swap(true, Ordering::AcqRel)
        {
            return Err(Error(SERVICE_BUSY));
        }
        let service = DeviceService::new(client)?;
        let mut found = None;
        for ordinal in 0..8
        {
            match service.probe(ordinal)
            {
                Ok(info) if info.bus.0 == *b"VMMI" && info.key[..4] == 3u32.to_le_bytes() =>
                {
                    found = Some((ordinal, info));
                    break;
                }
                Ok(_) | Err(Error(NOT_FOUND)) => (),
                Err(error) => return Err(error)
            }
        }
        let (ordinal, info) = found.ok_or(Error(NOT_FOUND))?;
        if info.register_bytes != 4096
           || info.flags & (interface::COHERENT_DMA | interface::MMIO)
              != interface::COHERENT_DMA | interface::MMIO
        {
            return Err(Error(NOT_SUPPORTED));
        }
        let claim = service.claim(ordinal)?;
        let mut reset_ticket = 0;
        if service.reset(claim.handle, &mut reset_ticket, Phase::Start)? != Completion::Complete(0)
        {
            return Err(Error(SERVICE_BUSY));
        }
        let address = MEMORY.0.get().cast::<u8>() as usize as u64;
        let mapping = service.map(claim.handle, address, size_of::<[QueueMemory; 4]>() as u64)?;
        if mapping.address != address || mapping.length != size_of::<[QueueMemory; 4]>() as u64
        {
            return Err(Error(BAD_ABI));
        }
        let mut device =
            Self { service,
                   claim: claim.handle,
                   mapping: mapping.handle,
                   registers: usize::try_from(info.registers).map_err(|_| Error(BAD_ABI))?,
                   dma: mapping.address,
                   queues: [Queue::new(); 4],
                   running: false,
                   pending_control: 1,
                   connected: false,
                   lost: false,
                   reset_ticket: 0,
                   live: true };
        device.write(0x70, 3);
        device.write(0x14, 0);
        let low = device.read(0x10);
        device.write(0x14, 1);
        let high = device.read(0x10);
        if low & 2 == 0 || high & 1 == 0
        {
            return Err(Error(NOT_SUPPORTED));
        }
        device.write(0x24, 0);
        device.write(0x20, 2);
        device.write(0x24, 1);
        device.write(0x20, 1);
        device.write(0x70, 11);
        if device.read(0x70) & 8 == 0
        {
            return Err(Error(BAD_ABI));
        }
        for queue in 0..4
        {
            device.write(0x30, queue as u32);
            if device.read(0x34) < N as u32 || device.read(0x44) != 0
            {
                return Err(Error(BAD_ABI));
            }
            device.write(0x38, N as u32);
            let pointer = device.queue_pointer(queue);
            unsafe {
                write_volatile(addr_of_mut!((*pointer).available.flags), 1u16.to_le());
            }
            let offset = queue as u64 * size_of::<QueueMemory>() as u64;
            device.address(0x80, device.dma + offset + offset_of!(QueueMemory, descriptors) as u64);
            device.address(0x90, device.dma + offset + offset_of!(QueueMemory, available) as u64);
            device.address(0xa0, device.dma + offset + offset_of!(QueueMemory, used) as u64);
            device.write(0x44, 1);
        }
        for queue in [0, 2]
        {
            for index in 0..N
            {
                device.post(queue, index, FRAGMENT, true)?;
            }
        }
        fence();
        device.write(0x70, 15);
        device.running = true;
        device.write(0x50, 0);
        device.write(0x50, 2);
        Ok(device)
    }

    fn read(&self, offset: usize) -> u32
    {
        unsafe { u32::from_le(read_volatile((self.registers + offset) as *const u32)) }
    }

    fn write(&self, offset: usize, value: u32)
    {
        unsafe {
            write_volatile((self.registers + offset) as *mut u32, value.to_le());
        }
    }

    fn address(&self, offset: usize, address: u64)
    {
        self.write(offset, address as u32);
        self.write(offset + 4, (address >> 32) as u32);
    }

    fn queue_pointer(&self, queue: usize) -> *mut QueueMemory
    {
        // Every caller uses one of the four fixed queue numbers.
        unsafe { MEMORY.0.get().cast::<QueueMemory>().add(queue) }
    }

    fn buffer(&self, queue: usize, index: usize) -> *mut u8
    {
        unsafe {
            addr_of_mut!((*self.queue_pointer(queue)).buffers).cast::<u8>().add(index * FRAGMENT)
        }
    }

    fn post(&mut self, queue: usize, index: usize, length: usize, receive: bool) -> Result<()>
    {
        if queue >= 4 || index >= N || length > FRAGMENT || self.queues[queue].owned[index]
        {
            return Err(Error(BAD_ARGUMENT));
        }
        let pointer = self.queue_pointer(queue);
        let address = self.dma
                      + (queue * size_of::<QueueMemory>()
                         + offset_of!(QueueMemory, buffers)
                         + index * FRAGMENT) as u64;
        unsafe {
            write_volatile(addr_of_mut!((*pointer).descriptors[index]),
                           Descriptor { address: address.to_le(),
                                        length:  (length as u32).to_le(),
                                        flags:   if receive { 2u16.to_le() } else { 0 },
                                        next:    0 });
        }
        self.queues[queue].owned[index] = true;
        let next = self.queues[queue].available;
        unsafe {
            write_volatile(addr_of_mut!((*pointer).available.ring[next as usize % N]),
                           (index as u16).to_le());
        }
        fence();
        self.queues[queue].available = next.wrapping_add(1);
        unsafe {
            write_volatile(addr_of_mut!((*pointer).available.index),
                           self.queues[queue].available.to_le());
        }
        fence();
        if self.running
        {
            self.write(0x50, queue as u32);
        }
        Ok(())
    }

    fn used(&mut self, queue: usize) -> Result<Option<(usize, usize)>>
    {
        let pointer = self.queue_pointer(queue);
        let index = unsafe { u16::from_le(read_volatile(addr_of_mut!((*pointer).used.index))) };
        let count = index.wrapping_sub(self.queues[queue].used);
        if count == 0
        {
            return Ok(None);
        }
        if count as usize > N
        {
            return Err(Error(BAD_ABI));
        }
        fence();
        let entry = unsafe {
            read_volatile(addr_of_mut!((*pointer).used.ring[self.queues[queue].used as usize % N]))
        };
        let id = u32::from_le(entry.id) as usize;
        let length = u32::from_le(entry.length) as usize;
        if id >= N || !self.queues[queue].owned[id]
        {
            return Err(Error(BAD_ABI));
        }
        self.queues[queue].used = self.queues[queue].used.wrapping_add(1);
        self.queues[queue].owned[id] = false;
        Ok(Some((id, length)))
    }

    fn send_queue(&mut self, queue: usize, bytes: &[u8]) -> Result<usize>
    {
        if bytes.len() > FRAGMENT
        {
            return Err(Error(BAD_ARGUMENT));
        }
        let Some(index) = self.queues[queue].owned.iter().position(|owned| !owned)
        else
        {
            return Ok(0);
        };
        // The descriptor is software-owned and the engine's source is disjoint
        // from this DMA memory. Hardware cannot read until post publishes it.
        unsafe {
            copy_nonoverlapping(bytes.as_ptr(), self.buffer(queue, index), bytes.len());
        }
        self.post(queue, index, bytes.len(), false)?;
        Ok(bytes.len())
    }

    fn receive_queue(&mut self, queue: usize, output: &mut [u8]) -> Result<usize>
    {
        let Some((index, length)) = self.used(queue)?
        else
        {
            return Ok(0);
        };
        if length > FRAGMENT || length > output.len()
        {
            return Err(Error(BAD_ABI));
        }
        // Used publication returned the buffer to software. Repost only after
        // copying; there are no live references to memory owned by the device.
        unsafe {
            copy_nonoverlapping(self.buffer(queue, index), output.as_mut_ptr(), length);
        }
        self.post(queue, index, FRAGMENT, true)?;
        Ok(length)
    }

    pub(crate) fn quiesce(&mut self) -> Result<()>
    {
        if !self.live
        {
            return Ok(());
        }
        let phase = if self.reset_ticket == 0 { Phase::Start } else { Phase::Poll };
        if self.service.reset(self.claim, &mut self.reset_ticket, phase)? == Completion::Pending
        {
            return Err(Error(SERVICE_BUSY));
        }
        self.running = false;
        if self.mapping != 0
        {
            self.service.unmap(self.mapping)?;
            self.mapping = 0;
        }
        if self.claim != 0
        {
            self.service.release(self.claim)?;
            self.claim = 0;
        }
        self.live = false;
        Ok(())
    }
}
impl Channel for Device
{
    fn poll(&mut self) -> Result<bool>
    {
        if !self.live || self.lost || self.read(0x70) & 64 != 0
        {
            return Err(Error(IO_ERROR));
        }
        for queue in [1, 3]
        {
            for _ in 0..2
            {
                if self.used(queue)?.is_none()
                {
                    break;
                }
            }
        }
        let mut bytes = [0; FRAGMENT];
        for _ in 0..2
        {
            let length = self.receive_queue(2, &mut bytes)?;
            if length == 0
            {
                break;
            }
            if length < 8
            {
                return Err(Error(BAD_ABI));
            }
            let id = u32::from_le_bytes(bytes[..4].try_into().map_err(|_| Error(BAD_ABI))?);
            let event = u16::from_le_bytes(bytes[4..6].try_into().map_err(|_| Error(BAD_ABI))?);
            let value = u16::from_le_bytes(bytes[6..8].try_into().map_err(|_| Error(BAD_ABI))?);
            if id != 0
            {
                continue;
            }
            match event
            {
                1 => self.pending_control |= 2 | 4,
                2 =>
                {
                    self.lost = true;
                    return Err(Error(IO_ERROR));
                }
                4 => self.pending_control |= 4,
                6 =>
                {
                    if value > 1
                    {
                        return Err(Error(BAD_ABI));
                    }
                    if value == 0 && self.connected
                    {
                        self.lost = true;
                        return Err(Error(IO_ERROR));
                    }
                    self.connected = value != 0;
                }
                5 | 7 => (),
                _ => return Err(Error(BAD_ABI))
            }
        }
        for _ in 0..2
        {
            let bit = if self.pending_control & 1 != 0
            {
                1
            }
            else if self.pending_control & 2 != 0
            {
                2
            }
            else if self.pending_control & 4 != 0
            {
                4
            }
            else
            {
                break;
            };
            let event: u16 = match bit
            {
                1 => 0,
                2 => 3,
                _ => 6
            };
            let mut control = [0; 8];
            control[4..6].copy_from_slice(&event.to_le_bytes());
            control[6..8].copy_from_slice(&1u16.to_le_bytes());
            if self.send_queue(3, &control)? == 0
            {
                break;
            }
            self.pending_control &= !bit;
        }
        let interrupts = self.read(0x60) & 3;
        if interrupts != 0
        {
            self.write(0x64, interrupts);
        }
        Ok(self.connected)
    }

    fn send(&mut self, bytes: &[u8]) -> Result<usize>
    {
        self.send_queue(1, bytes)
    }

    fn receive(&mut self, bytes: &mut [u8]) -> Result<usize>
    {
        self.receive_queue(0, bytes)
    }
}

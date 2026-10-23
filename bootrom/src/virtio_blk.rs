//! Modern virtio-mmio, split queue, one outstanding read/write/flush request.
use alloc::boxed::Box;
use core::{arch::asm,
           fmt::Write,
           ptr::{addr_of_mut, read_volatile, write_volatile}};

use mac_hfs::{BlockDevice, Error, Result, WritableBlockDevice};

const N: usize = 8;
#[repr(C)]
#[derive(Clone, Copy)]
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
    flags:      u16,
    index:      u16,
    ring:       [u16; N],
    used_event: u16
}
#[repr(C)]
#[derive(Clone, Copy)]
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
#[repr(C)]
struct Request
{
    kind:     u32,
    reserved: u32,
    sector:   u64
}
#[repr(C, align(4096))]
struct Queue
{
    descriptors: [Descriptor; N],
    available:   Available,
    used:        Used,
    request:     Request,
    data:        [u8; 512],
    status:      u8
}
impl Queue
{
    fn new() -> Self
    {
        Self { descriptors: [Descriptor { address: 0,
                                         length:  0,
                                         flags:   0,
                                         next:    0 }; N],
               available:   Available { flags:      1,
                                        index:      0,
                                        ring:       [0; N],
                                        used_event: 0 },
               used:        Used { flags:           0,
                                   index:           0,
                                   ring:            [UsedElement { id:     0,
                                                                  length: 0 };
                                                     N],
                                   available_event: 0 },
               request:     Request { kind:     0,
                                      reserved: 0,
                                      sector:   0 },
               data:        [0; 512],
               status:      0xff }
    }
}

pub struct Disk
{
    base:            usize,
    queue:           Box<Queue>,
    index:           u16,
    sectors:         u64,
    read_only:       bool,
    flush_supported: bool,
    failed:          bool
}

fn fence()
{
    unsafe {
        asm!("fence iorw, iorw", options(nostack));
    }
}
fn now() -> u64
{
    unsafe { read_volatile(0x0200bff8 as *const u64) }
}
pub fn is_block(base: usize) -> bool
{
    unsafe {
        read_volatile(base as *const u32) == 0x74726976
        && read_volatile((base + 8) as *const u32) == 2
    }
}

impl Disk
{
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

    fn address(&self, offset: usize, address: usize)
    {
        self.write(offset, address as u32);
        self.write(offset + 4, (address as u64 >> 32) as u32);
    }

    pub fn open(base: usize) -> Result<Self>
    {
        let mut disk = Self { base,
                              queue: Box::new(Queue::new()),
                              index: 0,
                              sectors: 0,
                              read_only: false,
                              flush_supported: false,
                              failed: false };
        if disk.read(4) != 2
        {
            return Err(Error::Malformed("virtio block requires modern MMIO version 2"));
        }
        disk.write(0x70, 0);
        if disk.read(0x70) != 0
        {
            return Err(Error::Malformed("virtio block reset failed"));
        }
        disk.write(0x70, 3);
        disk.write(0x14, 1);
        if disk.read(0x10) & 1 == 0
        {
            return Err(Error::Malformed("virtio block lacks VERSION_1"));
        }
        disk.write(0x14, 0);
        let features = disk.read(0x10);
        let read_only = features & (1 << 5);
        disk.flush_supported = features & (1 << 9) != 0;
        disk.read_only = read_only != 0;
        disk.write(0x24, 0);
        disk.write(0x20, read_only | (features & (1 << 9)));
        disk.write(0x24, 1);
        disk.write(0x20, 1);
        disk.write(0x70, 11);
        if disk.read(0x70) & 8 == 0
        {
            return Err(Error::Malformed("virtio block feature negotiation failed"));
        }
        disk.write(0x30, 0);
        if disk.read(0x34) < N as u32 || disk.read(0x44) != 0
        {
            return Err(Error::Malformed("virtio block queue is unavailable"));
        }
        disk.write(0x38, N as u32);
        let q = &mut *disk.queue;
        q.descriptors[0] = Descriptor { address: addr_of_mut!(q.request) as u64,
                                        length:  16,
                                        flags:   1,
                                        next:    1 };
        q.descriptors[1] = Descriptor { address: addr_of_mut!(q.data) as u64,
                                        length:  512,
                                        flags:   3,
                                        next:    2 };
        q.descriptors[2] = Descriptor { address: addr_of_mut!(q.status) as u64,
                                        length:  1,
                                        flags:   2,
                                        next:    0 };
        let descriptors = addr_of_mut!(q.descriptors) as usize;
        let available = addr_of_mut!(q.available) as usize;
        let used = addr_of_mut!(q.used) as usize;
        disk.address(0x80, descriptors);
        disk.address(0x90, available);
        disk.address(0xa0, used);
        fence();
        disk.write(0x44, 1);
        disk.write(0x70, 15);
        for _ in 0..8
        {
            let generation = disk.read(0xfc);
            let capacity = disk.read(0x100) as u64 | ((disk.read(0x104) as u64) << 32);
            if generation == disk.read(0xfc) && capacity != 0
            {
                disk.sectors = capacity;
                return Ok(disk);
            }
        }
        Err(Error::Malformed("virtio block capacity is unstable or empty"))
    }

    pub fn is_read_only(&self) -> bool
    {
        self.read_only || !self.flush_supported
    }
}

impl BlockDevice for Disk
{
    fn sector_count(&self) -> u64
    {
        self.sectors
    }

    fn read_sector(&mut self, sector: u64, output: &mut [u8; 512]) -> Result<()>
    {
        if sector >= self.sectors
        {
            return Err(Error::Malformed("virtio read outside disk"));
        }
        self.request(0, sector, None)?;
        for (index, byte) in output.iter_mut().enumerate()
        {
            *byte = unsafe { read_volatile(addr_of_mut!(self.queue.data[index])) };
        }
        Ok(())
    }
}

impl Disk
{
    fn request_error(&mut self, kind: u32, sector: u64, start: u64, reason: &'static str,
                     poison: bool)
                     -> Error
    {
        self.failed |= poison;
        fence();
        let q = &mut *self.queue;
        let used = unsafe { u16::from_le(read_volatile(addr_of_mut!(q.used.index))) };
        let completion = unsafe {
            read_volatile(addr_of_mut!(q.used.ring[self.index.wrapping_sub(1) as usize % N]))
        };
        let status = unsafe { read_volatile(addr_of_mut!(q.status)) };
        // Record the original failure, before later rollback/read requests report
        // only a poisoned queue. Never reuse DMA storage after uncertain completion.
        let _ = writeln!(crate::Serial,
                         "DISK_REQUEST_FAILED kind={} sector={} submitted={} used={} head={} \
                          length={} status={} device_status={:#x} elapsed_us={} poisoned={} \
                          reason={}",
                         kind,
                         sector,
                         self.index,
                         used,
                         u32::from_le(completion.id),
                         u32::from_le(completion.length),
                         status,
                         self.read(0x70),
                         now().wrapping_sub(start) / 10,
                         self.failed,
                         reason);
        Error::Malformed(reason)
    }

    fn request(&mut self, kind: u32, sector: u64, input: Option<&[u8; 512]>) -> Result<()>
    {
        if self.failed
        {
            return Err(Error::Malformed("virtio block queue is poisoned"));
        }
        let q = &mut *self.queue;
        q.request.kind = kind.to_le();
        q.request.sector = sector.to_le();
        if let Some(input) = input
        {
            q.data.copy_from_slice(input);
        }
        q.descriptors[0].next = if kind == 4 { 2 } else { 1 };
        q.descriptors[1].flags = if kind == 0 { 3 } else { 1 };
        unsafe {
            write_volatile(addr_of_mut!(q.status), 0xff);
        }
        q.available.ring[self.index as usize % N] = 0;
        self.index = self.index.wrapping_add(1);
        fence();
        unsafe {
            write_volatile(addr_of_mut!(q.available.index), self.index.to_le());
        }
        fence();
        self.write(0x50, 0);
        let start = now();
        loop
        {
            let used = unsafe { u16::from_le(read_volatile(addr_of_mut!(self.queue.used.index))) };
            if used == self.index
            {
                break;
            }
            if self.read(0x70) & 0xc0 != 0
            {
                return Err(self.request_error(kind, sector, start, "virtio block device failed",
                                              true));
            }
            if now().wrapping_sub(start) > 10_000_000
            {
                // DMA completion is uncertain. Never reuse its descriptors or data;
                // rollback will fail truthfully rather than race an in-flight write.
                return Err(self.request_error(kind, sector, start,
                                              "virtio block request timed out", true));
            }
            core::hint::spin_loop();
        }
        fence();
        let q = &mut *self.queue;
        let completion = unsafe {
            read_volatile(addr_of_mut!(q.used.ring[self.index.wrapping_sub(1) as usize % N]))
        };
        if u32::from_le(completion.id) != 0
           || u32::from_le(completion.length) != (if kind == 0 { 513 } else { 1 })
        {
            return Err(self.request_error(kind, sector, start,
                                          "virtio block completion is invalid", true));
        }
        if unsafe { read_volatile(addr_of_mut!(q.status)) } != 0
        {
            return Err(self.request_error(kind, sector, start, "virtio block request failed",
                                          false));
        }
        let pending = self.read(0x60);
        if pending != 0
        {
            self.write(0x64, pending);
        }
        Ok(())
    }
}
impl WritableBlockDevice for Disk
{
    fn is_read_only(&self) -> bool
    {
        self.read_only || !self.flush_supported
    }

    fn write_sector(&mut self, sector: u64, input: &[u8; 512]) -> Result<()>
    {
        if WritableBlockDevice::is_read_only(self)
        {
            return Err(Error::Malformed("virtio block is read-only or lacks flush"));
        }
        if sector >= self.sectors
        {
            return Err(Error::Malformed("virtio write outside disk"));
        }
        self.request(1, sector, Some(input))
    }

    fn flush(&mut self) -> Result<()>
    {
        if !self.flush_supported
        {
            return Err(Error::Malformed("virtio block lacks FLUSH"));
        }
        self.request(4, 0, None)
    }
}

impl Drop for Disk
{
    fn drop(&mut self)
    {
        // Stop DMA before releasing the queue's backing memory.
        self.write(0x70, 0);
        fence();
        let start = now();
        while self.read(0x70) != 0
        {
            assert!(now().wrapping_sub(start) <= 10_000_000,
                    "virtio reset timed out before DMA memory release");
        }
    }
}

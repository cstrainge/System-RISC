//! QEMU virt M-mode firmware, executed in place from read-only parallel flash.
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
extern crate alloc;
mod virtio_blk;

use alloc::vec::Vec;
use core::{alloc::Layout,
           arch::{asm, global_asm},
           fmt::{self, Write},
           panic::PanicInfo,
           ptr::{addr_of_mut, read_volatile, write_volatile}};

use linked_list_allocator::LockedHeap;
use mac_abi::{BAD_ARGUMENT, BootInfo, Header, NO_MEMORY, NOT_FOUND, NOT_SUPPORTED, OK, Status,
              SystemEntry, SystemServices};
use mac_formats::{elf,
                  native::{Descriptor, Role},
                  resource, text};
use mac_hfs::{BlockDevice, Error, Partition, Result, Volume};

#[global_allocator]
static HEAP: LockedHeap = LockedHeap::empty();

global_asm!(
            r#"
    .section .text.entry
    .global _start
_start:
    csrr t0, mhartid
    bnez t0, parked
    csrw mie, zero
    csrw satp, zero
    la sp, __stack_top
    la t0, trap
    csrw mtvec, t0
    la t0, __data_load
    la t1, __data_start
    la t2, __data_end
1:  bgeu t1, t2, 2f
    ld t3, 0(t0)
    sd t3, 0(t1)
    addi t0, t0, 8
    addi t1, t1, 8
    j 1b
2:  la t0, __bss_start
    la t1, __bss_end
3:  bgeu t0, t1, 4f
    sd zero, 0(t0)
    addi t0, t0, 8
    j 3b
4:  call boot_main
parked:
    wfi
    j parked
    .balign 4
trap:
    csrr a0, mcause
    csrr a1, mepc
    csrr a2, mtval
    call fatal_trap
    j parked
"#
);

struct Serial;
impl Write for Serial
{
    fn write_str(&mut self, text: &str) -> fmt::Result
    {
        unsafe {
            log(text.as_ptr(), text.len());
        }
        Ok(())
    }
}
macro_rules! serial { ($($arg:tt)*) => { let _ = writeln!(Serial, $($arg)*); } }

unsafe extern "C" fn log(bytes: *const u8, length: usize) -> Status
{
    for index in 0..length
    {
        unsafe {
            while read_volatile(0x10000005 as *const u8) & 0x20 == 0
            {
                core::hint::spin_loop();
            }
            write_volatile(0x10000000 as *mut u8, *bytes.add(index));
        }
    }
    OK
}

struct Startup
{
    descriptor: Descriptor,
    code:       Vec<u8>,
    image:      elf::Image
}

fn startup(disk: &mut impl BlockDevice, partition: Partition) -> Result<Startup>
{
    let mut volume = Volume::mount(disk, partition)?;
    serial!("HFS: volume '{}' at sector {}", text::decode(&volume.name), partition.start);
    let folder = volume.blessed_folder()?;
    serial!("HFS: blessed folder '{}' CNID={}", text::decode(&folder.name), folder.id);
    let file = volume.find_file(folder.id, b"System")?;
    if file.file_type != *b"zsys"
    {
        return Err(Error("startup file is not type zsys"));
    }
    let fork = volume.read_resources(&file, 4 * 1024 * 1024)?;
    let resources = resource::decode_borrowed(&fork)?;
    let abi = resources.iter()
                       .find(|r| r.kind == *b"NABI" && r.id == 0)
                       .ok_or(Error("System has no native ABI resource"))?;
    let descriptor = Descriptor::decode(abi.data)?;
    if descriptor.role != Role::System || descriptor.abi_version != mac_abi::ABI_VERSION
    {
        return Err(Error("startup executable is not a compatible System"));
    }
    // Fixed QEMU virt/128 MiB board contract. Keep low 2 MiB for ROM state,
    // stack; firmware scratch occupies 0x87000000..0x87800000 and QEMU's
    // device tree occupies the high 2 MiB. Resource forks now include fonts.
    if descriptor.load_base < 0x80200000 || descriptor.load_limit > 0x87000000
    {
        return Err(Error("System reservation overlaps firmware or exceeds usable RAM"));
    }
    let code = resources.into_iter()
                        .find(|r| r.kind == *b"RV64" && r.id == 0)
                        .ok_or(Error("System has no RV64 executable resource"))?
                        .data
                        .to_vec();
    let image = elf::decode(&code, descriptor.load_base, descriptor.load_limit)?;
    Ok(Startup { descriptor,
                 code,
                 image })
}

struct BootVolume
{
    disks:        *mut virtio_blk::Disk,
    disk_count:   usize,
    startup_disk: usize,
    partition:    Partition
}

unsafe extern "C" fn disk_info(context: *mut (), index: u32, output: *mut mac_abi::DiskInfo)
                               -> Status
{
    if context.is_null()
       || output.is_null()
       || !(output as usize).is_multiple_of(core::mem::align_of::<mac_abi::DiskInfo>())
    {
        return BAD_ARGUMENT;
    }
    let context = unsafe { &*context.cast::<BootVolume>() };
    if index as usize >= context.disk_count
    {
        return NOT_FOUND;
    }
    let disk = unsafe { &*context.disks.add(index as usize) };
    unsafe {
        *output = mac_abi::DiskInfo { sector_count: disk.sector_count(),
                                      read_only:    u32::from(disk.is_read_only()),
                                      icon_kind:    *b"ICN#",
                                      icon_id:      24000,
                                      reserved:     0 };
    }
    OK
}

unsafe extern "C" fn read_sector(context: *mut (), index: u32, sector: u64, output: *mut u8)
                                 -> Status
{
    if context.is_null() || output.is_null()
    {
        return BAD_ARGUMENT;
    }
    let context = unsafe { &mut *context.cast::<BootVolume>() };
    if index as usize >= context.disk_count
    {
        return NOT_FOUND;
    }
    let disk = unsafe { &mut *context.disks.add(index as usize) };
    if sector >= disk.sector_count()
    {
        return BAD_ARGUMENT;
    }
    let output = unsafe { &mut *output.cast::<[u8; 512]>() };
    match disk.read_sector(sector, output)
    {
        Ok(()) => OK,
        Err(error) =>
        {
            serial!("DISK_READ_FAILED: disk={} sector={} {}", index, sector, error);
            NOT_SUPPORTED
        }
    }
}

unsafe extern "C" fn read_startup(context: *mut (), name: *const u8, name_length: usize,
                                  output: *mut u8, capacity: usize, length: *mut usize)
                                  -> Status
{
    if context.is_null()
       || name.is_null()
       || length.is_null()
       || name_length == 0
       || name_length > 31
    {
        return BAD_ARGUMENT;
    }
    let context = unsafe { &mut *context.cast::<BootVolume>() };
    let name = unsafe { core::slice::from_raw_parts(name, name_length) };
    let result = (|| -> Result<()> {
        let disk = unsafe { &mut *context.disks.add(context.startup_disk) };
        let mut volume = Volume::mount(disk, context.partition)?;
        let folder = volume.blessed_folder()?;
        let file = volume.find_file(folder.id, name)?;
        unsafe {
            *length = file.resources.length as usize;
        }
        if output.is_null()
        {
            return Ok(());
        }
        if capacity < file.resources.length as usize
        {
            return Err(Error("startup output buffer is too small"));
        }
        let bytes = volume.read_resources(&file, 4 * 1024 * 1024)?;
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), output, bytes.len());
        }
        Ok(())
    })();
    match result
    {
        Ok(()) => OK,
        Err(error) =>
        {
            serial!("STARTUP_READ_FAILED: {}", error);
            if error.0 == "startup output buffer is too small" { NO_MEMORY } else { NOT_FOUND }
        }
    }
}

fn enter(startup: Startup, disks: &mut [virtio_blk::Disk], disk_index: usize, partition: Partition)
         -> Result<()>
{
    for segment in &startup.image.segments
    {
        serial!("LOAD: 0x{:x} file={} memory={} flags={}", segment.address, segment.file_size,
                segment.memory_size, segment.flags);
        unsafe {
            core::ptr::copy_nonoverlapping(startup.code.as_ptr().add(segment.offset as usize),
                                           segment.address as *mut u8,
                                           segment.file_size as usize);
            core::ptr::write_bytes((segment.address + segment.file_size) as *mut u8,
                                   0,
                                   (segment.memory_size - segment.file_size) as usize);
        }
    }
    unsafe {
        asm!("fence.i", options(nostack));
    }
    serial!("BOOT: entering System at 0x{:x}", startup.image.entry);
    let entry: SystemEntry = unsafe { core::mem::transmute(startup.image.entry as usize) };
    let mut context = BootVolume { disks: disks.as_mut_ptr(),
                                   disk_count: disks.len(),
                                   startup_disk: disk_index,
                                   partition };
    let context_pointer = (&mut context as *mut BootVolume).cast();
    let info = BootInfo { header:                  Header::for_type::<BootInfo>(),
                          ram_base:                0x80000000,
                          ram_size:                128 * 1024 * 1024,
                          log:                     Some(log),
                          startup_context:         context_pointer,
                          read_startup:            Some(read_startup),
                          storage_context:         context_pointer,
                          disk_info:               Some(disk_info),
                          read_sector:             Some(read_sector),
                          startup_disk:            disk_index as u32,
                          reserved:                0,
                          startup_partition_start: partition.start };
    let table = unsafe { entry(&info) };
    let address = table as usize as u64;
    if address < startup.descriptor.load_base
       || address.checked_add(core::mem::size_of::<SystemServices>() as u64).is_none_or(|end| {
                                                                                end
                                                                                > startup.descriptor
                                                                                         .load_limit
                                                                            })
       || !address.is_multiple_of(core::mem::align_of::<SystemServices>() as u64)
    {
        return Err(Error("System returned an invalid service table address"));
    }
    if !unsafe { &*table.cast::<Header>() }.supports::<SystemServices>()
    {
        return Err(Error("System returned an incompatible service table"));
    }
    serial!("BOOT_SYSTEM_READY: services=0x{:x}", address);
    Ok(())
}

#[unsafe(no_mangle)]
extern "C" fn boot_main() -> !
{
    unsafe extern "C" {
        static mut __heap_start: u8;
        static mut __heap_end: u8;
    }
    unsafe {
        HEAP.lock().init(addr_of_mut!(__heap_start),
                         addr_of_mut!(__heap_end) as usize - addr_of_mut!(__heap_start) as usize);
    }
    serial!("RISCy System Boot ROM v1 at 0x{:x}", boot_main as *const () as usize);
    serial!("Searching virtio disks for a blessed HFS System Folder...");
    let mut disks = Vec::new();
    for slot in 1..=8
    {
        let base = 0x10000000 + slot * 0x1000;
        if !virtio_blk::is_block(base)
        {
            continue;
        }
        let disk = match virtio_blk::Disk::open(base)
        {
            Ok(disk) => disk,
            Err(error) =>
            {
                serial!("DISK 0x{:x}: {}", base, error);
                continue;
            }
        };
        serial!("DISK 0x{:x}: {} sectors", base, disk.sector_count());
        disks.push(disk);
    }
    for disk_index in 0..disks.len()
    {
        let partitions = match mac_hfs::partitions(&mut disks[disk_index])
        {
            Ok(partitions) => partitions,
            Err(error) =>
            {
                serial!("DISK {}: {}", disk_index, error);
                continue;
            }
        };
        for partition in partitions
        {
            let result = startup(&mut disks[disk_index], partition);
            match result.and_then(|startup| enter(startup, &mut disks, disk_index, partition))
            {
                Ok(()) =>
                {
                    drop(disks);
                    halt();
                }
                Err(error) =>
                {
                    serial!("BOOT REJECTED: {}", error);
                }
            }
        }
    }
    serial!("BOOT_NO_SYSTEM: searched {} disk(s); no bootable native System found", disks.len());
    halt()
}

fn halt() -> !
{
    loop
    {
        unsafe {
            asm!("wfi", options(nostack));
        }
    }
}

#[unsafe(no_mangle)]
extern "C" fn fatal_trap(cause: usize, pc: usize, value: usize) -> !
{
    serial!("BOOT_TRAP: cause=0x{:x} pc=0x{:x} value=0x{:x}", cause, pc, value);
    halt()
}

#[alloc_error_handler]
fn allocation_failed(layout: Layout) -> !
{
    serial!("BOOT_ALLOCATION_FAILED: {} bytes", layout.size());
    halt()
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> !
{
    serial!("BOOT_PANIC: {}", info);
    halt()
}

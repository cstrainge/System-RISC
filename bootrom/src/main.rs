//! QEMU virt M-mode firmware, executed in place from read-only parallel flash.
#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
extern crate alloc;
mod virtio_blk;
mod screen;
use alloc::vec::Vec;
use core::{alloc::Layout,
           arch::{asm, global_asm},
           fmt::{self, Write},
           panic::PanicInfo,
           ptr::{addr_of_mut, read_volatile, write_volatile}};

use bootrom::memory;
use linked_list_allocator::LockedHeap;
use mac_abi::{BAD_ARGUMENT, BootInfo, Header, NOT_FOUND, NOT_SUPPORTED, OK, Status, SystemEntry,
              SystemServices};
use mac_filesystem::{BlockDevice, Error, Partition, Result, Volume, WritableBlockDevice};
use mac_formats::{native::{Descriptor, Role, image},
                  resource};

#[global_allocator]
static HEAP: LockedHeap = LockedHeap::empty();

global_asm!(concat!(
    include_str!("../../platforms/riscv64-entry.S"),
    r#"
    .section .text.entry
    .option push
    .option norvc
    .global _start
_start:
    csrr t0, mhartid
    bnez t0, parked
    mv s0, a1
    csrw mie, zero
    csrw satp, zero
    la sp, __stack_top
    la t0, trap
    csrw mtvec, t0
    call __system_risc_enable_isa
    .option pop
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
4:  mv a0, s0
    call boot_main
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
));

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
    image:      image::Image
}

fn startup(disk: &mut impl BlockDevice, partition: Partition) -> Result<Startup>
{
    let mut volume = Volume::mount(disk, partition)?;
    if volume.format != mac_filesystem::Format::HfsB
    {
        return Err(Error::Malformed("native boot requires HFS-B; recreate System disk"));
    }
    serial!("HFS-B: volume '{}' at sector {}",
            core::str::from_utf8(&volume.name).unwrap_or("?"),
            partition.start);
    let folder = volume.blessed_folder()?;
    serial!("HFS-B: blessed folder '{}' CNID={}",
            core::str::from_utf8(&folder.name).unwrap_or("?"),
            folder.id);
    let file = volume.find_file(folder.id, b"System")?;
    if file.file_type != *b"zsys"
    {
        return Err(Error::Malformed("startup file is not type zsys"));
    }
    let mut fork = file.resources;
    let resources =
        resource::stream::read_selected(fork.length,
                                        Default::default(),
                                        |kind, id| {
                                            id == 0 && (kind == *b"NABI" || kind == *b"RV64")
                                        },
                                        |offset, output| {
                                            let n = volume.read(&mut fork, offset, output)?;
                                            if n == output.len()
                                            {
                                                Ok(())
                                            }
                                            else
                                            {
                                                Err(Error::Malformed("short startup resource read"))
                                            }
                                        })?;
    let abi = resources.iter()
                       .find(|r| r.kind == *b"NABI" && r.id == 0)
                       .ok_or(Error::Malformed("System has no native ABI resource"))?;
    let descriptor = Descriptor::decode(&abi.data)?;
    if descriptor.role != Role::System || descriptor.abi_version != mac_abi::ABI_VERSION
    {
        return Err(Error::Malformed("startup executable is not a compatible System"));
    }
    // Placement policy is generated from the checked platform record.
    if descriptor.load_base < memory::IMAGE_BASE
       || descriptor.load_limit > memory::APPLICATION_PRIMARY_BASE
    {
        return Err(Error::Malformed("System reservation overlaps firmware or exceeds usable RAM"));
    }
    let code = resources.into_iter()
                        .find(|r| r.kind == *b"RV64" && r.id == 0)
                        .ok_or(Error::Malformed("System has no RV64 executable resource"))?
                        .data;
    let image = image::decode(&code, &descriptor)?;
    // A bootable installation includes a compatible Finder. Validate its code
    // and relocation plan before displaying Happy Mac or entering System.
    let finder = volume.find_file(folder.id, b"Finder")?;
    if finder.file_type != *b"FNDR"
    {
        return Err(Error::Malformed("startup Finder does not have type FNDR"));
    }
    let mut fork = finder.resources;
    let finder =
        resource::stream::read_selected(fork.length,
                                        Default::default(),
                                        |kind, id| {
                                            id == 0 && matches!(&kind, b"NABI" | b"RV64" | b"RLOC")
                                        },
                                        |offset, output| {
                                            let n = volume.read(&mut fork, offset, output)?;
                                            if n == output.len()
                                            {
                                                Ok(())
                                            }
                                            else
                                            {
                                                Err(Error::Malformed("short Finder resource read"))
                                            }
                                        })?;
    let get = |kind| {
        finder.iter()
              .find(|r| r.kind == kind)
              .ok_or(Error::Malformed("startup Finder code metadata is missing"))
    };
    let abi = Descriptor::decode(&get(*b"NABI")?.data)?;
    if abi.role != Role::Application
       || abi.abi_version != mac_abi::ABI_VERSION
       || !memory::application_region(abi.load_base, abi.load_limit)
    {
        return Err(Error::Malformed("startup Finder is incompatible; rebuild System disk"));
    }
    image::LoadPlan::new(&get(*b"RV64")?.data, &abi, Some(&get(*b"RLOC")?.data), abi.load_base)?;
    Ok(Startup { descriptor,
                 code,
                 image })
}

struct BootVolume
{
    disks:      *mut virtio_blk::Disk,
    disk_count: usize
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
unsafe extern "C" fn write_sector(context: *mut (), index: u32, sector: u64, input: *const u8)
                                  -> Status
{
    if context.is_null() || input.is_null()
    {
        return BAD_ARGUMENT;
    }
    let context = unsafe { &mut *context.cast::<BootVolume>() };
    if index as usize >= context.disk_count
    {
        return NOT_FOUND;
    }
    let disk = unsafe { &mut *context.disks.add(index as usize) };
    if disk.is_read_only()
    {
        return mac_abi::READ_ONLY;
    }
    if sector >= disk.sector_count()
    {
        return BAD_ARGUMENT;
    }
    let input = unsafe { &*input.cast::<[u8; 512]>() };
    disk.write_sector(sector, input).map_or_else(|error| {
                                                     serial!("DISK_WRITE_FAILED: disk={} \
                                                              sector={} {}",
                                                             index,
                                                             sector,
                                                             error);
                                                     mac_abi::IO_ERROR
                                                 },
                                                 |_| OK)
}
unsafe extern "C" fn flush_disk(context: *mut (), index: u32) -> Status
{
    if context.is_null()
    {
        return BAD_ARGUMENT;
    }
    let context = unsafe { &mut *context.cast::<BootVolume>() };
    if index as usize >= context.disk_count
    {
        return NOT_FOUND;
    }
    let disk = unsafe { &mut *context.disks.add(index as usize) };
    if disk.is_read_only()
    {
        return mac_abi::READ_ONLY;
    }
    disk.flush().map_or_else(|error| {
                                 serial!("DISK_FLUSH_FAILED: disk={} {}", index, error);
                                 mac_abi::IO_ERROR
                             },
                             |_| OK)
}



fn enter(startup: Startup, disks: &mut [virtio_blk::Disk], disk_index: usize,
         partition: Partition, memory: &mut memory::Memory)
         -> Result<()>
{
    let plan = image::LoadPlan::new(&startup.code, &startup.descriptor, None,
                                    startup.descriptor.load_base)?;
    for segment in &startup.image.segments
    {
        serial!("LOAD: 0x{:x} bytes={} flags={}",
                startup.descriptor.load_base + segment.offset,
                segment.length,
                segment.flags);
    }
    memory.reserve(startup.descriptor.load_base..startup.descriptor.load_limit)?;
    let output = unsafe {
        core::slice::from_raw_parts_mut(startup.descriptor.load_base as *mut u8, plan.length())
    };
    plan.write(output)?;
    unsafe {
        asm!("fence.i", options(nostack));
    }
    screen::happy();
    serial!("BOOT_VISUAL_HAPPY");
    serial!("BOOT: entering System at 0x{:x}", plan.entry());
    let entry: SystemEntry = unsafe { core::mem::transmute(plan.entry() as usize) };
    let mut context = BootVolume { disks:      disks.as_mut_ptr(),
                                   disk_count: disks.len() };
    let context_pointer = (&mut context as *mut BootVolume).cast();
    mac_abi::boot::Memory { ram: &memory.ram, reservations: &memory.reserved }
        .validate(memory::PAGE_BYTES, memory.ram[0])
        .map_err(|_| Error::Malformed("bootstrap memory account failed validation"))?;
    serial!("BOOT_MEMORY banks={} reservations={} primary_bytes={}",
            memory.ram.len(),
            memory.reserved.len(),
            memory.ram[0].length);
    let info = BootInfo { header:                  Header::for_type::<BootInfo>(),
                          ram_base:                memory.ram[0].base,
                          ram_size:                memory.ram[0].length,
                          log:                     Some(log),
                          storage_context:         context_pointer,
                          disk_info:               Some(disk_info),
                          read_sector:             Some(read_sector),
                          write_sector:            Some(write_sector),
                          flush_disk:              Some(flush_disk),
                          startup_disk:            disk_index as u32,
                          flags:                   mac_abi::BOOT_GRAPHICAL,
                          startup_partition_start: partition.start,
                          ram_regions:             memory.ram.as_ptr(),
                          ram_region_count:        memory.ram.len(),
                          reservations:            memory.reserved.as_ptr(),
                          reservation_count:       memory.reserved.len() };
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
        return Err(Error::Malformed("System returned an invalid service table address"));
    }
    if !unsafe { &*table.cast::<Header>() }.supports::<SystemServices>()
    {
        return Err(Error::Malformed("System returned an incompatible service table"));
    }
    serial!("BOOT_SYSTEM_READY: services=0x{:x}", address);
    Ok(())
}

#[unsafe(no_mangle)]
extern "C" fn boot_main(tree_address: usize) -> !
{
    // SAFETY: QEMU's reset firmware passes the valid DTB address in a1. Only
    // this bootstrap boundary reads a physical pointer; parsing uses checked
    // slices. Limit the full read to the build-defined FDT reservation size.
    let tree = unsafe {
        if tree_address == 0 || tree_address % 8 != 0
        {
            serial!("BOOT_MEMORY_REJECTED: missing or unaligned DTB");
            halt();
        }
        let header = core::slice::from_raw_parts(tree_address as *const u8, 40);
        let size = u32::from_be_bytes(header[4..8].try_into().unwrap()) as usize;
        if !(40..=memory::FDT_TAIL_BYTES as usize).contains(&size)
        {
            serial!("BOOT_MEMORY_REJECTED: DTB exceeds platform reservation");
            halt();
        }
        bootrom::devicetree::Tree::new(core::slice::from_raw_parts(tree_address as *const u8, size))
    };
    let tree = match tree.and_then(|tree| {
                             memory::admit(&tree)?;
                             Ok(tree)
                         })
    {
        Ok(tree) => tree,
        Err(error) =>
        {
            serial!("BOOT_MEMORY_REJECTED: {error}");
            halt();
        }
    };

    unsafe extern "C" {
        static mut __heap_start: u8;
        static mut __heap_end: u8;
    }
    unsafe {
        HEAP.lock().init(addr_of_mut!(__heap_start),
                         addr_of_mut!(__heap_end) as usize - addr_of_mut!(__heap_start) as usize);
    }
    let mut memory = match memory::Memory::discover(&tree, tree_address as u64)
    {
        Ok(memory) => memory,
        Err(error) =>
        {
            serial!("BOOT_MEMORY_REJECTED: {error}");
            halt();
        }
    };
    serial!("BOOT_RAM base={:#x} bytes={} banks={} reservations={}",
            memory.ram[0].base,
            memory.ram[0].length,
            memory.ram.len(),
            memory.reserved.len());
    serial!("System RISC Boot ROM v1 at 0x{:x}", boot_main as *const () as usize);
    let _ = screen::init();
    serial!("Searching virtio disks for a blessed HFS-B System Folder...");
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
        let partitions = match mac_filesystem::partitions(&mut disks[disk_index])
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
            match result.and_then(|startup| {
                            enter(startup, &mut disks, disk_index, partition, &mut memory)
                        })
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
    screen::missing();
    serial!("BOOT_VISUAL_MISSING");
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

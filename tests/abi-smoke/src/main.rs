//! QEMU-only entry/ABI test. This is not the future HFS disk bootstrap.
#![no_std]
#![no_main]
use core::{arch::global_asm,
           panic::PanicInfo,
           ptr::{read_volatile, write_volatile}};

use mac_abi::{ApplicationEntry, BAD_ABI, BAD_ARGUMENT, BootInfo, Header, OK, Status, SystemEntry,
              SystemServices};
use toolbox_quickdraw::{Point, QuickDraw, Rect, SurfacePort,
                        bitmaps::Bitmap,
                        graphics::{PortState, Shape, TransferMode, Verb}};
include!(concat!(env!("OUT_DIR"), "/entries.rs"));

// Canonical acceptance logic lives with the aggregate userspace Toolbox tests.
#[allow(dead_code)]
#[path = "../../../crates/mac-toolbox/tests/conformance/mod.rs"]
mod conformance;

global_asm!(concat!(
    include_str!("../../../platforms/riscv64-entry.S"),
    r#"
    .section .text.entry
    .option push
    .option norvc
    .global _start
_start:
    csrw mie, zero
    la sp, __stack_top
    la t0, trap
    csrw mtvec, t0
    call __system_risc_enable_isa
    .option pop
    la t0, __bss_start
    la t1, __bss_end
1:  bgeu t0, t1, 2f
    sd zero, 0(t0)
    addi t0, t0, 8
    j 1b
2:  call main
3:  wfi
    j 3b
    .balign 4
trap:
    j failed
"#
));

unsafe extern "C" fn log(bytes: *const u8, length: usize) -> Status
{
    for &byte in unsafe { core::slice::from_raw_parts(bytes, length) }
    {
        unsafe {
            while read_volatile(0x10000005 as *const u8) & 0x20 == 0
            {
                core::hint::spin_loop();
            }
            write_volatile(0x10000000 as *mut u8, byte);
        }
    }
    OK
}

#[unsafe(no_mangle)]
extern "C" fn main() -> !
{
    let boot = BootInfo { header:                  Header::for_type::<BootInfo>(),
                          ram_base:                0x80000000,
                          ram_size:                128 * 1024 * 1024,
                          log:                     Some(log),
                          storage_context:         core::ptr::null_mut(),
                          disk_info:               None,
                          read_sector:             None,
                          write_sector:            None,
                          flush_disk:              None,
                          startup_disk:            0,
                          flags:                   0,
                          startup_partition_start: 0 };
    let system_entry: SystemEntry = unsafe { core::mem::transmute(SYSTEM_ENTRY) };
    let finder_entry: ApplicationEntry = unsafe { core::mem::transmute(FINDER_ENTRY) };
    let services = unsafe { system_entry(&boot) };
    if services.is_null() || unsafe { finder_entry(services) } != OK
    {
        failed();
    }
    if unsafe { finder_entry(core::ptr::null()) } != BAD_ARGUMENT
    {
        failed();
    }
    let incompatible = Header { version: mac_abi::ABI_VERSION + 1,
                                size:    core::mem::size_of::<SystemServices>() as u32 };
    if unsafe { finder_entry((&incompatible as *const Header).cast()) } != BAD_ABI
    {
        failed();
    }
    if !unsafe { system_entry((&incompatible as *const Header).cast()) }.is_null()
    {
        failed();
    }
    // Exercise the real resident ECALL handler, not a host mock or direct method.
    let client = unsafe { mac_traps::Client::from_raw(services) }.unwrap_or_else(|_| failed());
    let mut geometry = mac_abi::traps::GeometryCall { a: mac_abi::Rect::new(0, 0, 20, 20),
                                                      b: mac_abi::Rect::new(10, 5, 30, 15),
                                                      ..Default::default() };
    if client.geometry(&mut geometry).is_err()
       || geometry.result != 1
       || geometry.output != mac_abi::Rect::new(10, 5, 20, 15)
    {
        failed();
    }
    if unsafe { ((*services).trap)(0xffffffff, core::ptr::null_mut(), 0) }
       != mac_abi::NOT_SUPPORTED as i64
    {
        failed();
    }
    if unsafe { ((*services).trap)(mac_abi::traps::WINDOW, core::ptr::null_mut(), 0) }
       != BAD_ARGUMENT as i64
    {
        failed();
    }
    let mut memory = [0u8; 64];
    let base = 0x1000u32;
    for (offset, value) in [(0, 4i16), (2, 3), (4, 2), (6, 1)]
    {
        memory[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
    }
    memory[8..12].copy_from_slice(&(base + 32).to_be_bytes());
    let mut registers = mac_abi::traps::ClassicRegisters { pc: 0x4000,
                                                           ..Default::default() };
    registers.a[7] = base;
    let mut classic_call = mac_abi::traps::ClassicCall { instruction:
                                                             mac_abi::traps::classic::SET_RECT,
                                                         reserved: 0,
                                                         guest_base: base,
                                                         memory: memory.as_mut_ptr(),
                                                         memory_length: memory.len(),
                                                         registers };
    let classic_status = unsafe {
        ((*services).trap)(mac_abi::traps::CLASSIC,
                           core::ptr::from_mut(&mut classic_call).cast(),
                           core::mem::size_of_val(&classic_call))
    };
    registers = classic_call.registers;
    if classic_status != OK as i64
       || memory[32..40] != mac_abi::Rect::new(1, 2, 3, 4).to_be_bytes()
       || registers.a[7] != base + 12
       || registers.pc != 0x4002
    {
        failed();
    }
    quickdraw_checks(client, unsafe { (*services).trap });
    let userspace = unsafe { mac_toolbox::System::from_raw(services) }.unwrap_or_else(|_| failed());
    conformance::run_core(&userspace).unwrap_or_else(|_| failed());
    if unsafe { ((*services).trap)(mac_abi::traps::MEMORY, core::ptr::null_mut(), 0) }
       != BAD_ARGUMENT as i64
    {
        failed();
    }
    let compatibility = b"SCRAP_USERSPACE_PASS original=18 native=17\n\
                          TOOLBOX_USERSPACE_PASS geometry=20 ovals=267 handles=48\n";
    unsafe {
        log(compatibility.as_ptr(), compatibility.len());
    }
    let mut files = mac_abi::files::FileCall { operation: mac_abi::files::VOLUME_COUNT,
                                               ..Default::default() };
    if client.file(&mut files, "", &mut []).is_err() || files.index != 0
    {
        failed();
    }
    files.operation = mac_abi::files::VOLUME_INDEX;
    if client.file(&mut files, "", &mut []) != Err(mac_traps::Error::NotFound)
    {
        failed();
    }
    if unsafe { ((*services).trap)(mac_abi::traps::FILE, core::ptr::null_mut(), 0) }
       != BAD_ARGUMENT as i64
    {
        failed();
    }
    unsafe {
        let native = b"NATIVE_TRAP_ABI_PASS\n";
        log(native.as_ptr(), native.len());
        let classic = b"CLASSIC_TRAP_ABI_PASS\n";
        log(classic.as_ptr(), classic.len());
        let quickdraw = b"QUICKDRAW_TRAP_ABI_PASS\n";
        log(quickdraw.as_ptr(), quickdraw.len());
        let files = b"FILE_TRAP_ABI_PASS\n";
        log(files.as_ptr(), files.len());
    }
    let message = b"ABI_SMOKE_PASS\n";
    unsafe {
        log(message.as_ptr(), message.len());
        write_volatile(0x100000 as *mut u32, 0x5555);
    }
    loop
    {
        core::hint::spin_loop();
    }
}

// Safe SDK calls cross the actual independently linked System ECALL boundary.
// Reading caller-owned pixels verifies the result without a graphics device.
fn quickdraw_checks(client: mac_traps::Client<'_>, gate: mac_abi::TrapGate)
{
    let qd = QuickDraw::new(client);
    let mut outer = [0; 64];
    let mut inner = [0; 64];
    let mut output = [0; 256];
    let outer =
        qd.regions().rectangle(Rect::new(0, 0, 20, 20), &mut outer).unwrap_or_else(|_| failed());
    let inner =
        qd.regions().rectangle(Rect::new(5, 5, 15, 15), &mut inner).unwrap_or_else(|_| failed());
    let ring = qd.regions().difference(outer, inner, &mut output).unwrap_or_else(|_| failed());
    if ring.contains(Point { h: 2,
                             v: 2 })
       != Ok(true)
       || ring.contains(Point { h: 10,
                                v: 10 })
          != Ok(false)
       || qd.map_point(Point { h: 5,
                               v: 5 },
                       Rect::new(0, 0, 10, 10),
                       Rect::new(10, 20, 30, 60))
          != Ok(Point { h: 20,
                        v: 40 })
    {
        failed();
    }
    let mut pixels = [0xabcdefu32; 16 * 8];
    let mut surface = mac_abi::Surface { pixels: pixels.as_mut_ptr(),
                                         width:  16,
                                         height: 8,
                                         origin: Point { h: 1,
                                                         v: 1 },
                                         clip:   Rect::new(0, 0, 16, 8) };
    let mut port =
        SurfacePort::new(unsafe { mac_traps::SurfaceConnection::new(&mut surface, gate) });
    let mut state = PortState { foreground: 0x173d85,
                                clip: Rect::new(1, 1, 6, 5),
                                ..Default::default() };
    port.draw_shape(&mut state, Shape::Rect(Rect::new(-5, -5, 20, 20)), Verb::Paint)
        .unwrap_or_else(|_| failed());
    for y in 0..8
    {
        for x in 0..16
        {
            let expected =
                if (2..7).contains(&x) && (2..6).contains(&y) { 0x173d85 } else { 0xabcdef };
            if pixels[y * 16 + x] != expected
            {
                failed();
            }
        }
    }
    let original = pixels;
    for _ in 0..2
    {
        port.draw_shape(&mut state, Shape::Rect(Rect::new(1, 1, 6, 5)), Verb::Invert)
            .unwrap_or_else(|_| failed());
    }
    if pixels != original
    {
        failed();
    }
    let bitmap = Bitmap::Mono { data:      &[0x80],
                                row_bytes: 1,
                                bounds:    Rect::new(0, 0, 2, 1) };
    port.copy_bits(&bitmap, bitmap.bounds(), Rect::new(0, 0, 8, 4), TransferMode::Copy, &[])
        .unwrap_or_else(|_| failed());
    for y in 1..5
    {
        for x in 1..9
        {
            if pixels[y * 16 + x] != if x < 5 { 0 } else { 0xffffff }
            {
                failed();
            }
        }
    }
    if unsafe { gate(mac_abi::traps::QUICKDRAW, core::ptr::null_mut(), 0) } != BAD_ARGUMENT as i64
    {
        failed();
    }
}

#[unsafe(no_mangle)]
extern "C" fn failed() -> !
{
    let message = b"ABI_SMOKE_FAIL\n";
    unsafe {
        log(message.as_ptr(), message.len());
        write_volatile(0x100000 as *mut u32, 0x3333);
    }
    loop
    {
        core::hint::spin_loop();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> !
{
    failed()
}

//! Private acceptance-only clock/log/heap boundaries. No application identity,
//! thread allocation, resident worker activation or host clipboard access.
#![allow(unsafe_code)]
use core::{fmt::{Arguments, Write},
           sync::atomic::{AtomicBool, Ordering}};

use linked_list_allocator::LockedHeap;
const HEAP_BYTES: usize = 16384;
#[repr(align(4096))]
struct Storage([u8; HEAP_BYTES]);
static mut STORAGE: Storage = Storage([0; HEAP_BYTES]);
#[global_allocator]
static HEAP: LockedHeap = LockedHeap::empty();
static INITIALIZED: AtomicBool = AtomicBool::new(false);
pub fn initialize() -> bool
{
    if INITIALIZED.swap(true, Ordering::AcqRel)
    {
        return false;
    }
    // SAFETY: the admitted module owns this private pinned BSS reservation.
    // Bootstrap initializes the allocator once, before any codec allocation.
    unsafe {
        HEAP.lock().init(core::ptr::addr_of_mut!(STORAGE.0).cast::<u8>(), HEAP_BYTES);
    }
    true
}
pub fn now() -> u64
{
    let ticks: u64;
    // SAFETY: virt machine's side-effect-free 10 MHz monotonic counter.
    unsafe {
        core::arch::asm!("rdtime {ticks}",ticks=out(reg)ticks,options(nomem,nostack));
    }
    ticks / 10
}
pub fn pause()
{
    let start = now();
    while now().saturating_sub(start) < 1000
    {
        core::hint::spin_loop();
    }
}
pub fn log(arguments: Arguments<'_>)
{
    struct Text
    {
        bytes:  [u8; 512],
        length: usize
    }
    impl Write for Text
    {
        fn write_str(&mut self, value: &str) -> core::fmt::Result
        {
            let mut count = value.len().min(self.bytes.len() - self.length);
            while !value.is_char_boundary(count)
            {
                count -= 1;
            }
            self.bytes[self.length..self.length + count].copy_from_slice(&value.as_bytes()
                                                                             [..count]);
            self.length += count;
            Ok(())
        }
    }
    let mut text = Text { bytes:  [0; 512],
                          length: 0 };
    let _ = text.write_fmt(arguments);
    let mut call = mac_abi::traps::LogCall { bytes:  text.bytes.as_ptr(),
                                             length: text.length };
    let mut result = core::ptr::from_mut(&mut call).addr();
    // SAFETY: synchronous LOG trap borrows initialized exclusive call and text
    // storage for this invocation. This does not invent an application owner.
    unsafe {
        core::arch::asm!("ecall",in("a7")mac_abi::traps::LOG,inlateout("a0")result,
        in("a1")core::mem::size_of_val(&call),clobber_abi("C"));
    }
    let _ = result;
}

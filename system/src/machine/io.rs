//! Synchronous handoff to the bootstrap I/O thread's dedicated stack.
//! Firmware contexts never become Send. Kernel traps mask interrupts during this
//! bounded handoff; callbacks execute exclusively on their owning OS thread1.
use core::{arch::global_asm, mem::MaybeUninit};
#[repr(C, align(16))]
struct Stack([u8; 256 * 1024]);
static mut STACK: Stack = Stack([0; 256 * 1024]);
unsafe extern "C" {
    fn __system_io_switch(context: *mut (), entry: unsafe extern "C" fn(*mut ()), stack: *mut u8);
}
global_asm!(
            r#"
.global __system_io_switch
__system_io_switch:
addi sp,sp,-112
sd ra,0(sp)
sd s0,8(sp)
sd s1,16(sp)
sd s2,24(sp)
sd s3,32(sp)
sd s4,40(sp)
sd s5,48(sp)
sd s6,56(sp)
sd s7,64(sp)
sd s8,72(sp)
sd s9,80(sp)
sd s10,88(sp)
sd s11,96(sp)
mv s0,sp
mv sp,a2
jalr a1
mv sp,s0
ld ra,0(sp)
ld s0,8(sp)
ld s1,16(sp)
ld s2,24(sp)
ld s3,32(sp)
ld s4,40(sp)
ld s5,48(sp)
ld s6,56(sp)
ld s7,64(sp)
ld s8,72(sp)
ld s9,80(sp)
ld s10,88(sp)
ld s11,96(sp)
addi sp,sp,112
ret
"#
);
pub fn run<F: FnOnce() -> R, R>(function: F) -> R
{
    super::interrupts::with_masked(|| run_masked(function))
}
fn run_masked<F: FnOnce() -> R, R>(function: F) -> R
{
    if super::scheduler::current_context().identity().thread == crate::context::BOOT_THREAD
    {
        return function();
    }
    struct Call<F, R>
    {
        function: Option<F>,
        result:   MaybeUninit<R>
    }
    unsafe extern "C" fn invoke<F: FnOnce() -> R, R>(pointer: *mut ())
    {
        let call = unsafe { &mut *pointer.cast::<Call<F, R>>() };
        let previous = super::scheduler::io_enter();
        let result = call.function.take().unwrap()();
        super::scheduler::io_leave(previous);
        call.result.write(result);
    }
    let mut call = Call { function: Some(function),
                          result:   MaybeUninit::uninit() };
    // Exclusive entry is guaranteed by the interrupt-masked native trap boundary.
    unsafe {
        let top = core::ptr::addr_of_mut!(STACK).cast::<u8>().add(256 * 1024);
        __system_io_switch((&mut call as *mut Call<F, R>).cast(), invoke::<F, R>, top);
        call.result.assume_init()
    }
}

//! Single-hart native entry ownership. Every raw control access stays here or
//! in traps.S; ordinary resident callers receive scoped stack/CPU services.
use core::{arch::{asm, global_asm},
           fmt,
           mem::MaybeUninit,
           ptr};

use super::{SavedState, TrapFrame};
use crate::machine::{memory,
                     pages::{Backing, Stack}};

#[derive(Clone, Copy)]
#[repr(C, align(16))]
pub(super) struct Control
{
    pub entering:       usize,
    pub depth:          usize,
    pub base_depth:     usize,
    pub frames:         usize,
    pub stride:         usize,
    pub floor:          usize,
    pub top:            usize,
    pub temporary0:     usize,
    pub temporary1:     usize,
    pub temporary2:     usize,
    pub interrupted_sp: usize,
    pub defer_allowed:  usize,
    #[cfg(feature = "event-trace")]
    observed:           usize
}
pub(super) static HART: spin::Once<Hart> = spin::Once::new();
pub(super) struct Hart
{
    _stack:     &'static Stack,
    _control:   Backing,
    _bootstrap: SavedState,
    control:    usize
}
#[derive(Debug)]
pub(crate) enum Error
{
    AlreadyInitialized,
    InterruptsEnabled
    {
        status: usize
    },
    Pages
    {
        resource: &'static str,
        cause:    crate::machine::pages::Error
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::AlreadyInitialized =>
            {
                f.write_str("System hart entry is already initialized; reset before retrying")
            }
            Self::InterruptsEnabled { status } =>
            {
                write!(f,
                       "System stack bootstrap requires local interrupts masked; \
                        sstatus={status:#x}; fix firmware admission and rebuild")
            }
            Self::Pages { resource,
                          cause } => write!(f, "System {resource} reservation refused: {cause}")
        }
    }
}
/// Single-use cold-bootstrap authority. It cannot migrate to another hart or
/// be reconstructed by a caller after resident execution has started.
#[must_use]
pub(crate) struct PreparedEntry
{
    hart:   &'static Hart,
    _local: core::marker::PhantomData<*mut ()>
}
pub(crate) fn prepare(stack: &'static Stack) -> Result<PreparedEntry, Error>
{
    let status: usize;
    unsafe {
        asm!("csrr {value},sstatus",value=out(reg)status,options(nomem,nostack));
    }
    if status & 2 != 0
    {
        return Err(Error::InterruptsEnabled { status });
    }
    if HART.get().is_some()
    {
        return Err(Error::AlreadyInitialized);
    }
    let control = Backing::allocate(size_of::<Control>()).map_err(|cause| {
                                                             Error::Pages { resource:
                                                                                "entry control",
                                                                            cause }
                                                         })?;
    let bootstrap =
        SavedState::new(TrapFrame::default()).map_err(|cause| {
                                                 Error::Pages { resource: "bootstrap CPU state",
                                                                cause }
                                             })?;
    let pointer = control.cpu_base();
    let state = Control { entering:                                 0,
                          depth:                                    0,
                          base_depth:                               0,
                          frames:                                   bootstrap.frame() as usize,
                          stride:                                   bootstrap.stride(),
                          floor:                                    stack.top()
                                                                    - stack.bytes() as usize,
                          top:                                      stack.top(),
                          temporary0:                               0,
                          temporary1:                               0,
                          temporary2:                               0,
                          interrupted_sp:                           0,
                          defer_allowed:                            0,
                          #[cfg(feature = "event-trace")]
                          observed:                                 0 };
    // SAFETY: exclusive zeroed backing, aligned for Control; only this native
    // binding and the serialized assembly boundary access its scalar fields.
    unsafe {
        (pointer as *mut Control).write(state);
    }
    let hart = HART.call_once(|| {
                       Hart { _stack:     stack,
                              _control:   control,
                              _bootstrap: bootstrap,
                              control:    pointer }
                   });
    // SAFETY: bootstrap owns this hart with interrupts masked. All backing is
    // permanent before the control pointer can become an entry authority.
    unsafe {
        asm!("csrw sscratch,{control}",control=in(reg)hart.control,options(nostack));
    }
    serial!("SYSTEM_STACK_READY base={:#x} bytes={} control={:#x} cpu_slots={} cpu_stride={}",
            state.floor,
            memory::SYSTEM_STACK_BYTES,
            pointer,
            super::CPU_SLOTS,
            state.stride);
    Ok(PreparedEntry { hart,
                       _local: core::marker::PhantomData })
}
/// The scheduler retains the selected core until the CPU and its readers leave.
/// An unchanged frame and bootstrap baseline perform no control-memory write.
pub(crate) fn publish(state: &SavedState)
{
    let Some(hart) = HART.get()
    else
    {
        super::entry_lost("hart entry was not prepared");
    };
    let field = unsafe { ptr::addr_of_mut!((*(hart.control as *mut Control)).frames) };
    // SAFETY: native scheduler owns this masked hart; no entry or other CPU
    // reads/writes its per-hart control concurrently. The selected core pins it.
    unsafe {
        let baseline = ptr::addr_of_mut!((*(hart.control as *mut Control)).base_depth);
        if baseline.read_volatile() != 0
        {
            baseline.write_volatile(0);
        }
        if field.read_volatile() != state.frame() as usize
        {
            field.write_volatile(state.frame() as usize);
        }
    }
}
/// Bootstrap resident work uses the same System stack as later trap services.
/// Base depth1 preserves its live Rust frames across definition ECALLs.
impl PreparedEntry
{
    pub(crate) fn run<F: FnOnce() -> R, R>(self, function: F) -> R
    {
        let hart = self.hart;
        struct Call<F, R>
        {
            function: MaybeUninit<F>,
            result:   MaybeUninit<R>
        }
        unsafe extern "C" fn invoke<F: FnOnce() -> R, R>(pointer: *mut ())
        {
            // SAFETY: the private switch invokes this entry exactly once while Call
            // and its captured borrows remain pinned on the bootstrap stack.
            let call = unsafe { &mut *pointer.cast::<Call<F, R>>() };
            let function = unsafe { call.function.assume_init_read() };
            call.result.write(function());
        }
        let mut call = Call { function: MaybeUninit::new(function),
                              result:   MaybeUninit::uninit() };
        unsafe {
            let control = hart.control as *mut Control;
            ptr::addr_of_mut!((*control).base_depth).write_volatile(1);
            ptr::addr_of_mut!((*control).depth).write_volatile(1);
            __system_boot_stack((&mut call as *mut Call<F, R>).cast(),
                                invoke::<F, R>,
                                hart._stack.top());
            ptr::addr_of_mut!((*control).depth).write_volatile(0);
            ptr::addr_of_mut!((*control).base_depth).write_volatile(0);
            call.result.assume_init()
        }
    }
}
unsafe extern "C" {
    fn __system_boot_stack(context: *mut (), entry: unsafe extern "C" fn(*mut ()), top: usize);
}
// The cold bootstrap call frame owns no lock/RAII resource needed by running
// applications. Graphical startup never returns; headless startup restores it.
global_asm!(
            r#"
.global __system_boot_stack
__system_boot_stack:
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

pub(crate) fn can_defer_io() -> bool
{
    let Some(hart) = HART.get()
    else
    {
        return false;
    };
    // SAFETY: caller is the interrupt-masked resident service on this hart.
    // Separate scalar metadata avoids aliasing the dispatcher's live &mut frame.
    unsafe {
        let control = hart.control as *const Control;
        ptr::addr_of!((*control).depth).read_volatile() == 1
        && ptr::addr_of!((*control).defer_allowed).read_volatile() != 0
    }
}

pub(crate) fn service_scope<R>(allowed: bool, call: impl FnOnce() -> R) -> R
{
    let Some(hart) = HART.get()
    else
    {
        super::entry_lost("service entry has no hart control");
    };
    struct Restore
    {
        field:    *mut usize,
        previous: usize
    }
    impl Drop for Restore
    {
        fn drop(&mut self)
        {
            unsafe {
                self.field.write_volatile(self.previous);
            }
        }
    }
    // SAFETY: only the masked native service owns this hart's control. Nested
    // dispatch restores permission; timer maintenance is never a caller frame.
    let field = unsafe { ptr::addr_of_mut!((*(hart.control as *mut Control)).defer_allowed) };
    let previous = unsafe { field.read_volatile() };
    unsafe {
        field.write_volatile(usize::from(allowed));
    }
    let _restore = Restore { field,
                             previous };
    call()
}

pub(crate) fn service_depth(physical: usize) -> usize
{
    let Some(hart) = HART.get()
    else
    {
        super::entry_lost("service depth has no hart control");
    };
    let baseline =
        unsafe { ptr::addr_of!((*(hart.control as *const Control)).base_depth).read_volatile() };
    match physical.checked_sub(baseline)
    {
        Some(depth) if depth != 0 => depth,
        _ => super::entry_lost("service depth is below its bootstrap baseline")
    }
}

/// Two bounded observations in traced private boots, not a per-event serial
/// stream. Real Rust SP plus the actual entry frame qualify the assembly seam.
#[cfg(feature = "event-trace")]
pub(crate) fn observe_entry(frame: usize)
{
    let Some(hart) = HART.get()
    else
    {
        super::entry_lost("trace entry has no hart control");
    };
    unsafe {
        let control = hart.control as *mut Control;
        let bootstrap = ptr::addr_of!((*control).base_depth).read_volatile() != 0;
        let bit = if bootstrap { 1 } else { 2 };
        let flags = ptr::addr_of_mut!((*control).observed);
        let previous = flags.read_volatile();
        if previous & bit != 0
        {
            return;
        }
        let floor = ptr::addr_of!((*control).floor).read_volatile();
        let top = ptr::addr_of!((*control).top).read_volatile();
        let sp: usize;
        asm!("mv {value},sp",value=out(reg)sp,options(nomem,nostack));
        if sp <= floor || sp > top || (floor..top).contains(&frame)
        {
            super::entry_lost("native Rust entry or owned frame is on the wrong stack");
        }
        flags.write_volatile(previous | bit);
        serial!("SYSTEM_ENTRY_STACK mode={} sp={sp:#x} frame={frame:#x} base={floor:#x} \
                 top={top:#x} physical={:#x}",
                if bootstrap { "bootstrap" } else { "runtime" },
                hart._stack.physical().0);
    }
}

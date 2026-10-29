//! Reset-lifetime native entry registry. Only the primary is Online; discovered
//! peers own prepared backing but gain no S execution/context authority.
//! Raw Control storage is authenticated before dereference and never borrowed
//! as &Control or a slice after assembly publication.
use core::{arch::{asm, global_asm},
           fmt,
           mem::MaybeUninit,
           ptr,
           sync::atomic::{AtomicBool, Ordering}};

pub(crate) mod execution;
use execution::HartLocal;

use super::{SavedState, TrapFrame};
use crate::{allocator::foundation::{self, Foundation},
            machine::pages::{ArrayError, Backing, OwnedArray, Stack}};

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
    local:              usize,
    #[cfg(feature = "event-trace")]
    observed:           usize
}

impl Control
{
    const fn poison() -> Self
    {
        Self { entering:                                 1,
               depth:                                    0,
               base_depth:                               0,
               frames:                                   0,
               stride:                                   0,
               floor:                                    0,
               top:                                      0,
               temporary0:                               0,
               temporary1:                               0,
               temporary2:                               0,
               interrupted_sp:                           0,
               defer_allowed:                            0,
               local:                                    0,
               #[cfg(feature = "event-trace")]
               observed:                                 0 }
    }
}
const CONTROL_STRIDE: usize = size_of::<Control>();
const _: () = assert!(CONTROL_STRIDE != 0 && CONTROL_STRIDE % align_of::<Control>() == 0);

/// Cold primary metadata is explicit and allocation-free. It is never a
/// fallback once REGISTRY exists, and cannot mint a runtime HartKey.
struct Cold
{
    control: spinning_top::Spinlock<Control>,
    local:   HartLocal
}
static COLD: spin::Once<Cold> = spin::Once::new();
static REGISTRY: spin::Once<Registry> = spin::Once::new();

pub(super) struct Hart
{
    _stack:   &'static Stack,
    baseline: Foundation<SavedState>,
    control:  usize,
    local:    HartLocal,
    online:   AtomicBool
}
/// Control has plain assembly-mutated words. Owning only raw Backing avoids
/// freezing those words through an accidental immutable typed slice.
struct ControlArena
{
    backing: Foundation<Backing>,
    bytes:   usize
}
impl ControlArena
{
    fn new(count: usize) -> Result<Self, Error>
    {
        let bytes = count.checked_mul(CONTROL_STRIDE)
                         .filter(|bytes| *bytes != 0)
                         .ok_or(Error::RegistryLayout)?;
        let backing = Foundation::<Backing>::allocate(bytes).map_err(|cause| {
                          Error::Foundation { resource: "entry control arena",
                                              cause }
                      })?;
        if backing.cpu_base() % align_of::<Control>() != 0
           || backing.cpu_base().checked_add(bytes).is_none()
           || backing.bytes() < bytes as u64
        {
            return Err(Error::RegistryLayout);
        }
        Ok(Self { backing,
                  bytes })
    }

    fn at(&self, index: usize) -> usize
    {
        // Only preparation's index<len initializer calls this. new checked
        // the complete product and address span before any row is created.
        self.backing.cpu_base() + index * CONTROL_STRIDE
    }
}
struct Registry
{
    // Before publication, failure drops every constructed row/CPU owner before
    // the raw control lease. After publication the registry lives until reset.
    rows:     OwnedArray<Hart>,
    controls: ControlArena
}
impl Registry
{
    fn selected(&'static self, control: usize) -> Option<&'static Hart>
    {
        let offset = control.checked_sub(self.controls.backing.cpu_base())?;
        // Prove logical arena membership/alignment BEFORE any Control load;
        // the rounded buddy tail is not an admitted row.
        if offset >= self.controls.bytes
           || offset % CONTROL_STRIDE != 0
           || control % align_of::<Control>() != 0
        {
            return None;
        }
        let row = self.rows.as_slice().get(offset / CONTROL_STRIDE)?;
        if row.control != control || !row.online.load(Ordering::Acquire)
        {
            return None;
        }
        // SAFETY: the immutable row and logical arena authenticated this exact
        // permanently owned address. Borrow only one scalar, never &Control.
        let bound = unsafe { ptr::addr_of!((*(control as *const Control)).local).read_volatile() };
        (bound == ptr::from_ref(&row.local) as usize).then_some(row)
    }
}
#[derive(Debug)]
pub(crate) enum Error
{
    AlreadyInitialized,
    ForeignHart
    {
        admitted: usize
    },
    MissingColdAdmission,
    ColdSelectionInUse,
    MissingRegistryInputs,
    RegistryLayout,
    StackBinding
    {
        hart: usize
    },
    InterruptsEnabled
    {
        status: usize
    },
    Foundation
    {
        resource: &'static str,
        cause:    foundation::Error
    },
    Cpu
    {
        hart:  u64,
        cause: foundation::Error
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::ForeignHart { admitted } =>
            {
                write!(f, "native entry admits only primary hart0; admitted boot hart={admitted}")
            }
            Self::MissingColdAdmission =>
            {
                f.write_str("System registry requires the actual admitted cold primary sscratch \
                             control")
            }
            Self::ColdSelectionInUse =>
            {
                f.write_str("cold primary selection still owns active/current/IO/idle state; \
                             registry transition refused")
            }
            Self::AlreadyInitialized =>
            {
                f.write_str("System hart entry registry is already initialized; reset before \
                             retrying")
            }
            Self::MissingRegistryInputs =>
            {
                f.write_str("System hart registry requires matching checked CPU topology and \
                             permanent service-stack arrays")
            }
            Self::RegistryLayout =>
            {
                f.write_str("System hart registry control extent is empty, misaligned or overflows")
            }
            Self::StackBinding { hart } =>
            {
                write!(f,
                       "System entry stack does not match the permanent service-stack slot for \
                        primary hart {hart}")
            }
            Self::InterruptsEnabled { status } =>
            {
                write!(f,
                       "System stack bootstrap requires local interrupts masked; \
                        sstatus={status:#x}; fix firmware admission and rebuild")
            }
            Self::Foundation { resource,
                               cause } =>
            {
                write!(f, "System {resource} reservation refused: {cause}")
            }
            Self::Cpu { hart,
                        cause } =>
            {
                write!(f, "System hart {hart} baseline CPU reservation refused: {cause}")
            }
        }
    }
}
fn scratch() -> usize
{
    let control;
    // SAFETY: only this audited native binding reads the local selection CSR.
    unsafe {
        asm!("csrr {},sscratch",out(reg)control,options(nomem,nostack));
    }
    control
}
/// Actual checked primary cold admission, before pool/heap initialization.
/// Poisoned Control cannot serve trap frames and confers no secondary authority.
pub(crate) fn admit_bootstrap(admitted: usize) -> Result<(), Error>
{
    let status: usize;
    unsafe {
        asm!("csrr {},sstatus",out(reg)status,options(nomem,nostack));
    }
    if status & 2 != 0
    {
        return Err(Error::InterruptsEnabled { status });
    }
    if admitted != 0
    {
        return Err(Error::ForeignHart { admitted });
    }
    if COLD.get().is_some() || REGISTRY.get().is_some()
    {
        return Err(Error::AlreadyInitialized);
    }
    let cold = COLD.call_once(|| {
                       Cold { control: spinning_top::Spinlock::new(Control::poison()),
                              local:   HartLocal::new(admitted) }
                   });
    let pointer = {
        let mut control = cold.control.lock();
        control.local = ptr::from_ref(&cold.local) as usize;
        ptr::from_ref(&*control) as usize
    };
    // SAFETY: this masked primary owns cold admission; stable BSS metadata was
    // initialized before the actual CSR can name it. No System vector is active.
    unsafe {
        asm!("csrw sscratch,{}",in(reg)pointer,options(nostack));
    }
    Ok(())
}
// Private bounded lookup; every caller masks its complete scalar/pin access.
fn selected_local() -> Option<&'static HartLocal>
{
    let control = scratch();
    if let Some(registry) = REGISTRY.get()
    {
        return registry.selected(control).map(|row| &row.local);
    }
    let cold = COLD.get()?;
    let (expected, bound) = {
        let selected = cold.control.lock();
        (ptr::from_ref(&*selected) as usize, selected.local)
    };
    (control == expected && bound == ptr::from_ref(&cold.local) as usize).then_some(&cold.local)
}
fn selected_hart() -> Option<&'static Hart>
{
    REGISTRY.get()?.selected(scratch())
}
fn unadmitted() -> !
{
    // Invalid System execution selection is contract failure, distinct from
    // ROM's intentional secondary parking. No context/firmware lookup occurs.
    unsafe {
        asm!("csrci sstatus,2", options(nostack));
    }
    crate::machine::emergency::serial(format_args!("SYSTEM_HART_SELECTION_FAILED \
                                                    sscratch={:#x}; no matching admitted \
                                                    execution control\n",
                                                   scratch()));
    crate::machine::power::fatal_shutdown()
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
        asm!("csrr {},sstatus",out(reg)status,options(nomem,nostack));
    }
    if status & 2 != 0
    {
        return Err(Error::InterruptsEnabled { status });
    }
    if REGISTRY.get().is_some()
    {
        return Err(Error::AlreadyInitialized);
    }
    let cold = selected_local().ok_or(Error::MissingColdAdmission)?;
    if !cold.is_cold()
    {
        return Err(Error::ColdSelectionInUse);
    }
    let primary = cold.id;
    let cpus = crate::machine::paging::enabled_cpus().ok_or(Error::MissingRegistryInputs)?;
    let stacks = crate::machine::paging::system_stacks().ok_or(Error::MissingRegistryInputs)?;
    if cpus.is_empty() || cpus.len() != stacks.len()
    {
        return Err(Error::MissingRegistryInputs);
    }
    let primary_index = cpus.binary_search_by_key(&(primary as u64), |cpu| cpu.hart)
                            .map_err(|_| Error::MissingRegistryInputs)?;
    let expected: &'static Stack = &stacks[primary_index];
    if !ptr::eq(stack, expected)
    {
        return Err(Error::StackBinding { hart: primary });
    }
    let controls = ControlArena::new(cpus.len())?;
    let rows = OwnedArray::try_new(cpus.len(), |index| -> Result<Hart, foundation::Error> {
                   let baseline = Foundation::<SavedState>::new(TrapFrame::default())?;
                   let mapped: &'static Stack = &stacks[index];
                   // Native execution is RV64. The numeric value is a label only; the
                   // immutable row/control binding, not this cast, authenticates selection.
                   Ok(Hart { _stack: mapped,
                             baseline,
                             control: controls.at(index),
                             local: HartLocal::new(cpus[index].hart as usize),
                             online: AtomicBool::new(index == primary_index) })
               }).map_err(|error| {
                     match error
                     {
                         ArrayError::Storage(cause) =>
                         {
                             Error::Foundation { resource: "hart registry metadata",
                                                 cause }
                         }
                         ArrayError::Element { index,
                                               cause } =>
                         {
                             Error::Cpu { hart: cpus[index].hart,
                                          cause }
                         }
                     }
                 })?;
    for row in rows.as_slice()
    {
        let state = Control { entering: usize::from(!row.online.load(Ordering::Relaxed)),
                              frames: row.baseline.frame() as usize,
                              stride: row.baseline.stride(),
                              floor: row._stack.top() - row._stack.bytes() as usize,
                              top: row._stack.top(),
                              local: ptr::from_ref(&row.local) as usize,
                              ..Control::poison() };
        // SAFETY: exclusive unpublished raw Control slots, with stable row/
        // CPU/stack ownership. No &Control or slice can freeze runtime words.
        unsafe {
            (row.control as *mut Control).write(state);
        }
    }
    let registry = REGISTRY.call_once(|| {
                               Registry { rows,
                                          controls }
                           });
    // Publication -> sscratch transition is deliberately allocation/drop/
    // callback-free with SIE clear. Once REGISTRY exists the old cold pointer
    // is invalid; only the pure M monitor may interrupt this tiny transition.
    let hart = &registry.rows.as_slice()[primary_index];
    unsafe {
        asm!("csrw sscratch,{}",in(reg)hart.control,options(nostack));
    }
    serial!("SYSTEM_STACK_READY base={:#x} bytes={} control={:#x} cpu_slots={} cpu_stride={} \
             prepared_harts={} control_bytes={}",
            stack.top() - stack.bytes() as usize,
            stack.bytes(),
            hart.control,
            super::CPU_SLOTS,
            hart.baseline.stride(),
            registry.rows.as_slice().len(),
            registry.controls.backing.bytes());
    Ok(PreparedEntry { hart,
                       _local: core::marker::PhantomData })
}
/// Complete outer capture on this hart's System stack. Its raw frame address
/// carries no surviving Rust borrow; only native entry may mint this proof.
/// There is deliberately no Drop action: execution ownership spans sret.
pub(crate) struct OuterCapture
{
    frame:   *mut TrapFrame,
    hart:    execution::HartKey,
    current: usize,
    _local:  core::marker::PhantomData<*mut ()>
}
impl OuterCapture
{
    pub(crate) fn frame(&self) -> *mut TrapFrame
    {
        self.frame
    }

    pub(crate) fn hart(&self) -> execution::HartKey
    {
        self.hart
    }

    pub(crate) fn current(&self) -> usize
    {
        self.current
    }
}
/// # Safety
/// Sole assembly entry supplied this complete live frame. All Rust frame and
/// service borrows must have ended before the scheduler consumes this capture.
pub(super) unsafe fn captured_outer(frame: *mut TrapFrame) -> OuterCapture
{
    let Some(hart) = selected_hart()
    else
    {
        super::entry_lost("outer capture has no admitted hart");
    };
    let status: usize;
    // SAFETY: checked selected Control has reset-lifetime backing; native entry
    // owns it masked. Verify the address before touching any supplied frame.
    let valid = unsafe {
        asm!("csrr {},sstatus", out(reg)status, options(nostack));
        let control = hart.control as *const Control;
        status & 2 == 0
        && ptr::addr_of!((*control).entering).read_volatile() == 0
        && ptr::addr_of!((*control).depth).read_volatile() == 1
        && ptr::addr_of!((*control).base_depth).read_volatile() == 0
        && ptr::addr_of!((*control).frames).read_volatile() == frame as usize
        && ptr::addr_of!((*frame).prior_depth).read() == 0
    };
    if !valid
    {
        super::entry_lost("scheduling requires complete outer capture on the selected System \
                           stack");
    }
    OuterCapture { frame,
                   hart: execution::selected_hart(),
                   current: execution::current(),
                   _local: core::marker::PhantomData }
}

/// The scheduler retains the selected core until the CPU and its readers leave.
/// An unchanged frame and bootstrap baseline perform no control-memory write.
pub(crate) fn publish(state: &SavedState)
{
    crate::machine::interrupts::with_masked(|| {
        let Some(hart) = selected_hart()
        else
        {
            unadmitted();
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
    })
}
/// Bootstrap resident work uses the same System stack as later trap services.
/// Base depth1 preserves its live Rust frames across definition ECALLs.
impl PreparedEntry
{
    pub(crate) fn run<F: FnOnce() -> R, R>(self, function: F) -> R
    {
        let status: usize;
        // SAFETY: this single-use authority may enter only with its admitted
        // local interrupts still masked; it cannot migrate between CPUs.
        unsafe {
            asm!("csrr {},sstatus", out(reg)status, options(nostack));
        }
        if status & 2 != 0
        {
            unadmitted();
        }
        let hart = self.hart;
        if !selected_hart().is_some_and(|selected| core::ptr::eq(selected, hart))
        {
            unadmitted();
        }
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
    crate::machine::interrupts::with_masked(|| {
        let Some(hart) = selected_hart()
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
    })
}

pub(crate) fn service_scope<R>(allowed: bool, call: impl FnOnce() -> R) -> R
{
    struct Restore
    {
        control:  usize,
        field:    *mut usize,
        previous: usize,
        _local:   core::marker::PhantomData<*mut ()>
    }
    impl Drop for Restore
    {
        fn drop(&mut self)
        {
            crate::machine::interrupts::with_masked(|| {
                if scratch() != self.control
                {
                    unadmitted();
                }
                // SAFETY: same admitted CPU/control, one scalar restoration.
                unsafe {
                    self.field.write_volatile(self.previous);
                }
            });
        }
    }
    let _restore = crate::machine::interrupts::with_masked(|| {
        let Some(hart) = selected_hart()
        else
        {
            unadmitted();
        };
        // SAFETY: authenticated permanent control; no reference or guard spans
        // the closure/nested ECALL, just this private restoration metadata.
        let field = unsafe { ptr::addr_of_mut!((*(hart.control as *mut Control)).defer_allowed) };
        let previous = unsafe { field.read_volatile() };
        unsafe {
            field.write_volatile(usize::from(allowed));
        }
        Restore { control: hart.control,
                  field,
                  previous,
                  _local: core::marker::PhantomData }
    });
    call()
}

pub(crate) fn service_depth(physical: usize) -> usize
{
    crate::machine::interrupts::with_masked(|| {
        let Some(hart) = selected_hart()
        else
        {
            unadmitted();
        };
        let baseline = unsafe {
            ptr::addr_of!((*(hart.control as *const Control)).base_depth).read_volatile()
        };
        match physical.checked_sub(baseline)
        {
            Some(depth) if depth != 0 => depth,
            _ => super::entry_lost("service depth is below its bootstrap baseline")
        }
    })
}

/// Two bounded observations in traced private boots, not a per-event serial
/// stream. Real Rust SP plus the actual entry frame qualify the assembly seam.
#[cfg(feature = "event-trace")]
pub(crate) fn observe_entry(frame: usize)
{
    crate::machine::interrupts::with_masked(|| {
        let Some(hart) = selected_hart()
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
    })
}

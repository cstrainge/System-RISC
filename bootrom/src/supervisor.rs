//! Audited RV64 M-to-S admission. Firmware is still the executor here; normal
//! resident code never reads M-mode registers after the non-returning handoff.
use core::{arch::{asm, global_asm},
           fmt,
           marker::PhantomData};

use mac_abi::boot::monitor::{self, BootImage, SecondaryStart};
use mac_kernel_model::pages::PhysicalAddress;

const MIE: u64 = 1 << 3;
const MPRV: u64 = 1 << 17;
const MPP: u64 = 3 << 11;
const SUPERVISOR: u64 = 1 << 11;
const SV39: u64 = 8 << 60;
macro_rules! csr
{
    ($read:ident, $write:ident, $name:literal) => {
        unsafe fn $read() -> u64 {
            let value;
            // SAFETY: private M-mode bootstrap caller owns this hart.
            unsafe { asm!(concat!("csrr {value}, ", $name), value=out(reg)value, options(nostack)); }
            value
        }
        unsafe fn $write(value: u64) {
            // SAFETY: private bootstrap writes only these architecture CSRs.
            // No nomem: mapping/permission publication cannot cross the write.
            unsafe { asm!(concat!("csrw ", $name, ", {value}"), value=in(reg)value, options(nostack)); }
        }
    };
}
csr!(status, set_status, "mstatus");
csr!(environment, set_environment, "menvcfg");
csr!(counters, set_counters, "mcounteren");
csr!(exceptions, set_exceptions, "medeleg");
csr!(interrupts, set_interrupts, "mideleg");
csr!(permissions, set_permissions, "pmpcfg0");
csr!(physical_limit, set_physical_limit, "pmpaddr0");
csr!(root, set_root, "satp");
unsafe fn set_timer(value: u64)
{
    // SAFETY: Previous records this CSR only after the shared STCE probe.
    unsafe {
        asm!("csrw 0x14d,{}",in(reg)value,options(nostack));
    }
}

#[derive(Debug)]
pub enum Error
{
    BootstrapState
    {
        status: u64, root: u64
    },
    Root
    {
        physical: u64
    },
    Probe
    {
        step: u64, required: u64, observed: u64
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::BootstrapState { status,
                                   root } =>
            {
                write!(f,
                       "supervisor admission requires cold M-mode bootstrap with interrupts/MPRV \
                        off and no active root; mstatus={status:#x} satp={root:#x}")
            }
            Self::Root { physical } =>
            {
                write!(f,
                       "firmware root {physical:#x} must be 4096-byte aligned and fit the Sv39 \
                        physical address field")
            }
            Self::Probe { step,
                          required,
                          observed } =>
            {
                write!(f,
                       "platform admission step {step} ({}): requirement {required:#x}, observed \
                        {observed:#x}; select a compatible platform/CPU",
                       step_name(*step))
            }
        }
    }
}
struct Previous
{
    status:         u64,
    environment:    u64,
    counters:       u64,
    exceptions:     u64,
    interrupts:     u64,
    permissions:    u64,
    physical_limit: u64,
    root:           u64,
    timer:          Option<u64>
}
impl Drop for Previous
{
    fn drop(&mut self)
    {
        // SAFETY: cold bootstrap remains M mode until enter consumes this
        // restoration. Every restored value was read from this same hart.
        unsafe {
            set_root(self.root);
            asm!("sfence.vma zero,zero", options(nostack));
            if let Some(timer) = self.timer
            {
                set_timer(timer);
            }
            set_interrupts(self.interrupts);
            set_exceptions(self.exceptions);
            set_counters(self.counters);
            set_environment(self.environment);
            set_physical_limit(self.physical_limit);
            set_permissions(self.permissions);
            set_status(self.status);
        }
    }
}
/// Not Send/Sync: the CSR state and its restoration belong to this hart only.
pub struct Admitted
{
    previous:     Option<Previous>,
    root:         u64,
    asid_mask:    u16,
    vector_bytes: u64,
    _hart:        PhantomData<*mut ()>
}
#[repr(C)]
struct PrimaryReport
{
    report:         SecondaryStart,
    previous_timer: u64,
    timer_valid:    u64
}
const _: () = assert!(core::mem::offset_of!(PrimaryReport, previous_timer) == 96
                      && core::mem::offset_of!(PrimaryReport, timer_valid) == 104);
pub(crate) const PRIMARY_TIMER_OFFSET: usize = core::mem::offset_of!(PrimaryReport, previous_timer);
pub(crate) const PRIMARY_TIMER_VALID_OFFSET: usize =
    core::mem::offset_of!(PrimaryReport, timer_valid);
unsafe extern "C" {
    fn __monitor_primary_admission(report: *mut PrimaryReport, requirements: *const BootImage,
                                   root: u64)
                                   -> u64;
}
fn step_name(step: u64) -> &'static str
{
    match step
    {
        monitor::step::ISA => "RV64 major ISA profile",
        monitor::step::WIDTH => "64-bit supervisor register width",
        monitor::step::ENVIRONMENT => "Sstc/cache-management permissions",
        monitor::step::COUNTERS => "supervisor time counter",
        monitor::step::DELEGATION => "supervisor delegation and machine fatal ownership",
        monitor::step::PMP => "unlocked NAPOT physical coverage",
        monitor::step::ROOT => "Sv39 root physical frame",
        monitor::step::ASID => "contiguous compatible hardware tags",
        monitor::step::VECTOR => "compatible vector register length",
        _ => "native supervisor admission state"
    }
}
impl Admitted
{
    /// # Safety
    /// Called on the bootstrap hart in M mode after ISA admission, with MIE
    /// and MPRV clear. Root points at a fully owned initialized firmware tree;
    /// its owner stays pinned through admission and all subsequent hardware use.
    pub unsafe fn prepare(root_page: PhysicalAddress, required_physical_limit: u64)
                          -> Result<Self, Error>
    {
        if !root_page.0.is_multiple_of(4096) || root_page.0 >= 1 << 56
        {
            return Err(Error::Root { physical: root_page.0 });
        }
        let (status, root) = unsafe { (status(), root()) };
        if status & (MIE | MPRV) != 0 || root != 0
        {
            return Err(Error::BootstrapState { status,
                                               root });
        }
        let mut previous = unsafe {
            Previous { status,
                       root,
                       environment: environment(),
                       counters: counters(),
                       exceptions: exceptions(),
                       interrupts: interrupts(),
                       permissions: permissions(),
                       physical_limit: physical_limit(),
                       timer: None }
        };
        let requirements = BootImage::new();
        use core::sync::atomic::Ordering;
        requirements.required_isa.store(monitor::REQUIRED_ISA_BITS, Ordering::Relaxed);
        requirements.physical_limit.store(required_physical_limit, Ordering::Relaxed);
        let mut report = PrimaryReport { report:         SecondaryStart::new(),
                                         previous_timer: 0,
                                         timer_valid:    0 };
        let root = SV39 | (root_page.0 >> 12);
        // SAFETY: the shared integer/CSR leaf runs on this actual primary's
        // full boot stack. It never calls Rust and writes only this unique
        // report. Previous retains restoration for every admitted CSR.
        let step = unsafe { __monitor_primary_admission(&mut report, &requirements, root) };
        if report.timer_valid != 0
        {
            previous.timer = Some(report.previous_timer);
        }
        if step != 0
        {
            return Err(Error::Probe { step,
                                      required: report.report
                                                      .observed_aux
                                                      .load(Ordering::Relaxed),
                                      observed: report.report.observed.load(Ordering::Relaxed) });
        }
        Ok(Self { previous: Some(previous),
                  root,
                  asid_mask: report.report.hardware_tag_mask.load(Ordering::Relaxed) as u16,
                  vector_bytes: report.report.vector_bytes.load(Ordering::Relaxed),
                  _hart: PhantomData })
    }

    pub fn asid_mask(&self) -> u16
    {
        self.asid_mask
    }

    pub fn vector_bytes(&self) -> u64
    {
        self.vector_bytes
    }

    /// # Safety
    /// The entry, BootInfo and stack are mapped supervisor virtual addresses;
    /// all firmware owners and the original borrowed boot storage remain pinned
    /// until reset. The caller must not rebase or transport Rust references.
    /// Both escape stubs are executable in the firmware root, and M trap entry
    /// must switch to the reserved physical emergency stack before using Rust.
    pub unsafe fn enter(mut self, entry: u64, info: u64, stack: u64) -> !
    {
        let mut status = unsafe { self::status() };
        // Enter ordinary little-endian S mode, never virtualized VS mode. Allow
        // SRET, WFI and SATP; the resident still masks interrupts at first entry.
        status &= !(MPP
                    | MIE
                    | MPRV
                    | (1 << 20)
                    | (1 << 21)
                    | (1 << 22)
                    | (1 << 36)
                    | (1 << 37)
                    | (1 << 39)
                    | (1 << 1)
                    | (1 << 5)
                    | (1 << 7));
        status |= SUPERVISOR | 0x6600;
        // Consume, rather than run, the restoration only on the non-returning
        // path. The physical root owner remains in the suspended ROM frame.
        let _previous = core::mem::ManuallyDrop::new(self.previous.take());
        unsafe {
            asm!(
                "csrw satp,a4", "sfence.vma zero,zero", "fence.i",
                "csrw sie,zero", "csrw sscratch,zero", "csrw stvec,t0",
                "csrw mepc,a3", "csrw mstatus,a6", "mv ra,a7", "mv sp,a5", "mret",
                in("a0")info,
                in("a1")core::mem::size_of::<mac_abi::BootInfo>(),
                in("a2")monitor::ENTRY_PRIMARY,
                in("a3")entry, in("a4")self.root, in("a5")stack,
                in("a6")status,
                in("a7")__bootstrap_supervisor_returned as *const () as usize,
                in("t0")__bootstrap_supervisor_fault as *const () as usize,
                options(noreturn)
            );
        }
    }
}
unsafe extern "C" {
    fn __bootstrap_supervisor_fault();
    fn __bootstrap_supervisor_returned();
}
global_asm!(
            r#"
.section .text.bootstrap_supervisor,"ax"
.balign 4
.global __bootstrap_supervisor_fault
__bootstrap_supervisor_fault:
ebreak
j __bootstrap_supervisor_fault
.global __bootstrap_supervisor_returned
__bootstrap_supervisor_returned:
ebreak
j __bootstrap_supervisor_returned
"#
);

pub(crate) fn fault_entry() -> u64
{
    __bootstrap_supervisor_fault as *const () as u64
}
pub(crate) fn returned_entry() -> u64
{
    __bootstrap_supervisor_returned as *const () as u64
}

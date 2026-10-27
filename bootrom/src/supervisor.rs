//! Audited RV64 M-to-S admission. Firmware is still the executor here; normal
//! resident code never reads M-mode registers after the non-returning handoff.
use core::{arch::{asm, global_asm},
           fmt,
           marker::PhantomData};

use mac_kernel_model::pages::PhysicalAddress;

const MIE: u64 = 1 << 3;
const MPRV: u64 = 1 << 17;
const MPP: u64 = 3 << 11;
const SUPERVISOR: u64 = 1 << 11;
const SXL: u64 = 3 << 34;
const SXL64: u64 = 2 << 34;
const STCE: u64 = 1 << 63;
const CACHE_PERMISSIONS: u64 = (3 << 4) | (1 << 6) | (1 << 7);
const TIMER: u64 = 1 << 5;
// Breakpoint deliberately remains machine-owned for the allocation-free fatal
// escape. S-mode ECALL is delegated horizontally for the native trap gateway.
const EXCEPTIONS: u64 = (1 << 0)
                        | (1 << 1)
                        | (1 << 2)
                        | (1 << 4)
                        | (1 << 5)
                        | (1 << 6)
                        | (1 << 7)
                        | (1 << 8)
                        | (1 << 9)
                        | (1 << 12)
                        | (1 << 13)
                        | (1 << 15);
const SV39: u64 = 8 << 60;
const PPN: u64 = (1 << 44) - 1;
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
csr!(timer, set_timer, "0x14d");

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
    Feature
    {
        name: &'static str, required: u64, observed: u64
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
            Self::Feature { name,
                            required,
                            observed } =>
            {
                write!(f,
                       "platform cannot provide {name}: required bits {required:#x}, observed \
                        {observed:#x}; select a compatible platform/CPU")
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
    previous:  Option<Previous>,
    root:      u64,
    asid_mask: u16,
    _hart:     PhantomData<*mut ()>
}
fn require(name: &'static str, required: u64, observed: u64) -> Result<(), Error>
{
    if observed & required != required
    {
        Err(Error::Feature { name,
                             required,
                             observed })
    }
    else
    {
        Ok(())
    }
}
impl Admitted
{
    /// # Safety
    /// Called on the bootstrap hart in M mode after ISA admission, with MIE
    /// and MPRV clear. Root points at a fully owned initialized firmware tree;
    /// its owner stays pinned through admission and all subsequent hardware use.
    pub unsafe fn prepare(root_page: PhysicalAddress) -> Result<Self, Error>
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
        unsafe {
            set_status((previous.status & !SXL) | SXL64);
        }
        let configured_width = unsafe { self::status() } & SXL;
        if configured_width != SXL64
        {
            return Err(Error::Feature { name:     "64-bit supervisor address/register width",
                                        required: SXL64,
                                        observed: configured_width });
        }
        // All probes are WARL fields. Unsupported timer CSRs are never touched
        // before STCE readback confirms their existence and access contract.
        unsafe {
            set_environment(previous.environment | STCE | CACHE_PERMISSIONS);
        }
        let configured = unsafe { environment() };
        require("supervisor timer compare (Sstc)", STCE, configured)?;
        require("supervisor cache-management permissions", CACHE_PERMISSIONS, configured)?;
        unsafe {
            set_counters(previous.counters | 7);
        }
        require("supervisor time counter", 2, unsafe { counters() })?;
        previous.timer = Some(unsafe { timer() });
        unsafe {
            set_timer(u64::MAX);
            set_exceptions(EXCEPTIONS);
        }
        require("supervisor fault and native ECALL delegation", EXCEPTIONS, unsafe {
            exceptions()
        })?;
        let delegated = unsafe { exceptions() };
        if delegated & (1 << 3) != 0
        {
            return Err(Error::Feature { name:     "machine-owned fatal breakpoint",
                                        required: 0,
                                        observed: delegated & (1 << 3) });
        }
        unsafe {
            set_interrupts(TIMER);
        }
        require("supervisor timer interrupt delegation", TIMER, unsafe { interrupts() })?;
        unsafe {
            set_physical_limit(u64::MAX);
            set_permissions((previous.permissions & !255) | 0x1f);
        }
        let access = unsafe { permissions() } & 255;
        if access != 0x1f
        {
            return Err(Error::Feature { name:     "unlocked supervisor PMP read/write/execute",
                                        required: 0x1f,
                                        observed: access });
        }
        let root = SV39 | (root_page.0 >> 12);
        unsafe {
            set_root(root | (0xffff << 44));
        }
        let observed = unsafe { self::root() };
        if observed >> 60 != 8
        {
            return Err(Error::Feature { name: "Sv39 page translation",
                                        required: SV39,
                                        observed });
        }
        if observed & PPN != root_page.0 >> 12
        {
            return Err(Error::Feature { name:     "Sv39 root physical frame",
                                        required: root_page.0 >> 12,
                                        observed: observed & PPN });
        }
        let asid_mask = ((observed >> 44) & 0xffff) as u16;
        if !(u32::from(asid_mask) + 1).is_power_of_two()
        {
            return Err(Error::Feature { name:     "contiguous low ASID field",
                                        required: 0,
                                        observed: u64::from(asid_mask) });
        }
        // ASID zero is reserved for the boot firmware root. The resident
        // allocates and validates process tag lifetimes through its facility.
        unsafe {
            set_root(root);
            asm!("sfence.vma zero,zero", options(nostack));
        }
        Ok(Self { previous: Some(previous),
                  root,
                  asid_mask,
                  _hart: PhantomData })
    }

    pub fn asid_mask(&self) -> u16
    {
        self.asid_mask
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
                "csrw satp,a3", "sfence.vma zero,zero", "fence.i",
                "csrw sie,zero", "csrw sscratch,zero",
                "csrw stvec,a5", "csrw mepc,a1", "csrw mstatus,a4",
                "mv ra,a6", "mv sp,a2", "mret",
                in("a0")info, in("a1")entry, in("a2")stack,
                in("a3")self.root, in("a4")status,
                in("a5")__bootstrap_supervisor_fault as *const () as usize,
                in("a6")__bootstrap_supervisor_returned as *const () as usize,
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

//! Direct Host callbacks borrow their caller only after the actual scheduled
//! thread has a canonical primary-service lease. This private native gate is
//! not a public placement grant or an independently issued thread identity.
#![allow(unsafe_code)]
use core::{arch::global_asm,
           mem::{align_of, size_of},
           ptr};

use mac_abi::{BAD_ABI, BAD_ARGUMENT, OK, Status};

use super::{Route, ServiceLease};
use crate::machine::traps::TrapFrame;

/// Private System-to-System gate, distinct from public Toolbox services and
/// the module worker MWRK gate. The sole callsite passes an exclusive live
/// `Option<ServiceLease>` output on its actual, retained continuation stack.
pub(crate) const TRAP: u32 = 0x4d535256; // MSRV

global_asm!(
    ".pushsection .text.system_primary_service,\"ax\",@progbits",
    ".balign 4",
    ".global __system_primary_service_acquire",
    ".type __system_primary_service_acquire,@function",
    "__system_primary_service_acquire:",
    "li a7, {trap}",
    ".global __system_primary_service_acquire_ecall",
    "__system_primary_service_acquire_ecall:",
    "ecall",
    "ret",
    ".size __system_primary_service_acquire, .-__system_primary_service_acquire",
    ".popsection",
    trap = const TRAP
);
unsafe extern "C" {
    fn __system_primary_service_acquire(output: *mut (), bytes: usize) -> isize;
    fn __system_primary_service_acquire_ecall();
}

/// Acquire before interpreting Host request pointers, entering a module scope,
/// taking IRQ/manager guards or invoking the caller's thunk. The caller checks
/// the appropriate policy operation FIRST. The canonical primary fast path
/// checks placement and acquires its lease in the same masked metadata turn;
/// a timer cannot migrate the thread between those two operations.
pub(crate) fn acquire() -> Result<ServiceLease, Status>
{
    if let Some(lease) = super::try_acquire_host_here()?
    {
        return Ok(lease);
    }
    let mut output = None::<ServiceLease>;
    // SAFETY: this exact System stub and exclusive stack output are retained
    // by the same canonical THRD across migration. The peer first turn touches
    // no output; primary replay writes one actual owning lease. No closure,
    // application pointer or borrowed module state is transported.
    let returned = unsafe {
        __system_primary_service_acquire(ptr::from_mut(&mut output).cast(),
                                         size_of::<Option<ServiceLease>>())
    };
    let status = Status::try_from(returned).unwrap_or(BAD_ABI);
    match (status, output)
    {
        (OK, Some(lease)) => Ok(lease),
        (cause, None) if cause < OK => Err(cause),
        (_, lease) =>
        {
            // An inconsistent private reply is a typed contract refusal. If
            // it nevertheless contains a genuine lease, finish that obligation
            // here instead of abandoning a canonical depth or ownership pin.
            drop(lease);
            Err(BAD_ABI)
        }
    }
}

pub(crate) enum Outcome
{
    Resume,
    Migrate
}

fn refuse(frame: &mut TrapFrame, status: Status) -> Outcome
{
    // Commit refusal before releasing only its matching pending route. A
    // definitive acquire error must not strand the temporary Primary obligation.
    super::refuse_host(frame, status);
    Outcome::Resume
}

/// # Safety
/// Only the actual native trap dispatcher supplies its exclusive live complete
/// frame, before caller request views, trap scopes or resident guards exist.
/// Policy rejection has already run. A valid sole System callsite guarantees
/// its output pointer's lifetime/exclusivity; shared supervisor mappings do not
/// provide arbitrary malicious pointer isolation or privileged-fault recovery.
pub(crate) unsafe fn dispatch(frame: &mut TrapFrame) -> Outcome
{
    let output = frame.registers[8] as *mut Option<ServiceLease>;
    if frame.pc != __system_primary_service_acquire_ecall as *const () as usize
       || frame.registers[9] != size_of::<Option<ServiceLease>>()
       || output.is_null()
       || !(output as usize).is_multiple_of(align_of::<Option<ServiceLease>>())
    {
        return refuse(frame, BAD_ARGUMENT);
    }
    // Only scalar shape/actual canonical claim checks happen on the peer.
    // In particular no output reference is constructed before actual routing.
    match super::host_route(frame)
    {
        Ok(Route::Migrate) => return Outcome::Migrate,
        Ok(Route::Here) => (),
        Err(status) => return refuse(frame, status)
    }
    let lease = match super::acquire_host(frame)
    {
        Ok(lease) => lease,
        Err(status) => return refuse(frame, status)
    };
    // SAFETY: exact sole System callsite plus native entry contract above. The
    // canonical acquisition's final commit owns this pin/depth; this store and
    // ordinary result/PC commit are the only subsequent operations, neither is
    // fallible or calls foreign code. The output is initially None, never read.
    unsafe { output.write(Some(lease)) };
    frame.registers[8] = OK as usize;
    frame.pc = frame.pc.wrapping_add(4);
    Outcome::Resume
}

//! The class-neutral C entry boundary. Borrowed byte buffers never transfer
//! allocator ownership. Class request/reply bytes have their own schemas.
use crate::BROKER_ABI;
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Header
{
    pub version: u32,
    pub size:    u32
}
impl Header
{
    pub const fn of<T>() -> Self
    {
        Self { version: BROKER_ABI,
               size:    core::mem::size_of::<T>() as u32 }
    }

    pub fn valid<T>(&self) -> bool
    {
        self.version == BROKER_ABI && self.size as usize == core::mem::size_of::<T>()
    }
}
/// Module entry receives a borrowed host descriptor, valid for the module and all of its
/// outstanding allocations. A successful entry initializes Instance; no service is public before
/// commit.
pub type Entry = unsafe extern "C" fn(*const Host, *mut Instance) -> i32;
pub type Dispatch = unsafe extern "C" fn(u64, *mut Call) -> i32;
pub type Resolve = unsafe extern "C" fn(u64, u32, u32, *mut u64) -> i32;
pub type Broker = unsafe extern "C" fn(u64, u64, *mut Call) -> i32;
/// Allocations use the current admitted subject. Failure leaves `out` null.
pub type Allocate = unsafe extern "C" fn(u64, usize, usize, *mut *mut u8) -> i32;
/// Disposal validates the original allocation receipt and exact caller layout.
pub type Deallocate = unsafe extern "C" fn(u64, *mut u8, usize, usize) -> i32;
/// Failure preserves the original allocation and leaves `out` null.
pub type Reallocate = unsafe extern "C" fn(u64, *mut u8, usize, usize, usize, *mut *mut u8) -> i32;
pub type AllocationOperation = unsafe extern "C" fn(*mut core::ffi::c_void) -> i32;
/// Synchronous native exclusion starts before invoking the borrowed thunk.
/// Neither thunk nor state may escape; return its nonpositive status unchanged.
pub type CriticalSection =
    unsafe extern "C" fn(u64, AllocationOperation, *mut core::ffi::c_void) -> i32;
pub type Workers = unsafe extern "C" fn(u64, *mut WorkerCall) -> i32;
/// Entry consumes the captured allocation exactly once, including its destructor.
pub type WorkerEntry = unsafe extern "C" fn(*mut core::ffi::c_void) -> i32;
/// Only used if entry never began; consumes the captured allocation exactly once.
pub type WorkerDestroy = unsafe extern "C" fn(*mut core::ffi::c_void);

/// CREATE borrows this record/name only through return. Success irrevocably
/// transfers captured state to the resident; any error retains SDK ownership.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct WorkerSpecification
{
    pub header:          Header,
    pub entry:           Option<WorkerEntry>,
    pub destroy:         Option<WorkerDestroy>,
    pub state:           *mut core::ffi::c_void,
    pub state_size:      usize,
    pub state_alignment: usize,
    pub name:            *const u8,
    pub name_length:     usize
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct WorkerCall
{
    pub header:        Header,
    pub operation:     u32,
    pub flags:         u32,
    pub worker:        u64,
    pub specification: *const WorkerSpecification,
    pub state:         u32,
    pub exit_status:   i32,
    pub reserved1:     u64
}
pub mod worker
{
    /// Native private worker trap: a0=WorkerCall*, a1=exact size, a7=TRAP;
    /// returns Status in a0. Only PARK/EXIT may use this boundary.
    pub const TRAP: u32 = 0x4d57524b;
    /// Input flags must be zero; STATUS/CURRENT report sticky cancellation.
    pub const CANCEL_REQUESTED: u32 = 1;
    pub const CREATE: u32 = 0;
    pub const START: u32 = 1;
    pub const WAKE: u32 = 2;
    pub const CANCEL: u32 = 3;
    pub const STATUS: u32 = 4;
    pub const DISPOSE: u32 = 5;
    pub const CURRENT: u32 = 6;
    pub const PARK: u32 = 7;
    pub const EXIT: u32 = 8;
    pub const PREPARED: u32 = 0;
    pub const ARMED: u32 = 1;
    pub const RUNNING: u32 = 2;
    pub const PARKED: u32 = 3;
    pub const FINISHED: u32 = 4;
}
/// Invoke exactly once, synchronously on this thread, under this genuine module's
/// Kernel account. Neither callback nor state may be retained. No deferred result
/// is permitted; return the callback's nonpositive status unchanged.
pub type WithModuleAllocation =
    unsafe extern "C" fn(u64, AllocationOperation, *mut core::ffi::c_void) -> i32;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Host
{
    pub header:                 Header,
    pub module:                 u64,
    pub reserved:               u64,
    pub context:                u64,
    pub call:                   Option<Broker>,
    pub resolve:                Option<Resolve>,
    pub allocate:               Option<Allocate>,
    pub deallocate:             Option<Deallocate>,
    pub reallocate:             Option<Reallocate>,
    pub with_module_allocation: Option<WithModuleAllocation>,
    pub workers:                Option<Workers>,
    pub critical_section:       Option<CriticalSection>
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Instance
{
    pub header:   Header,
    pub state:    u64,
    pub dispatch: Option<Dispatch>
}
impl Default for Instance
{
    fn default() -> Self
    {
        Self { header:   Header::of::<Self>(),
               state:    0,
               dispatch: None }
    }
}
/// START creates request state; POLL and CANCEL reuse its generation-tagged
/// ticket. Each invocation borrows fresh buffers only until callback return.
/// PENDING must retain no buffer pointers and do bounded work; the resident
/// scheduler owns retry timing. QUIESCE has no class or buffers.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Call
{
    pub header:          Header,
    pub ticket:          u64,
    pub phase:           u32,
    pub class:           [u8; 4],
    pub operation:       u32,
    pub input:           *const u8,
    pub input_len:       u32,
    pub output_capacity: u32,
    pub output:          *mut u8,
    pub output_len:      u32,
    pub reserved:        u32
}
pub const START: u32 = 1;
pub const POLL: u32 = 2;
pub const CANCEL: u32 = 3;
pub const QUIESCE: u32 = 4;
/// Strictly synchronous resident operations: no ticket is created, no work may
/// remain pending, and contention occurs before side effects. RESET is excluded.
/// System validates a live subject scope inside its native invocation gate and
/// keeps this bounded resident leaf on that hart through return. Such callbacks
/// cannot yield, park or call independently linked code; SMP needs a real
/// concurrent root-use/removal pin before this local gate can be widened.
pub const SYNCHRONOUS: u32 = 5;
pub const PENDING: i32 = 1;
/// Exact contract fingerprint includes boundary sizes, pointer width and the
/// full-width provider-reference signature (bit 56) request tickets (bit 57), and device/DMA
/// references (bit 58), backing-source references (bit 59), and ticket-free
/// synchronous resident calls (bit 60), and original-account allocator forwarding
/// with synchronous module allocation selection (bit 61), native workers (bit 62),
/// and native critical sections (bit 63). ABI remains unreleased v1. Generated
/// metadata and the resident reject a mismatch before entry.
pub const LAYOUT_ID: u64 = 64
                           | (96 << 8)
                           | (24 << 24)
                           | (64 << 40)
                           | (1 << 56)
                           | (1 << 57)
                           | (1 << 58)
                           | (1 << 59)
                           | (1 << 60)
                           | (1 << 61)
                           | (1 << 62)
                           | (1 << 63);
#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(core::mem::size_of::<Host>() == 96);
    assert!(core::mem::size_of::<WorkerSpecification>() == 64);
    assert!(core::mem::align_of::<WorkerSpecification>() == 8);
    assert!(core::mem::offset_of!(WorkerSpecification, state) == 24);
    assert!(core::mem::offset_of!(WorkerSpecification, name) == 48);
    assert!(core::mem::size_of::<WorkerCall>() == 48);
    assert!(core::mem::align_of::<WorkerCall>() == 8);
    assert!(core::mem::offset_of!(WorkerCall, worker) == 16);
    assert!(core::mem::offset_of!(WorkerCall, state) == 32);
    assert!(core::mem::size_of::<Instance>() == 24);
    assert!(core::mem::size_of::<Call>() == 64);
};

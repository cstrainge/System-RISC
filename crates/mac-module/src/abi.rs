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
/// Module entry receives a borrowed host descriptor, valid until reboot.
/// A successful entry initializes Instance; no service is public before commit.
pub type Entry = unsafe extern "C" fn(*const Host, *mut Instance) -> i32;
pub type Dispatch = unsafe extern "C" fn(u64, *mut Call) -> i32;
pub type Resolve = unsafe extern "C" fn(u64, u32, u32, *mut u64) -> i32;
pub type Broker = unsafe extern "C" fn(u64, u64, *mut Call) -> i32;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Host
{
    pub header:   Header,
    pub module:   u64,
    pub reserved: u64,
    pub context:  u64,
    pub call:     Option<Broker>,
    pub resolve:  Option<Resolve>
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
/// synchronous resident calls (bit 60). ABI remains unreleased v1. Generated
/// metadata and the resident reject a mismatch before entry.
pub const LAYOUT_ID: u64 = 64
                           | (48 << 8)
                           | (24 << 24)
                           | (64 << 40)
                           | (1 << 56)
                           | (1 << 57)
                           | (1 << 58)
                           | (1 << 59)
                           | (1 << 60);
#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(core::mem::size_of::<Host>() == 48);
    assert!(core::mem::size_of::<Instance>() == 24);
    assert!(core::mem::size_of::<Call>() == 64);
};

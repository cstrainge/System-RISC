//! Native wait-set transport. Sources borrow existing owner-checked identities;
//! no callback, buffer loan, or user address survives the invocation.
pub const WAIT: u32 = 0;
pub const CURRENT: u32 = 1;
pub const NOTIFY: u32 = 2;
pub const PIPE_READ: u32 = 0;
pub const PIPE_WRITE: u32 = 1;
pub const CHILD: u32 = 2;
pub const DEADLINE: u32 = 3;
pub const TIMER: u32 = 4;
pub const READY: u32 = 1;
pub const CLOSED: u32 = 2;
pub const INVALID: u32 = 4;
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Source
{
    pub kind:    u32,
    pub ready:   u32,
    pub id:      u64,
    pub minimum: u64
}
#[repr(C)]
pub struct Call
{
    pub operation: u32,
    pub notified:  u32,
    pub thread:    u64,
    pub sources:   *mut Source,
    pub count:     usize,
    pub event:     crate::events::PositionedEventCall
}
impl Default for Call
{
    fn default() -> Self
    {
        Self { operation: WAIT,
               notified:  0,
               thread:    0,
               sources:   core::ptr::null_mut(),
               count:     0,
               event:     Default::default() }
    }
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(core::mem::size_of::<Source>() == 24
                      && core::mem::align_of::<Source>() == 8
                      && core::mem::offset_of!(Source, id) == 8
                      && core::mem::offset_of!(Source, minimum) == 16
                      && core::mem::size_of::<Call>() == 120
                      && core::mem::align_of::<Call>() == 8
                      && core::mem::offset_of!(Call, thread) == 8
                      && core::mem::offset_of!(Call, sources) == 16
                      && core::mem::offset_of!(Call, count) == 24
                      && core::mem::offset_of!(Call, event) == 32);

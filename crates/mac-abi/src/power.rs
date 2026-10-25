//! Native System power requests. Drivers have a separate device-neutral contract.
pub const CAPABILITIES: u32 = 0;
pub const SHUTDOWN: u32 = 1;
pub const RESTART: u32 = 2;
pub const SHUTDOWN_AVAILABLE: u64 = 1;
pub const RESTART_AVAILABLE: u64 = 2;
pub const SELECTION_FAILURE: u32 = 1;
pub const STORAGE_FAILURE: u32 = 2;
pub const DEVICE_FAILURE: u32 = 3;
pub const APPLICATION_FAILURE: u32 = 4;
#[repr(C)]
#[derive(Default)]
pub struct PowerCall
{
    pub operation:     u32,
    pub reserved:      u32,
    pub capabilities:  u64,
    pub failure_stage: u32
}

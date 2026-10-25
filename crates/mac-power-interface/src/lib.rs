//! Device-neutral power-trigger contract for external kernel drivers.
//! A successful request means the hardware trigger was issued. An action must
//! complete in its invocation; it must not report Pending after triggering.
#![no_std]
#![forbid(unsafe_code)]
use mac_module::ClassId;
pub const CLASS: ClassId = ClassId(*b"POWR");
pub const VERSION: u32 = 1;
pub const SHUTDOWN: u64 = 1;
pub const RESTART: u64 = 2;
pub const ALL: u64 = SHUTDOWN | RESTART;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action
{
    Shutdown,
    Restart
}
impl Action
{
    pub const fn operation(self) -> u32
    {
        match self
        {
            Self::Shutdown => 1,
            Self::Restart => 2
        }
    }

    pub const fn capability(self) -> u64
    {
        match self
        {
            Self::Shutdown => SHUTDOWN,
            Self::Restart => RESTART
        }
    }

    pub const fn from_operation(value: u32) -> Option<Self>
    {
        match value
        {
            1 => Some(Self::Shutdown),
            2 => Some(Self::Restart),
            _ => None
        }
    }
}

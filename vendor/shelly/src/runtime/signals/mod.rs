#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub use crate::runtime::signals::unix::{
    InteractiveInterrupts, take_interrupt, terminate_interrupted, generation,
};

#[cfg(not(unix))]
pub struct InteractiveInterrupts;

#[cfg(not(unix))]
impl InteractiveInterrupts
{
    pub fn install() -> std::io::Result<Self> { Ok(Self) }
}

#[cfg(not(unix))]
pub fn take_interrupt() -> bool { false }

#[cfg(not(unix))]
pub fn terminate_interrupted() {}

#[cfg(not(unix))]
pub fn generation() -> u64
{
    0
}

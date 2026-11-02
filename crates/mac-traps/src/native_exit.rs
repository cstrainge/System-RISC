//! Nonreturning Process termination transport. No success value can be
//! manufactured if an accepted native exit unexpectedly returns to its caller.
use core::{convert::Infallible, fmt};

use mac_abi::{processes::*, traps::PROCESS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitKind
{
    Normal,
    Abort
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitReturned
{
    Refused(crate::Error),
    UnexpectedSuccess,
    InvalidStatus(i64)
}
impl fmt::Display for ExitReturned
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Refused(cause) => write!(out, "native process exit refused: {cause}"),
            Self::UnexpectedSuccess =>
            {
                out.write_str("native process exit returned success without consuming its author \
                               continuation")
            }
            Self::InvalidStatus(value) =>
            {
                write!(out, "native process exit returned invalid wire status {value}")
            }
        }
    }
}
impl core::error::Error for ExitReturned
{
}
impl crate::Client<'_>
{
    /// NORMAL carries the caller's main/explicit exit result. This trap itself
    /// does not run application TLS destructors. ABORT never acknowledges user
    /// cleanup callbacks. A CRT performs any normal teardown before this call.
    pub fn native_exit(self, code: i32, kind: ExitKind) -> Result<Infallible, ExitReturned>
    {
        let mut call = NativeExitCall { code,
                                        flags: match kind
                                        {
                                            ExitKind::Normal => NATIVE_EXIT_NORMAL,
                                            ExitKind::Abort => NATIVE_EXIT_ABORT
                                        },
                                        reserved: 0 };
        let returned = self.invoke(PROCESS, &mut call);
        Err(if returned == 0
            {
                ExitReturned::UnexpectedSuccess
            }
            else if let Ok(status) = i32::try_from(returned)
                      && status < 0
            {
                ExitReturned::Refused(crate::Error::from_status(status))
            }
            else
            {
                ExitReturned::InvalidStatus(returned)
            })
    }
}

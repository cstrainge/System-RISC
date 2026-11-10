//! Query actual process stream attachments; a redirected pipe is not a tty.
use mac_abi::pipes;

use crate::sys::pal::runtime;

pub trait TerminalSource
{
    fn stream(&self) -> Option<usize>;
}
macro_rules! source {
    ($ty:ty, $index:expr) => {
        impl TerminalSource for $ty
        {
            fn stream(&self) -> Option<usize>
            {
                $index
            }
        }
    };
}
source!(crate::fs::File, None);
source!(crate::io::Stdin, Some(0));
source!(crate::io::StdinLock<'_>, Some(0));
source!(crate::io::Stdout, Some(1));
source!(crate::io::StdoutLock<'_>, Some(1));
source!(crate::io::Stderr, Some(2));
source!(crate::io::StderrLock<'_>, Some(2));

pub fn is_terminal<T: TerminalSource>(stream: &T) -> bool
{
    let Some(index) = stream.stream()
    else
    {
        return false;
    };
    let handle = runtime::stream(index);
    if handle == 0
    {
        return false;
    }
    let mut call = pipes::Call { operation: pipes::TERMINAL_QUERY,
                                 handle,
                                 ..Default::default() };
    runtime::client().pipes(&mut call).is_ok()
}

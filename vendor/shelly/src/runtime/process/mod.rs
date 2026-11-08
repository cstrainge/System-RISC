
#[cfg(not(unix))]
use std::{ io, process::Command };

#[cfg(not(unix))]
use crate::language::data::value::Value;

#[cfg(unix)]
mod foreground;
#[cfg(unix)]
pub(crate) use crate::runtime::process::foreground::Foreground;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use crate::runtime::process::unix::{ Terminal, invoke };

#[cfg(not(unix))]
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Terminal;

#[cfg(not(unix))]
pub fn invoke(_: &str, _: &[Value], _: &[(String, String)], _: impl Fn(&str) -> String,
              _: impl FnMut(&mut Command) -> io::Result<()>) -> Result<Value, String>
{
    Err("Process and terminal APIs currently require Unix".to_string())
}

pub const COMMANDS: &[&str] = &[
        "run_process", "open_terminal", "terminal_write", "terminal_read", "terminal_close",
    ];

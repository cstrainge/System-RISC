//! List directory contents, with sixel icons on the Terminal.
//!
//! Mono icons are the default; `--color` selects color icons. Icons are
//! only drawn when stdout is a terminal: piped output is plain text, one
//! entry per line.

mod args;
mod format;
mod icons;
mod listing;
mod native_fs;
mod sixel;

use std::{env::args_os,
          io::{self, IsTerminal, Write},
          process::ExitCode};

use icons::BuiltinIconProvider;

fn main() -> ExitCode
{
    let raw: Vec<_> = args_os().skip(1).collect();
    let opts = match args::parse(&raw)
    {
        args::Parse::Run(opts) => opts,
        args::Parse::Help =>
        {
            print!("{}", args::usage());
            return ExitCode::from(0);
        }
        args::Parse::Invalid(message) =>
        {
            // The message already carries the "ls: " prefix.
            eprintln!("{message}");
            eprint!("{}", args::usage());
            return ExitCode::from(2);
        }
    };
    let stdout = io::stdout();
    let tty = stdout.is_terminal();
    let provider = BuiltinIconProvider;
    let mut out = stdout.lock();
    let mut err = io::stderr().lock();
    let code = listing::run(&opts, &provider, &mut out, &mut err, tty);
    let _ = out.flush();
    let _ = err.flush();
    ExitCode::from(code as u8)
}

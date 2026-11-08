//! Write command-line arguments to standard output.
use std::{env::args_os,
          io::{self, Write}};

fn main() -> io::Result<()>
{
    let mut args = args_os().skip(1).peekable();
    let newline = args.peek().is_none_or(|arg| arg != "-n");
    if !newline
    {
        args.next();
    }

    let mut output = io::stdout().lock();
    if let Some(first) = args.next()
    {
        output.write_all(first.as_encoded_bytes())?;
        for arg in args
        {
            output.write_all(b" ")?;
            output.write_all(arg.as_encoded_bytes())?;
        }
    }
    if newline
    {
        output.write_all(b"\n")?;
    }
    output.flush()
}

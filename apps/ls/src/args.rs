//! Command-line parsing for `ls`. Hand-rolled: no argument-parsing crate,
//! so this stays a plain std program. (Not to be confused with CLAP, the
//! application format: the name collision is why nothing here is called
//! `clap`.)

use std::ffi::OsString;

/// Sort order for entries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sort
{
    /// Byte order of the file name.
    #[default]
    Name,
    /// Modification time, newest first.
    Time,
    /// Size, largest first.
    Size
}

/// Parsed options. Icon mode is mono by default; `--color` selects color
/// icons. Icons are on by default and only ever drawn on a terminal.
#[derive(Clone, Debug)]
pub struct Options
{
    pub all:            bool,
    pub long:           bool,
    pub human_readable: bool,
    pub recursive:      bool,
    pub reverse:        bool,
    pub sort:           Sort,
    pub one_per_line:   bool,
    pub directory:      bool,
    pub classify:       bool,
    pub color:          bool,
    pub icons:          bool,
    pub paths:          Vec<OsString>
}

impl Default for Options
{
    fn default() -> Self
    {
        Self { all:            false,
               long:           false,
               human_readable: false,
               recursive:      false,
               reverse:        false,
               sort:           Sort::Name,
               one_per_line:   false,
               directory:      false,
               classify:       false,
               color:          false,
               icons:          true,
               paths:          Vec::new() }
    }
}

/// Result of parsing.
pub enum Parse
{
    Run(Options),
    /// `--help` was given: print the usage text to stdout and exit 0.
    Help,
    /// Bad usage: print the message to stderr, then the usage, exit 2.
    Invalid(String)
}

pub fn usage() -> &'static str
{
    "usage: ls [-aFlRrSt1dh] [--all] [--long] [--human-readable] [--recursive]\n       [--reverse] \
     [--directory] [--classify] [--color] [--mono]\n       [--icons] [--no-icons] [--help] [path \
     ...]\n"
}

pub fn parse(args: &[OsString]) -> Parse
{
    let mut opts = Options::default();
    let mut only_paths = false;
    for arg in args
    {
        let bytes = arg.as_encoded_bytes();
        if only_paths
        {
            opts.paths.push(arg.clone());
            continue;
        }
        if bytes == b"--"
        {
            only_paths = true;
            continue;
        }
        if bytes.len() > 2 && bytes.starts_with(b"--")
        {
            match parse_long(&mut opts, &bytes[2..])
            {
                Ok(true) => return Parse::Help,
                Ok(false) => (),
                Err(message) => return Parse::Invalid(message)
            }
            continue;
        }
        if bytes.len() > 1 && bytes[0] == b'-'
        {
            if let Err(message) = parse_short(&mut opts, &bytes[1..])
            {
                return Parse::Invalid(message);
            }
            continue;
        }
        opts.paths.push(arg.clone());
    }
    Parse::Run(opts)
}

/// Parse one `--long` flag. Returns true for `--help`.
fn parse_long(opts: &mut Options, name: &[u8]) -> Result<bool, String>
{
    match name
    {
        b"all" => opts.all = true,
        b"long" => opts.long = true,
        b"human-readable" => opts.human_readable = true,
        b"recursive" => opts.recursive = true,
        b"reverse" => opts.reverse = true,
        b"directory" => opts.directory = true,
        b"classify" => opts.classify = true,
        b"color" => opts.color = true,
        b"mono" => opts.color = false,
        b"icons" => opts.icons = true,
        b"no-icons" => opts.icons = false,
        b"help" => return Ok(true),
        _ => return Err(format!("ls: unrecognized option '--{}'", String::from_utf8_lossy(name)))
    }
    Ok(false)
}

/// Parse one cluster of short flags, as in `-la`.
fn parse_short(opts: &mut Options, flags: &[u8]) -> Result<(), String>
{
    for &flag in flags
    {
        match flag
        {
            b'a' => opts.all = true,
            b'l' => opts.long = true,
            b'h' => opts.human_readable = true,
            b'R' => opts.recursive = true,
            b'r' => opts.reverse = true,
            b't' => opts.sort = Sort::Time,
            b'S' => opts.sort = Sort::Size,
            b'1' => opts.one_per_line = true,
            b'd' => opts.directory = true,
            b'F' => opts.classify = true,
            _ => return Err(format!("ls: illegal option -- {}", flag as char))
        }
    }
    Ok(())
}

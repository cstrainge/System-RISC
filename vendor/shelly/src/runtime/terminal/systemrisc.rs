use crossterm::{cursor, event::Event, terminal};
use std::sync::OnceLock;

static STARTUP_SIZE: OnceLock<Option<(u16, u16)>> = OnceLock::new();

fn startup_size() -> Option<(u16, u16)>
{
    *STARTUP_SIZE.get_or_init(|| terminal::size().ok())
}

pub(crate) fn attached() -> bool
{
    // CLAP streams are pipes. A reply on stdin proves this session is attached
    // to a terminal frontend that interprets our stdout.
    startup_size().is_some()
}

#[derive(Default)]
pub struct TerminalCapabilities
{
    pub sixel:       bool,
    pub iterm2:      bool,
    pub cell_size:   Option<(u16, u16)>,
    pub screen_size: Option<(u16, u16)>,
    pub input:       Vec<Event>,
}

pub(crate) fn term_size() -> Option<(u16, u16)>
{
    let (columns, rows) = terminal::size().ok()?;
    Some((rows, columns))
}

pub(crate) fn cursor_position(_input: &mut Vec<Event>) -> Option<(u16, u16)>
{
    // Crossterm leaves key events queued and returns zero-based coordinates.
    let (column, row) = cursor::position().ok()?;
    Some((row.checked_add(1)?, column.checked_add(1)?))
}

pub fn detect_capabilities() -> TerminalCapabilities
{
    let screen_size = startup_size();
    TerminalCapabilities { screen_size,
                           ..TerminalCapabilities::default() }
}

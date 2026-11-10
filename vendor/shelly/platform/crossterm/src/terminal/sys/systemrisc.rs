//! Terminal mode and size negotiation for a System RISC CLAP session.
use crate::{
    event::{filter::Filter, poll_internal, read_internal, InternalEvent},
    terminal::WindowSize,
};
use std::{
    io::{self, Write},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

static RAW: AtomicBool = AtomicBool::new(false);

pub(crate) fn is_raw_mode_enabled() -> bool {
    RAW.load(Ordering::Acquire)
}

fn raw(value: bool) -> io::Result<()> {
    if is_raw_mode_enabled() == value {
        return Ok(());
    }
    let mut out = io::stdout().lock();
    let command = if value {
        b"\x1b]777;systemrisc;raw;1\x1b\\"
    } else {
        b"\x1b]777;systemrisc;raw;0\x1b\\"
    };
    out.write_all(command)?;
    out.flush()?;
    RAW.store(value, Ordering::Release);
    Ok(())
}

pub(crate) fn enable_raw_mode() -> io::Result<()> {
    raw(true)
}
pub(crate) fn disable_raw_mode() -> io::Result<()> {
    raw(false)
}

struct SizeFilter;
impl Filter for SizeFilter {
    fn eval(&self, event: &InternalEvent) -> bool {
        matches!(event, InternalEvent::WindowSize(..))
    }
}

pub(crate) fn window_size() -> io::Result<WindowSize> {
    {
        let mut out = io::stdout().lock();
        out.write_all(b"\x1b[18t")?;
        out.flush()?;
    }
    if poll_internal(Some(Duration::from_secs(2)), &SizeFilter)? {
        if let InternalEvent::WindowSize(columns, rows) = read_internal(&SizeFilter)? {
            return Ok(WindowSize {
                columns,
                rows,
                width: 0,
                height: 0,
            });
        }
    }
    Err(io::ErrorKind::TimedOut.into())
}

struct SixelFilter;
impl Filter for SixelFilter {
    fn eval(&self, event: &InternalEvent) -> bool {
        matches!(event, InternalEvent::SixelSupport(..))
    }
}

struct CellFilter;
impl Filter for CellFilter {
    fn eval(&self, event: &InternalEvent) -> bool {
        matches!(event, InternalEvent::CellSize(..))
    }
}

fn ask<F: Filter>(query: &[u8], filter: &F) -> io::Result<Option<InternalEvent>> {
    {
        let mut out = io::stdout().lock();
        out.write_all(query)?;
        out.flush()?;
    }
    if poll_internal(Some(Duration::from_secs(2)), filter)? {
        return read_internal(filter).map(Some);
    }
    Ok(None)
}

/// Ask the frontend whether it draws sixel graphics and, if it does, for its
/// character cell in pixels (`width`, `height`). No reply means no support.
pub fn sixel_support() -> io::Result<(bool, Option<(u16, u16)>)> {
    let Some(InternalEvent::SixelSupport(true)) = ask(b"\x1b[c", &SixelFilter)? else {
        return Ok((false, None));
    };
    let cell = match ask(b"\x1b[16t", &CellFilter)? {
        Some(InternalEvent::CellSize(width, height)) => Some((width, height)),
        _ => None,
    };
    Ok((true, cell))
}

pub(crate) fn size() -> io::Result<(u16, u16)> {
    let size = window_size()?;
    Ok((size.columns, size.rows))
}

pub fn supports_keyboard_enhancement() -> io::Result<bool> {
    Ok(false)
}

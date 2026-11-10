//! Terminal mode and size negotiation for a System RISC CLAP session.
use std::{io::{self, Write},
          time::Duration};

use crate::{event::{InternalEvent, filter::Filter, poll_internal, read_internal},
            terminal::WindowSize};

static SAVED_MODE: std::sync::Mutex<Option<u32>> = std::sync::Mutex::new(None);

fn terminal(operation: u32, flags: u32) -> io::Result<mac_toolbox::abi::pipes::TerminalInfo>
{
    use mac_toolbox::{System,
                      abi::{pipes, processes}};
    let error = |e: mac_toolbox::Error| io::Error::from_raw_os_error(e.status());
    let system = System::new().map_err(error)?;
    let mut streams = processes::ProcessCall { operation: processes::STANDARD_STREAMS,
                                               ..Default::default() };
    system.client().process(&mut streams).map_err(error)?;
    let mut call = pipes::Call { operation,
                                 handle: streams.streams[0],
                                 terminal: pipes::TerminalInfo { flags,
                                                                 ..Default::default() },
                                 ..Default::default() };
    system.client().pipes(&mut call).map_err(error)?;
    Ok(call.terminal)
}

pub(crate) fn is_raw_mode_enabled() -> bool
{
    use mac_toolbox::abi::pipes;
    terminal(pipes::TERMINAL_QUERY, 0).is_ok_and(|info| info.flags & pipes::TERMINAL_RAW != 0)
}

pub(crate) fn enable_raw_mode() -> io::Result<()>
{
    use mac_toolbox::abi::pipes;
    let mut saved = SAVED_MODE.lock().map_err(|_| io::ErrorKind::Other)?;
    if saved.is_some()
    {
        return Ok(());
    }
    let old = terminal(pipes::TERMINAL_QUERY, 0)?.flags;
    terminal(pipes::TERMINAL_MODE, pipes::TERMINAL_RAW)?;
    *saved = Some(old);
    Ok(())
}
pub(crate) fn disable_raw_mode() -> io::Result<()>
{
    use mac_toolbox::abi::pipes;
    let mut saved = SAVED_MODE.lock().map_err(|_| io::ErrorKind::Other)?;
    if let Some(flags) = *saved
    {
        terminal(pipes::TERMINAL_MODE, flags)?;
        *saved = None;
    }
    Ok(())
}

pub(crate) fn window_size() -> io::Result<WindowSize>
{
    let info = terminal(mac_toolbox::abi::pipes::TERMINAL_QUERY, 0)?;
    Ok(WindowSize { columns: info.columns,
                    rows:    info.rows,
                    width:   info.width,
                    height:  info.height })
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

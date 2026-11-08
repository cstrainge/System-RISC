//! Read VT events from the process's attached stdin pipe.
use std::{
    collections::VecDeque,
    io,
    time::{Duration, Instant},
};

use mac_toolbox::{
    abi::{pipes, processes},
    Error, System,
};

use crate::event::{source::EventSource, sys::native_parse::parse_event, InternalEvent};

pub(crate) struct NativeSource {
    handle: u64,
    input: VecDeque<u8>,
    sequence: Vec<u8>,
    escape_since: Option<Instant>,
}

fn native_error(cause: Error) -> io::Error {
    io::Error::from_raw_os_error(cause.status())
}

impl NativeSource {
    pub(crate) fn new() -> io::Result<Self> {
        let system = System::new().map_err(native_error)?;
        let mut call = processes::ProcessCall {
            operation: processes::STANDARD_STREAMS,
            ..Default::default()
        };
        system.client().process(&mut call).map_err(native_error)?;
        let handle = call.streams[0];
        if handle == 0 {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "Terminal stdin is required",
            ));
        }
        Ok(Self {
            handle,
            input: VecDeque::new(),
            sequence: Vec::new(),
            escape_since: None,
        })
    }

    fn refill(&mut self, system: &System<'_>, timeout: Option<Duration>) -> io::Result<()> {
        let mut bytes = [0; 128];
        let mut call = pipes::Call {
            operation: pipes::READ_WAIT,
            wait_nanoseconds: match timeout {
                Some(value) => u64::try_from(value.as_nanos())
                    .ok()
                    .filter(|value| *value != u64::MAX)
                    .ok_or(io::ErrorKind::InvalidInput)?,
                None => u64::MAX,
            },
            handle: self.handle,
            buffer: bytes.as_mut_ptr(),
            length: bytes.len(),
            ..Default::default()
        };
        let result = system.client().pipes(&mut call);
        match result {
            Ok(()) if call.transferred != 0 => {
                self.input.extend(
                    bytes
                        .get(..call.transferred)
                        .ok_or(io::ErrorKind::InvalidData)?
                        .iter()
                        .copied(),
                );
                Ok(())
            }
            Ok(()) => Err(io::ErrorKind::UnexpectedEof.into()),
            Err(Error::WouldBlock) => Ok(()),
            Err(cause) => Err(native_error(cause)),
        }
    }
}

impl EventSource for NativeSource {
    fn try_read(&mut self, timeout: Option<Duration>) -> io::Result<Option<InternalEvent>> {
        let system = System::new().map_err(native_error)?;
        let started = Instant::now();
        let mut attempted = false;
        loop {
            if self.input.is_empty() {
                let remaining = timeout.map(|limit| limit.saturating_sub(started.elapsed()));
                if attempted && remaining.is_some_and(|left| left.is_zero()) {
                    return Ok(None);
                }
                let escape_remaining = if self.sequence == b"\x1b" {
                    self.escape_since
                        .map(|at| Duration::from_millis(20).saturating_sub(at.elapsed()))
                } else {
                    None
                };
                let wait = match (remaining, escape_remaining) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
                self.refill(&system, wait)?;
                attempted = true;
            }
            while let Some(byte) = self.input.pop_front() {
                self.sequence.push(byte);
                if self.sequence == b"\x1b" {
                    self.escape_since = Some(Instant::now());
                }
                match parse_event(&self.sequence, true) {
                    Ok(Some(event)) => {
                        self.sequence.clear();
                        self.escape_since = None;
                        return Ok(Some(event));
                    }
                    Ok(None) if self.sequence.len() < 16_384 => {}
                    _ => {
                        self.sequence.clear();
                        self.escape_since = None;
                    }
                }
            }
            if self.sequence == b"\x1b"
                && self
                    .escape_since
                    .is_some_and(|at| at.elapsed() >= Duration::from_millis(20))
            {
                let event = parse_event(&self.sequence, false)?;
                self.sequence.clear();
                self.escape_since = None;
                return Ok(event);
            }
        }
    }
}

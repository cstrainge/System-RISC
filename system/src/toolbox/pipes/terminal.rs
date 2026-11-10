//! Resident input discipline. The associated pipes remain the identities and
//! lifetime owners; this object owns only editing, mode and geometry state.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_abi::{BAD_ARGUMENT, NO_MEMORY, Status, WOULD_BLOCK, pipes as wire};

use super::ring::Ring;

struct Mode
{
    owner: u64,
    flags: u32
}

pub(super) struct State
{
    pub info:          wire::TerminalInfo,
    pub output:        u64,
    pub echo_endpoint: u64,
    modes:             Vec<Mode>,
    draft:             Vec<u8>,
    /// Some(offset) is one committed line, or one empty-line EOF indication.
    /// It is consumed independently of the transport ring's capacity.
    committed:         Option<usize>
}
impl State
{
    pub fn new(mut info: wire::TerminalInfo, output: u64) -> Result<Self, Status>
    {
        Self::validate_size(info)?;
        info.flags = wire::TERMINAL_DEFAULT;
        info.revision = 1;
        Ok(Self { info,
                  output,
                  echo_endpoint: 0,
                  modes: Vec::new(),
                  draft: Vec::new(),
                  committed: None })
    }

    pub fn validate_size(info: wire::TerminalInfo) -> Result<(), Status>
    {
        if info.columns == 0 || info.rows == 0 || info.reserved != 0
        {
            Err(BAD_ARGUMENT)
        }
        else
        {
            Ok(())
        }
    }

    pub fn set_mode(&mut self, flags: u32, owner: u64) -> Result<(), Status>
    {
        if flags & !wire::TERMINAL_FLAGS != 0
        {
            return Err(BAD_ARGUMENT);
        }
        if self.modes.last().is_some_and(|mode| mode.owner == owner && mode.flags == flags)
        {
            return Ok(());
        }
        let existing = self.modes.iter().position(|mode| mode.owner == owner);
        if existing.is_none()
        {
            self.modes.try_reserve(1).map_err(|_| NO_MEMORY)?;
        }
        if let Some(at) = existing
        {
            self.modes.remove(at);
        }
        self.modes.push(Mode { owner,
                               flags });
        self.apply_mode(flags);
        Ok(())
    }

    fn apply_mode(&mut self, flags: u32)
    {
        if self.info.flags == flags
        {
            return;
        }
        // Previously accepted cooked typeahead precedes new raw bytes.
        if flags & wire::TERMINAL_RAW != 0 && !self.draft.is_empty() && self.committed.is_none()
        {
            self.committed = Some(0);
        }
        self.info.flags = flags;
        self.changed();
    }

    pub fn reset(&mut self, owner: u64)
    {
        self.modes.retain(|mode| mode.owner != owner);
        let flags = self.modes.last().map_or(wire::TERMINAL_DEFAULT, |mode| mode.flags);
        self.apply_mode(flags);
    }

    pub fn resize(&mut self, info: wire::TerminalInfo) -> Result<(), Status>
    {
        Self::validate_size(info)?;
        if (self.info.columns, self.info.rows, self.info.width, self.info.height)
           != (info.columns, info.rows, info.width, info.height)
        {
            self.info.columns = info.columns;
            self.info.rows = info.rows;
            self.info.width = info.width;
            self.info.height = info.height;
            self.changed();
        }
        Ok(())
    }

    fn changed(&mut self)
    {
        // A wrapping revision is a change token, never an object identity.
        self.info.revision = self.info.revision.wrapping_add(1);
    }

    pub fn cancel_input(&mut self)
    {
        self.draft.clear();
        self.committed = None;
    }

    pub fn readable(&self, hung_up: bool) -> bool
    {
        self.committed.is_some() || (hung_up && !self.draft.is_empty())
    }

    pub fn writable(&self, ring: &Ring, echo_space: usize, minimum: usize) -> bool
    {
        self.committed.is_none()
        && (self.info.flags & wire::TERMINAL_RAW == 0 || ring.space() >= minimum)
        && (self.info.flags & wire::TERMINAL_ECHO == 0 || echo_space >= minimum.max(3))
    }

    pub fn read(&mut self, output: &mut [u8], hung_up: bool) -> Option<usize>
    {
        if hung_up && !self.draft.is_empty() && self.committed.is_none()
        {
            self.committed = Some(0);
        }
        let offset = self.committed?;
        let count = output.len().min(self.draft.len() - offset);
        output[..count].copy_from_slice(&self.draft[offset..offset + count]);
        if offset + count == self.draft.len()
        {
            self.committed = None;
            self.draft.clear();
        }
        else
        {
            self.committed = Some(offset + count);
        }
        Some(count)
    }

    /// Consume one byte only after storage and echo capacity are preflighted.
    /// The manager copies the returned echo before exposing the next byte.
    pub fn input(&mut self, byte: u8, ring: &mut Ring, echo_space: usize)
                 -> Result<([u8; 3], usize), Status>
    {
        if self.committed.is_some()
        {
            return Err(WOULD_BLOCK);
        }
        let echoing = self.info.flags & wire::TERMINAL_ECHO != 0;
        let raw = self.info.flags & wire::TERMINAL_RAW != 0;
        let erase = !raw && matches!(byte, 8 | 127);
        let newline = !raw && matches!(byte, b'\r' | b'\n');
        let eof = !raw && byte == 4;
        let echo = if !echoing || eof || (erase && self.draft.is_empty())
        {
            ([0; 3], 0)
        }
        else if erase
        {
            ([8, b' ', 8], 3)
        }
        else if newline
        {
            ([b'\r', b'\n', 0], 2)
        }
        else
        {
            ([byte, 0, 0], 1)
        };
        if echo_space < echo.1
        {
            return Err(WOULD_BLOCK);
        }
        if raw
        {
            if ring.space() == 0
            {
                return Err(WOULD_BLOCK);
            }
            ring.write_from(&[byte]);
        }
        else if erase
        {
            // Erase one complete UTF-8 scalar; a partial sequence is removed
            // as a unit as well. Grapheme-aware editing is a separate policy.
            if let Some(mut last) = self.draft.pop()
            {
                while last & 0xc0 == 0x80
                {
                    let Some(previous) = self.draft.pop()
                    else
                    {
                        break;
                    };
                    last = previous;
                }
            }
        }
        else if eof
        {
            self.committed = Some(0);
        }
        else
        {
            if self.draft.len() >= wire::TERMINAL_LINE_BYTES - usize::from(!newline)
            {
                return Err(NO_MEMORY);
            }
            self.draft.try_reserve(1).map_err(|_| NO_MEMORY)?;
            self.draft.push(if newline { b'\n' } else { byte });
            if newline
            {
                self.committed = Some(0);
            }
        }
        Ok(echo)
    }
}

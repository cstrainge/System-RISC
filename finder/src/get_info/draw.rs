//! Resource-based original Finder layout, using only native Toolbox clients.
use mac_toolbox::{controls::StandardState,
                  quickdraw::{fonts::FontSelection, graphics::GraphicsPort}};

use super::*;
pub(super) fn text<'b>(system: &System<'_>, index: u32, output: &'b mut [u8])
                       -> Result<&'b str, Error>
{
    system.resources().application().indexed_string(2003, index, output)
}
fn substitute<const N: usize>(source: &str, values: &[&str]) -> Result<Text<N>, Error>
{
    let mut result = Text::default();
    let mut remaining = source;
    while !remaining.is_empty()
    {
        if let Some(after) = remaining.strip_prefix('^')
        {
            let digit = after.as_bytes().first().copied().ok_or(Error::BadArgument)?;
            let value = values.get(usize::from(digit.checked_sub(b'0')
                                                    .ok_or(Error::BadArgument)?))
                              .ok_or(Error::BadArgument)?;
            result.write_str(value).map_err(|_| Error::NoMemory)?;
            remaining = &after[1..];
        }
        else
        {
            let c = remaining.chars().next().ok_or(Error::BadArgument)?;
            result.write_char(c).map_err(|_| Error::NoMemory)?;
            remaining = &remaining[c.len_utf8()..];
        }
    }
    Ok(result)
}
pub(super) fn filesystem(system: &System<'_>, volume: &files::VolumeInfo)
                         -> Result<Text<256>, Error>
{
    let filesystem = volume.filesystem;
    if let Some(version) = filesystem.version()
    {
        let mut bytes = [0; 128];
        let mut number = Text::<16>::default();
        write!(&mut number, "{version}").map_err(|_| Error::NoMemory)?;
        substitute(text(system, 46, &mut bytes)?, &[filesystem.name(), number.as_str()])
    }
    else
    {
        let mut result = Text::default();
        result.write_str(filesystem.name()).map_err(|_| Error::NoMemory)?;
        Ok(result)
    }
}
pub(super) fn volume_location(system: &System<'_>, volume: &files::VolumeInfo)
                              -> Result<Text<768>, Error>
{
    let mut bytes = [0; 128];
    let mut number = Text::<16>::default();
    write!(&mut number, "{}", volume.disk).map_err(|_| Error::NoMemory)?;
    substitute(text(system, 47, &mut bytes)?, &[number.as_str()])
}
pub(super) fn title(system: &System<'_>, name: &str) -> Result<Text<768>, Error>
{
    let mut bytes = [0; 512];
    let source = system.resources().application().string(3091, &mut bytes)?;
    substitute(source, &[name])
}
fn date(system: &System<'_>, when: Timestamp) -> Result<Text<128>, Error>
{
    let date = format::calendar(when);
    let mut day = [0; 32];
    let mut month = [0; 32];
    let mut period = [0; 16];
    let day = text(system, 15 + u32::from(date.weekday), &mut day)?;
    let month = text(system, 22 + u32::from(date.month - 1), &mut month)?;
    let period = text(system, if date.hour >= 12 { 35 } else { 34 }, &mut period)?;
    let mut output = Text::default();
    let hour = if date.hour.is_multiple_of(12) { 12 } else { date.hour % 12 };
    write!(&mut output,
           "{}, {} {}, {}, {}:{:02} {}",
           day, month, date.day, date.year, hour, date.minute, period).map_err(|_| {
                                                                          Error::NoMemory
                                                                      })?;
    Ok(output)
}
fn body(system: &System<'_>, port: GraphicsPort<'_>, bounds: Rect, value: &str, color: u32)
        -> Result<(), Error>
{
    if bounds == Rect::ZERO
    {
        return Ok(());
    }
    let width = bounds.right - bounds.left;
    let mut remaining = value;
    let mut y = bounds.top + 10;
    while !remaining.is_empty() && y <= bounds.bottom
    {
        let line_end = remaining.find(['\r', '\n']).unwrap_or(remaining.len());
        let candidate = &remaining[..line_end];
        let mut newline = line_end < remaining.len();
        let mut end = line_end;
        if system.quickdraw().fonts().text_width(FONT, candidate)? > width
        {
            newline = false;
            end = 0;
            for (index, _) in candidate.match_indices(' ')
            {
                if system.quickdraw().fonts().text_width(FONT, &candidate[..index])? > width
                {
                    break;
                }
                end = index;
            }
            if end == 0
            {
                for (index, c) in candidate.char_indices()
                {
                    let next = index + c.len_utf8();
                    if system.quickdraw().fonts().text_width(FONT, &candidate[..next])? > width
                    {
                        break;
                    }
                    end = next;
                }
            }
        }
        if end == 0 && !newline
        {
            break;
        }
        let line = &remaining[..end];
        port.text(FONT,
                  Point { h: bounds.left,
                          v: y },
                  line,
                  color)?;
        remaining = &remaining[end..];
        if newline
        {
            remaining = remaining.strip_prefix('\r')
                                 .or_else(|| remaining.strip_prefix('\n'))
                                 .unwrap_or(remaining);
        }
        else
        {
            remaining = remaining.trim_start_matches(' ');
        }
        y += 12;
    }
    Ok(())
}
impl Info<'_>
{
    pub(super) fn control(&self, system: &System<'_>, check: Check, highlighted: bool)
                          -> Result<StandardState, Error>
    {
        let (bounds, index, value, enabled) = match check
        {
            Check::Locked => (self.layout.locked, 7, self.entry.is_locked(), !self.read_only),
            Check::Stationery =>
            {
                (self.layout.stationery,
                 8,
                 self.entry.is_stationery(),
                 !self.read_only && !self.entry.is_locked())
            }
        };
        let mut bytes = [0; 128];
        let label = text(system, index, &mut bytes)?;
        let mut state = StandardState::new(bounds, label);
        state.font = FontSelection { face: 1,
                                     ..FONT };
        state.enabled = u32::from(enabled);
        state.value = i16::from(value);
        state.highlighted = if highlighted { 11 } else { 0 };
        Ok(state)
    }

    fn size_text(&self, system: &System<'_>) -> Result<Text<256>, Error>
    {
        let mut bytes = [0; 512];
        if let Some(volume) = self.volume_info
        {
            let total = crate::browser::disk_size_text(volume.total_bytes);
            let free = crate::browser::disk_size_text(volume.free_bytes);
            return substitute(text(system, 45, &mut bytes)?, &[total.as_str(), free.as_str()]);
        }
        if let Some(status) = self.job.as_ref().and_then(|job| job.error)
        {
            let mut number = Text::<32>::default();
            write!(&mut number, "{}", status.status()).map_err(|_| Error::NoMemory)?;
            return substitute(text(system, 37, &mut bytes)?, &[number.as_str()]);
        }
        if self.job.as_ref().is_some_and(|job| !job.complete)
        {
            let mut result = Text::default();
            result.write_str(text(system, 9, &mut bytes)?).map_err(|_| Error::NoMemory)?;
            return Ok(result);
        }
        let mut disk = Text::<40>::default();
        if self.allocated == 0
        {
            disk.write_str(text(system, 12, &mut bytes)?).map_err(|_| Error::NoMemory)?;
        }
        else
        {
            disk.write_str(crate::browser::disk_size_text(self.allocated).as_str())
                .map_err(|_| Error::NoMemory)?;
        }
        let used = format::grouped(self.logical);
        let count = format::grouped(self.job.as_ref().map_or(0, |job| job.items));
        let source =
            text(system,
                 if self.entry.is_directory()
                 {
                     if self.job.as_ref().is_some_and(|job| job.items == 1) { 38 } else { 14 }
                 }
                 else
                 {
                     13
                 },
                 &mut bytes)?;
        substitute(source, &[disk.as_str(), used.as_str(), count.as_str()])
    }

    pub(super) fn draw(&self, system: &System<'_>) -> Result<(), Error>
    {
        system.begin_update(self.window)?;
        let result: Result<(), Error> = (|| {
            let port = system.quickdraw().graphics(self.window);
            port.reset()?;
            port.colors(0xffffff, 0xffffff)?;
            port.paint_rect(Rect::new(0, 0, self.layout.width, self.layout.height))?;
            port.colors(0, 0xffffff)?;
            for (index, bounds) in [(0, self.layout.location),
                                    (1, self.layout.kind),
                                    (2, self.layout.size),
                                    (3, self.layout.created),
                                    (4, self.layout.modified),
                                    (5, self.layout.version),
                                    (6, self.layout.comments)]
            {
                if bounds == Rect::ZERO
                {
                    continue;
                }
                let mut bytes = [0; 128];
                let label = text(system, index, &mut bytes)?;
                let font = FontSelection { face: 1,
                                           ..FONT };
                let width = system.quickdraw().fonts().text_width(font, label)?;
                port.text(font,
                          Point { h: 69 - width,
                                  v: if index == 6 { bounds.top - 4 } else { bounds.top + 10 } },
                          label,
                          0)?;
            }
            let bounds = self.layout.icon;
            let icon_bounds =
                Rect::new(bounds.left + 3, bounds.top + 3, bounds.left + 35, bounds.top + 35);
            crate::icon_view::Drawing::icon(&mut crate::browser::Port { system,
                                                                        graphics: port },
                                            self.icon,
                                            icon_bounds,
                                            crate::icon_view::State::default())?;
            system.textedit().draw(self.name.id(), self.window)?;
            if self.focus == Focus::Name && self.active && self.editable(Focus::Name)
            {
                port.frame_rect(layout::inset(self.layout.name, -1))?;
            }
            body(system, port, self.layout.kind, self.kind.as_str(), 0)?;
            body(system, port, self.layout.size, self.size_text(system)?.as_str(), 0)?;
            body(system, port, self.layout.location, self.location.as_str(), 0)?;
            body(system,
                 port,
                 self.layout.created,
                 date(system, self.entry.created_at())?.as_str(),
                 0)?;
            body(system,
                 port,
                 self.layout.modified,
                 date(system, self.entry.modified_at())?.as_str(),
                 0)?;
            body(system, port, self.layout.version, self.version.as_str(), 0)?;
            if let Some(status) = self.status.or(self.metadata_error)
            {
                let mut bytes = [0; 256];
                let mut number = Text::<32>::default();
                write!(&mut number, "{}", status.status()).map_err(|_| Error::NoMemory)?;
                let index = if self.status.is_none()
                               || self.job.as_ref().is_some_and(|job| job.error == Some(status))
                {
                    37
                }
                else
                {
                    match status
                    {
                        Error::AlreadyExists => 39,
                        Error::BadArgument => 40,
                        Error::ReadOnly => 41,
                        Error::AccessDenied => 42,
                        Error::DiskFull => 43,
                        Error::Io | Error::RollbackFailed => 44,
                        _ => 11
                    }
                };
                let source = text(system, index, &mut bytes)?;
                let message = substitute::<256>(source, &[number.as_str()])?;
                body(system, port, self.layout.description, message.as_str(), 0xa00000)?;
            }
            else
            {
                body(system, port, self.layout.description, self.description.as_str(), 0)?;
            }
            if !self.editable(Focus::Comments)
            {
                port.pen_pattern([0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55])?;
                port.frame_rect(self.layout.comments)?;
                port.pen_pattern([0xff; 8])?;
            }
            else
            {
                port.frame_rect(self.layout.comments)?;
            }
            system.textedit().draw(self.comments.id(), self.window)?;
            for (index, check) in [Check::Locked, Check::Stationery].into_iter().enumerate()
            {
                if let Some(id) = self.controls[index]
                {
                    let control = system.controls().control(id);
                    control.update(controls::StandardKind::WindowFontCheckbox,
                                   self.control(system, check, false)?)?;
                }
            }
            Ok(())
        })();
        let end = system.end_update(self.window);
        result?;
        end
    }
}

pub(super) fn location(system: &System<'_>, volume_id: VolumeId, parent: Cnid)
                       -> Result<Text<768>, Error>
{
    let mut output = Text::default();
    let volume = system.files().volume(volume_id)?;
    write!(&mut output, "{} :", volume.name()).map_err(|_| Error::NoMemory)?;
    let mut path = Text::<512>::default();
    system.files().path(volume_id, parent, &mut path)?;
    // File Manager returns a native slash path. Its catalog path remains
    // authoritative; Finder contributes the classic presentation separators.
    for part in path.as_str().split('/').filter(|part| !part.is_empty())
    {
        write!(&mut output, " {} :", part).map_err(|_| Error::NoMemory)?;
    }
    Ok(output)
}

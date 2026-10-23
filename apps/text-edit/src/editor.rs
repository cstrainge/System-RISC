//! Interactive editor using only resident TextEdit and the public Toolbox SDK.
#[cfg(feature = "input-trace")]
use core::fmt::Write;

use mac_toolbox::{textedit::{CursorMovement, EditorId},
                  *};


#[derive(Default)]
pub struct Editor<'a>
{
    pub window:       WindowId,
    editor:           Option<textedit::Editor<'a>>,
    scrollbar:        Option<controls::Control<'a>>,
    scrollbar_state:  Option<(controls::ScrollbarState, i16)>,
    horizontal:       Option<controls::Control<'a>>,
    horizontal_state: Option<(controls::ScrollbarState, i16)>,
    horizontal_scale: u32,
    options:          textedit::DisplayOptions,
    owner:            Option<windows::Window<'a>>,
    pub document:     Option<processes::DocumentRef>,
    file_type:        TypeCode,
    saved_revision:   u64,
    ending:           crate::encoding::LineEnding,
    font:             quickdraw::fonts::FontSelection,
    size:             Point,
    tracking:         bool,
    last_mouse:       Point,
    ticks:            u32,
    defer_drawing:    bool,
    draw_pending:     bool,
    reveal:           bool,
    report_pending:   bool
}
impl<'a> Editor<'a>
{
    pub fn with_font(font: quickdraw::fonts::FontSelection) -> Self
    {
        Self { font,
               ..Default::default() }
    }

    pub fn options(&self) -> textedit::DisplayOptions
    {
        self.options
    }

    pub fn set_options(&mut self, system: &System<'a>, options: textedit::DisplayOptions)
                       -> Result<(), Error>
    {
        if options == self.options
        {
            return Ok(());
        }
        system.textedit().set_display_options(self.id(), options)?;
        self.options = options;
        self.size = Point::default();
        self.draw(system)
    }

    pub fn with_options(mut self, options: textedit::DisplayOptions) -> Self
    {
        self.options = options;
        self
    }

    fn id(&self) -> EditorId
    {
        self.editor.as_ref().map_or(EditorId(0), |editor| editor.id())
    }

    pub fn close(&mut self, _system: &System<'a>) -> Result<(), Error>
    {
        if self.window != WindowId(0)
        {
            if let Some(editor) = &mut self.editor
            {
                editor.dispose()?;
            }
            if let Some(scrollbar) = &self.scrollbar
            {
                scrollbar.control().dispose()?;
            }
            self.scrollbar = None;
            self.horizontal = None;
            self.owner.as_mut().ok_or(Error::BadAbi)?.close()?;
            *self = Self::default();
        }
        Ok(())
    }

    #[cfg(feature = "input-trace")]
    fn report(&mut self, system: &System<'a>) -> Result<(), Error>
    {
        if self.defer_drawing
        {
            self.report_pending = true;
            return Ok(());
        }
        let info = system.textedit().info(self.id())?;
        let mut line = crate::Text::<128>::default();
        let _ = writeln!(&mut line,
                         "FINDER_TEXTEDIT_STATE length={} selection={}..{}",
                         info.text_length, info.selection_start, info.selection_end);
        system.log(line.as_str())
    }

    #[cfg(not(feature = "input-trace"))]
    fn report(&mut self, _system: &System<'a>) -> Result<(), Error>
    {
        Ok(())
    }

    /// Apply queued edits immediately, committing their final drawing together.
    pub fn defer_drawing(&mut self)
    {
        self.defer_drawing = true;
    }

    pub fn flush_drawing(&mut self, system: &System<'a>) -> Result<(), Error>
    {
        self.defer_drawing = false;
        if core::mem::take(&mut self.draw_pending)
        {
            self.draw(system)?;
        }
        if core::mem::take(&mut self.report_pending)
        {
            self.report(system)?;
        }
        Ok(())
    }

    pub fn active(&self, system: &System<'a>) -> bool
    {
        self.window != WindowId(0)
        && system.processes().current().is_ok_and(|process| process.foreground)
        && system.window(WindowId(0)).is_ok_and(|w| w.id == self.window)
    }

    pub fn new_document(&mut self, system: &System<'a>, title: &str) -> Result<(), Error>
    {
        self.open_text(system, title, "")
    }

    pub fn font(&self) -> quickdraw::fonts::FontSelection
    {
        self.font
    }

    pub fn set_font(&mut self, system: &System<'a>, font: quickdraw::fonts::FontSelection)
                    -> Result<(), Error>
    {
        if self.font == font
        {
            return Ok(());
        }
        system.textedit().set_font(self.id(), font)?;
        self.font = font;
        self.draw(system)
    }

    pub fn info(&self, system: &System<'a>) -> Result<textedit::EditorInfo, Error>
    {
        system.textedit().info(self.id())
    }

    pub fn modified(&self, revision: u64) -> bool
    {
        revision != self.saved_revision
    }

    pub fn dirty(&self, system: &System<'a>) -> Result<bool, Error>
    {
        Ok(system.textedit().info(self.id())?.content_revision != self.saved_revision)
    }

    pub fn name<'b>(&self, system: &System<'a>, output: &'b mut [u8]) -> Result<&'b str, Error>
    {
        let window = system.windows().info(self.window)?;
        let name = window.title();
        let destination = output.get_mut(..name.len()).ok_or(Error::NoMemory)?;
        destination.copy_from_slice(name.as_bytes());
        core::str::from_utf8(destination).map_err(|_| Error::CorruptData)
    }

    pub fn save(&mut self, system: &System<'a>, force_dialog: bool,
                location: standard_file::Location, occupied: &[processes::DocumentRef])
                -> Result<bool, Error>
    {
        let existing = match self.document
        {
            Some(file) => Some(system.files().catalog(file.volume, file.id)?),
            None => None
        };
        let readonly = match existing
        {
            Some(entry) =>
            {
                system.files().volume(entry.volume)?.read_only
                || entry.flags.contains(files::CatalogFlags::LOCKED)
            }
            None => false
        };
        let choice = if force_dialog || existing.is_none() || readonly
        {
            let mut title = [0; 256];
            let title = self.name(system, &mut title)?;
            let mut prompt = [0; 256];
            let prompt = crate::text(system, 4, &mut prompt)?;
            let place = existing.filter(|_| !readonly).map_or(location, |entry| standard_file::Location { volume: entry.volume, directory: entry.parent });
            let Some(choice) = system.standard_file().save_as(place, prompt, title)?
            else
            {
                return Ok(false);
            };
            choice
        }
        else
        {
            standard_file::Selection { entry:     existing.unwrap(),
                                       replacing: true }
        };
        if occupied.iter()
                   .any(|file| file.volume == choice.entry.volume && file.id == choice.entry.id)
        {
            return Err(Error::ServiceBusy);
        }
        self.write(system, choice)?;
        Ok(true)
    }

    fn write(&mut self, system: &System<'a>, choice: standard_file::Selection)
             -> Result<(), Error>
    {
        let mut text = [0; textedit::MAX_BYTES];
        let text = system.textedit().text(self.id(), &mut text)?;
        let length = text.len()
                         .checked_add(if self.ending == crate::encoding::LineEnding::CrLf
                                      {
                                          text.bytes().filter(|b| *b == b'\r').count()
                                      }
                                      else
                                      {
                                          0
                                      })
                         .ok_or(Error::NoMemory)?;
        let mut writer =
            system.files().create_data(files::DataFileSpec { volume:    choice.entry.volume,
                                                              parent:    choice.entry.parent,
                                                              name:      choice.name(),
                                                              replacing:
                                                                  choice.replacing
                                                                        .then_some(choice.entry.id),
                                                              file_type:
                                                                  if self.file_type
                                                                     == TypeCode::new(*b"JSON")
                                                                  {
                                                                      self.file_type
                                                                  }
                                                                  else
                                                                  {
                                                                      TypeCode::new(*b"TEXT")
                                                                  },
                                                              creator:   TypeCode::new(*b"TEDT") },
                                        length)?;
        let mut encoded = [0; 4096];
        let mut start = 0;
        while start < text.len()
        {
            let mut end = (start + 2048).min(text.len());
            while !text.is_char_boundary(end)
            {
                end -= 1;
            }
            let count = crate::encoding::encode_chunk(&text[start..end], self.ending, &mut encoded).ok_or(Error::NoMemory)?;
            writer.write_all(&encoded[..count])?;
            start = end;
        }
        let entry = writer.finish()?;
        // The verified publication is authoritative even if presentation fails.
        self.document = Some(processes::DocumentRef { volume: entry.volume,
                                                      id:     entry.id });
        self.saved_revision = system.textedit().info(self.id())?.content_revision;
        system.windows().set_title(self.window, entry.name())?;
        system.log("TEXT_EDIT_DOCUMENT_SAVED\n")
    }

    fn open_text(&mut self, system: &System<'a>, title: &str, sample: &str) -> Result<(), Error>
    {
        self.prune(system)?;
        if self.window == WindowId(0)
        {
            let resource =
                system.resources()
                      .application()
                      .get(resources::ResourceKey { kind: mac_toolbox::TypeCode::new(*b"WIND"),
                                                    id:   2003 })?;
            let mut bytes = [0; 20];
            if resource.size() != bytes.len() || resource.read(0, &mut bytes)? != bytes.len()
            {
                return Err(Error::BadArgument);
            }
            resource.release()?;
            let word = |at| i16::from_be_bytes([bytes[at], bytes[at + 1]]);
            let bounds = Rect::new(word(2), word(0), word(6), word(4));
            if word(8) != 8 || bounds.is_empty()
            {
                return Err(Error::BadArgument);
            }
            let owner = system.windows()
                              .create_owned(windows::WindowSpec::new(bounds, title).procedure(8)
                                                                                   .minimum(240,
                                                                                            120))?;
            let window = owner.id();
            let view =
                Rect::new(8, 8, bounds.right - bounds.left - 8, bounds.bottom - bounds.top - 24);
            let editor = match system.textedit().create_owned(view, view)
            {
                Ok(id) => id,
                Err(e) =>
                {
                    return Err(e);
                }
            };
            self.saved_revision = editor.set_text(sample)?.content_revision;
            system.textedit().set_font(editor.id(), self.font)?;
            editor.set_display_options(self.options)?;
            self.window = window;
            self.editor = Some(editor);
            self.owner = Some(owner);
        }
        system.select_window(self.window)?;
        system.textedit().activate(self.id(), true)?;
        self.ticks = system.events().ticks()?;
        self.draw(system)?;
        system.log("FINDER_TEXTEDIT_READY\n")?;
        self.report(system)
    }

    pub fn open_document(&mut self, system: &System<'a>, file: processes::DocumentRef)
                         -> Result<(), Error>
    {
        let info = system.files().catalog(file.volume, file.id)?;
        if !matches!(info.file_type.bytes(), [b'T', b'E', b'X', b'T'] | [b'J', b'S', b'O', b'N'])
        {
            return Err(Error::NotSupported);
        }
        let mut fork = system.files().open(file.volume, file.id, files::Fork::Data)?;
        let length = usize::try_from(fork.len()?).map_err(|_| Error::NoMemory)?;
        if length > mac_toolbox::abi::textedit::MAX_BYTES
        {
            return Err(Error::NoMemory);
        }
        let mut bytes = [0u8; mac_toolbox::abi::textedit::MAX_BYTES];
        let mut received = 0;
        while received < length
        {
            let count = fork.read(&mut bytes[received..length])?;
            if count == 0
            {
                return Err(Error::CorruptData);
            }
            received += count;
        }
        if fork.read(&mut [0; 1])? != 0
        {
            return Err(Error::ScanStale);
        }
        fork.close()?;
        let (out, ending) = crate::encoding::decode(&mut bytes[..received]);
        let text = core::str::from_utf8(&bytes[..out]).map_err(|_| Error::CorruptData)?;
        self.open_text(system, info.name(), text)?;
        self.ending = ending;
        self.file_type = info.file_type;
        self.document = Some(file);
        system.log("TEXT_EDIT_DOCUMENT_OPEN\n")
    }

    fn prune(&mut self, system: &System<'a>) -> Result<(), Error>
    {
        if self.window == WindowId(0)
        {
            return Ok(());
        }
        match system.window(self.window)
        {
            Ok(_) =>
            {}
            Err(Error::NotFound) =>
            {
                if let Some(editor) = &mut self.editor
                {
                    editor.dispose()?;
                }
                *self = Self::default();
            }
            Err(status) => return Err(status)
        }
        Ok(())
    }

    pub fn draw(&mut self, system: &System<'a>) -> Result<(), Error>
    {
        if self.defer_drawing
        {
            self.draw_pending = true;
            return Ok(());
        }
        let window = system.window(self.window)?;
        let size = Point { h: window.bounds.right - window.bounds.left,
                           v: window.bounds.bottom - window.bounds.top };
        let full = size != self.size;
        let bottom = size.v - if self.options.wrap { 0 } else { 15 };
        // Numbering owns an edge-to-edge gutter; text retains its top inset.
        let view = Rect::new(if self.options.line_numbers { 0 } else { 8 },
                             if self.options.line_numbers { 0 } else { 8 },
                             size.h - 16,
                             bottom);
        let dest = Rect::new(view.left, 8, view.right, view.bottom);
        if full
        {
            system.textedit().set_rects(self.id(), dest, view)?;

            system.windows().set_grow_icon_clip(self.window,
                                                 Some(Rect::new(size.h - 15,
                                                                size.v - 15,
                                                                size.h,
                                                                size.v)))?;
            self.size = size;
            #[cfg(feature = "input-trace")]
            {
                let mut line = crate::Text::<96>::default();
                writeln!(&mut line, "FINDER_TEXTEDIT_LAYOUT width={} height={}",size.h,size.v).map_err(|_|Error::NoMemory)?;
                system.log(line.as_str())?;
            }
        }
        if core::mem::take(&mut self.reveal)
        {
            system.textedit().reveal_selection(self.id())?;
        }
        let info = system.textedit().idle(self.id(), self.ticks)?;
        let height = i32::from(info.line_height).max(1);
        let visible = ((i32::from(view.bottom) - i32::from(view.top)) / height).max(1);
        let maximum = info.scroll_max.div_ceil(height as u32).min(i16::MAX as u32) as i16;
        let state = controls::ScrollbarState { bounds: Rect::new(size.h - 15,
                                                                 -1,
                                                                 size.h + 1,
                                                                 size.v - 14),
                                               value: ((info.scroll_y + height as u32 - 1)
                                                       / height as u32)
                                                                       .min(maximum as u32)
                                                      as i16,
                                               maximum,
                                               active: u32::from(info.active && maximum != 0),
                                               ..Default::default() };
        let page = (visible - 1).max(1).min(i16::MAX as i32) as i16;
        if let Some(control) = &self.scrollbar
        {
            if self.scrollbar_state != Some((state, page))
            {
                control.control().configure_scrollbar(state, 1, page)?;
            }
        }
        else
        {
            self.scrollbar =
                Some(system.controls().create_scrollbar(self.window, state, 1, page)?);
        }
        self.scrollbar_state = Some((state, page));
        if !self.options.wrap
        {
            let horizontal = system.textedit().display_info(self.id())?;
            let scale = horizontal.scroll_max_x.div_ceil(i16::MAX as u32).max(1);
            let maximum = horizontal.scroll_max_x.div_ceil(scale).min(i16::MAX as u32) as i16;
            let hstate = controls::ScrollbarState { bounds: Rect::new(-1,
                                                                      size.v - 15,
                                                                      size.h - 14,
                                                                      size.v + 1),
                                                    value: horizontal.scroll_x
                                                                     .div_ceil(scale)
                                                                     .min(maximum as u32)
                                                           as i16,
                                                    maximum,
                                                    active: u32::from(info.active
                                                                      && maximum != 0),
                                                    ..Default::default() };
            let hpage =
                ((i32::from(view.right) - i32::from(view.left)).max(1) as u32 / scale).max(1)
                                                                                      .min(i16::MAX
                                                                                           as u32)
                as i16;
            if let Some(control) = &self.horizontal
            {
                if self.horizontal_state != Some((hstate, hpage))
                {
                    control.control().configure_scrollbar(hstate, 1, hpage)?;
                }
            }
            else
            {
                self.horizontal =
                    Some(system.controls().create_scrollbar(self.window, hstate, 1, hpage)?);
            }
            self.horizontal_scale = scale;
            self.horizontal_state = Some((hstate, hpage));
        }
        else
        {
            self.horizontal = None;
            self.horizontal_state = None;
        }
        let port = system.quickdraw().graphics(self.window);
        let bounds = if full { Rect::new(0, 0, size.h, size.v) } else { view };
        let update = port.masked_rectangles(&[bounds])?;
        port.reset()?;
        port.colors(0xffffff, 0xffffff)?;
        port.paint_rect(bounds)?;
        system.textedit().draw(self.id(), self.window)?;
        if full
        {
            self.scrollbar.as_ref().ok_or(Error::BadAbi)?.control().draw()?;
            if let Some(horizontal) = &self.horizontal
            {
                horizontal.control().draw()?;
            }
        }
        port.reset()?;
        update.commit()
    }

    pub fn command(&mut self, system: &System<'a>, command: u32, ticks: u32)
                   -> Result<bool, Error>
    {
        self.ticks = ticks;
        if !self.active(system)
        {
            return Ok(false);
        }
        let edit = system.textedit();
        match command
        {
            crate::menus::CUT =>
            {
                edit.cut(self.id())?;
            }
            crate::menus::COPY =>
            {
                edit.copy(self.id())?;
            }
            crate::menus::PASTE =>
            {
                edit.paste(self.id())?;
            }
            crate::menus::CLEAR =>
            {
                edit.delete(self.id())?;
            }
            crate::menus::SELECT_ALL =>
            {
                edit.select(self.id(), 0, u32::MAX)?;
            }
            _ => return Ok(false)
        }
        self.reveal = !matches!(command, crate::menus::COPY | crate::menus::SELECT_ALL);
        self.draw(system)?;
        system.log("FINDER_TEXTEDIT_EDIT\n")?;
        self.report(system)?;
        Ok(true)
    }

    pub fn event(&mut self, system: &System<'a>, event: &Event, position: PointerPosition)
                 -> Result<bool, Error>
    {
        self.ticks = event.input().when;
        self.prune(system)?;
        if self.window == WindowId(0)
        {
            return Ok(false);
        }
        let edit = system.textedit();
        if matches!(event, Event::Activate { .. })
        {
            edit.activate(self.id(), self.active(system))?;
            self.draw(system)?;
        }
        if matches!(event, Event::Update { .. } | Event::WindowGeometryChanged { .. })
           && event.window().unwrap_or_default() == self.window
        {
            self.draw(system)?;
            return Ok(true);
        }
        if !self.active(system)
        {
            self.tracking = false;
            return Ok(false);
        }
        match event
        {
            Event::MouseDown { .. } if event.window().unwrap_or_default() == self.window =>
            {
                let local = position.in_window(self.window)?;
                if let Some(control) = self.scrollbar.as_ref().map(|owner| owner.control())
                {
                    if control.track_at(local, |state| {
                                  let info = self.info(system)?;
                                  system.textedit()
                                        .scroll_to(self.id(),
                                                   state.scrollbar.value.max(0) as u32
                                                   * info.line_height.max(1) as u32)?;
                                  self.draw(system)
                              })?
                    {
                        return Ok(true);
                    }
                }
                if let Some(control) = self.horizontal.as_ref().map(|owner| owner.control())
                {
                    if control.track_at(local, |state| {
                                  let info = self.info(system)?;
                                  system.textedit().scroll_axes(self.id(),
                                                                 state.scrollbar.value.max(0)
                                                                 as u32
                                                                 * self.horizontal_scale,
                                                                 info.scroll_y)?;
                                  self.draw(system)
                              })?
                    {
                        return Ok(true);
                    }
                }
                self.last_mouse = event.input().at;
                edit.click(self.id(),
                           position.in_window(self.window)?.point()?,
                           event.input()
                                .modifiers
                                .contains(mac_toolbox::events::Modifiers::SHIFT))?;
                self.tracking = true;
                self.draw(system)?;
                Ok(true)
            }
            Event::MouseUp { .. } if self.tracking =>
            {
                self.tracking = false;
                Ok(true)
            }
            // A key release does not change browser policy or the filesystem.
            Event::KeyUp { .. } => Ok(true),
            Event::KeyDown { .. } =>
            {
                let command =
                    event.input().modifiers.contains(mac_toolbox::events::Modifiers::COMMAND);
                let movement = match event.key().map_or(0, |key| key.code())
                {
                    0x7b => Some(CursorMovement::Left),
                    0x7c => Some(CursorMovement::Right),
                    0x7e => Some(CursorMovement::Up),
                    0x7d => Some(CursorMovement::Down),
                    0x73 =>
                    {
                        Some(if command
                             {
                                 CursorMovement::DocumentStart
                             }
                             else
                             {
                                 CursorMovement::LineStart
                             })
                    }
                    0x77 =>
                    {
                        Some(if command
                             {
                                 CursorMovement::DocumentEnd
                             }
                             else
                             {
                                 CursorMovement::LineEnd
                             })
                    }
                    _ => None
                };
                if let Some(movement) = movement
                {
                    edit.move_cursor(self.id(),
                                     movement,
                                     event.input()
                                          .modifiers
                                          .contains(mac_toolbox::events::Modifiers::SHIFT))?;
                    self.reveal = true;
                    self.draw(system)?;
                    self.report(system)?;
                    return Ok(true);
                }
                if command
                {
                    return Ok(true);
                }
                let key = event.key().map_or('\0', |key| key.character());
                if key == '\r'
                   || key == '\u{8}'
                   || ('\u{1c}'..='\u{1f}').contains(&key)
                   || !key.is_control()
                {
                    edit.key(self.id(), key)?;
                    self.reveal = true;
                    self.draw(system)?;
                    self.report(system)?;
                }
                Ok(true)
            }
            Event::Idle { .. } =>
            {
                if self.tracking && event.input().at != self.last_mouse
                {
                    edit.click(self.id(), position.in_window(self.window)?.point()?, true)?;
                    self.last_mouse = event.input().at;
                    self.draw(system)?;
                }
                if edit.blink(self.id(), self.window, event.input().when)?
                {
                    self.draw(system)?;
                }
                Ok(self.tracking)
            }
            _ => Ok(false)
        }
    }
}

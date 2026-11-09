//! Window-backed CLAP launcher and output viewer.
use std::{cell::RefCell,
          collections::VecDeque,
          rc::Rc,
          sync::{Arc, OnceLock,
                 atomic::{AtomicBool, Ordering},
                 mpsc::{self, Receiver, TryRecvError}},
          thread};

use mac_toolbox::{Error, Event, PointerPosition, Rect, ResourceScope, System, TypeCode, VolumeId,
                  WindowId,
                  controls::{ControlId, Reflow, ScrollbarState},
                  events::{Key, Modifiers, TimeTask},
                  files::CatalogInfo,
                  pipes::{PipeReader, PipeWriter},
                  processes::ProcessId,
                  standard_file::{Location, TypeFilter}};
use sr_event_async::{CHILD, Engine, ScopeOwner, Source as ReadySource, TIMER};
use sr_event_dispatch::{Handler, Router};

mod screen;
mod sixel;
mod vt;
use screen::{BAR_WIDTH, FONT, ROW_HEIGHT, Screen, Style};

mac_toolbox::include_resource_ids!();
use resources::commands::{NEW_SHELL, OPEN_COMMAND, QUIT, SEND_EOF};

const CLI_FOLDER: &str = "/Applications/Utility/Cli";
const LINE_BYTES: usize = 72;
const PIPE_BYTES: usize = 4096;
struct Command<'a>
{
    id:                      ProcessId,
    input:                   Option<PipeWriter<'a>>,
    line:                    String,
    pending:                 VecDeque<u8>,
    eof:                     bool,
    readers:                 [Option<PipeReader<'a>>; 2],
    streams:                 [vt::Stream; 2],
    hangup_refusal_reported: bool
}

impl<'a> Command<'a>
{
    fn start(system: &System<'a>, window: WindowId, entry: CatalogInfo, path: &str)
             -> Result<Self, Error>
    {
        let pipes = system.pipes();
        let (mut input, mut input_writer) = pipes.create(PIPE_BYTES)?;
        let (mut output, mut child_output) = pipes.create(PIPE_BYTES)?;
        let (mut error, mut child_error) = pipes.create(PIPE_BYTES)?;
        output.set_blocking(false)?;
        error.set_blocking(false)?;
        input_writer.set_blocking(false)?;
        let id = pipes.launch_cli_with_path(window, entry.volume, entry.id, path, &mut input,
                                            &mut child_output, &mut child_error)?;
        Ok(Self { id,
                  input: Some(input_writer),
                  line: String::new(),
                  pending: VecDeque::new(),
                  eof: false,
                  readers: [Some(output), Some(error)],
                  streams: [vt::Stream::new(Style::NORMAL), vt::Stream::new(Style::NORMAL)],
                  hangup_refusal_reported: false })
    }

    fn hang_up(&mut self)
    {
        // EOF belongs to the child. Keep its output readers and result receipt
        // until it has flushed, closed its writers and returned its real result.
        self.input.take();
        self.eof = true;
        self.hangup_refusal_reported = false;
        self.pending.clear();
        self.line.clear();
    }

    fn poll_hangup(&mut self, system: &System<'a>, ready: u8) -> bool
    {
        if self.hangup_refusal_reported
        {
            return false;
        }
        let result = (|| -> Result<bool, Error> {
            let mut bytes = [0; PIPE_BYTES];
            for (stream, reader) in self.readers.iter_mut().enumerate()
            {
                if ready & (1 << stream) == 0
                {
                    continue;
                }
                if let Some(input) = reader
                {
                    match input.read(&mut bytes)
                    {
                        Ok(0) => *reader = None,
                        Ok(_) | Err(Error::WouldBlock) => (),
                        Err(cause) => return Err(cause)
                    }
                }
            }
            if self.readers.iter().all(Option::is_none)
               && let Some(exit) = system.processes().try_wait_child(self.id)?
            {
                system.processes().forget_child(self.id)?;
                let _ = system.log(&format!("TERMINAL_SESSION_EXIT child={} code={} aborted={}\n",
                                            self.id.0, exit.code, exit.aborted));
                return Ok(true);
            }
            Ok(false)
        })();
        match result
        {
            Ok(done) => done,
            Err(cause) =>
            {
                // Retain the unique handles and receipt on refusal; other
                // sessions and the menu remain usable; explicit Quit can retry.
                if !self.hangup_refusal_reported
                {
                    let _ = system.log(&format!("TERMINAL_HANGUP_RETAINED child={} \
                                                 cause={cause:?}\n",
                                                self.id.0));
                    self.hangup_refusal_reported = true;
                }
                false
            }
        }
    }

    fn key(&mut self, key: Key, modifiers: Modifiers, output: &mut Screen) -> bool
    {
        if self.input.is_none() || self.eof || modifiers.contains(Modifiers::COMMAND)
        {
            return false;
        }
        let character = key.character();
        if output.alternate() || output.raw_input
        {
            let bytes = match key.code()
            {
                0x7b..=0x7e =>
                {
                    let suffix = match key.code()
                    {
                        0x7b => 'D',
                        0x7c => 'C',
                        0x7d => 'B',
                        _ => 'A'
                    };
                    format!("\x1b{}{suffix}", if output.application_keys { 'O' } else { '[' })
                }
                0x73 => "\x1b[H".into(),
                0x77 => "\x1b[F".into(),
                0x74 => "\x1b[5~".into(),
                0x79 => "\x1b[6~".into(),
                0x75 => "\x1b[3~".into(),
                0x33 => "\x7f".into(),
                _ if modifiers.contains(Modifiers::CONTROL) && character.is_ascii_alphabetic() =>
                {
                    ((character as u8 & 31) as char).to_string()
                }
                _ if character != '\0' => character.to_string(),
                _ => return false
            };
            if self.pending.len() + bytes.len() <= PIPE_BYTES
            {
                self.pending.extend(bytes.bytes());
            }
            return true;
        }
        let eof =
            modifiers.contains(Modifiers::CONTROL) && matches!(character, 'd' | 'D' | '\u{4}');
        if eof || matches!(character, '\r' | '\n')
        {
            return self.submit(eof, output);
        }
        if modifiers.contains(Modifiers::CONTROL)
        {
            return false;
        }
        if matches!(character, '\u{8}' | '\u{7f}')
        {
            return self.line.pop().is_some();
        }
        if !character.is_control() && self.line.len() + character.len_utf8() <= LINE_BYTES
        {
            self.line.push(character);
            return true;
        }
        false
    }

    fn submit(&mut self, eof: bool, output: &mut Screen) -> bool
    {
        if self.input.is_none() || self.eof
        {
            return false;
        }
        if self.pending.len() + self.line.len() + usize::from(!eof) > PIPE_BYTES
        {
            return false;
        }
        let line = std::mem::take(&mut self.line);
        self.pending.extend(line.bytes());
        if !eof
        {
            self.pending.push_back(b'\n');
        }
        if !eof || !line.is_empty()
        {
            output.line(false, &line);
        }
        self.eof = eof;
        return true;
    }

    fn pump_input(&mut self) -> Result<bool, Error>
    {
        let Some(writer) = &mut self.input
        else
        {
            return Ok(false);
        };
        if !self.pending.is_empty()
        {
            let (first, _) = self.pending.as_slices();
            match writer.write(&first[..first.len().min(512)])
            {
                Ok(count) =>
                {
                    self.pending.drain(..count);
                }
                Err(Error::WouldBlock) => return Ok(false),
                Err(Error::BrokenPipe) =>
                {
                    self.input = None;
                    self.pending.clear();
                    return Ok(!std::mem::take(&mut self.line).is_empty());
                }
                Err(error) => return Err(error)
            }
        }
        if self.eof
           && self.pending.is_empty()
           && let Some(writer) = self.input.take()
        {
            writer.close()?;
        }
        Ok(false)
    }

    fn poll(&mut self, system: &System<'a>, output: &mut Screen, ready: u8)
            -> Result<(bool, bool), Error>
    {
        if self.hangup_refusal_reported
        {
            return Ok((false, false));
        }
        let mut changed = if ready & 4 != 0 { self.pump_input()? } else { false };
        let queued_before_output = self.pending.len();
        for (stream, slot) in self.readers.iter_mut().enumerate()
        {
            if ready & (1 << stream) == 0
            {
                continue;
            }
            let Some(reader) = slot
            else
            {
                continue;
            };
            // One bounded read drains the pipe's actual capacity. A ready
            // output burst must not take four timer wakes to reach the parser.
            let mut bytes = [0; PIPE_BYTES];
            match reader.read(&mut bytes)
            {
                Ok(0) =>
                {
                    *slot = None;
                }
                Ok(count) =>
                {
                    self.streams[stream].advance(&bytes[..count], output, &mut self.pending);
                    changed = true;
                }
                Err(Error::WouldBlock) => (),
                Err(error) => return Err(error)
            }
        }
        if self.pending.len() > queued_before_output
        {
            // Answer queries parsed above during this pass, rather than
            // leaving the child blocked until the next UI polling interval.
            changed |= self.pump_input()?;
        }
        if self.readers.iter().all(Option::is_none)
           && let Some(exit) = system.processes().try_wait_child(self.id)?
        {
            system.processes().forget_child(self.id)?;

            output.switch(false);
            output.line(false,
                        &format!("Application has terminated with exit code {}.", exit.code));
            system.log(&format!("TERMINAL_CHILD_EXIT code={} aborted={}\n",
                                exit.code, exit.aborted))?;
            return Ok((true, true));
        }
        Ok((changed, false))
    }
}

enum LaunchTarget
{
    Shell,
    File(CatalogInfo)
}

struct LaunchDefaults
{
    volume: VolumeId,
    path:   String
}

fn launch_defaults(system: &System<'_>) -> Result<&'static LaunchDefaults, Error>
{
    static DEFAULTS: OnceLock<LaunchDefaults> = OnceLock::new();
    if let Some(defaults) = DEFAULTS.get()
    {
        return Ok(defaults);
    }
    let files = system.files();
    let boot = files.startup_volume()?;
    let _ = DEFAULTS.set(LaunchDefaults { volume: boot.id,
                                          path: format!("/Volumes/{}{CLI_FOLDER}", boot.name()) });
    DEFAULTS.get().ok_or(Error::BadAbi)
}

// A launch can park on filesystem, loader, or extension work. Keep the
// Window Manager event loop free and serialize launches from this Terminal.
#[derive(Default)]
struct Launcher
{
    active:   Option<(WindowId, Receiver<Result<Command<'static>, Error>>, Arc<AtomicBool>)>,
    retiring: Option<Command<'static>>
}

impl Launcher
{
    fn poll_completion(&mut self, system: &System<'static>, sessions: &mut [SessionRow],
                       retiring_ready: u8)
                       -> Result<(), Error>
    {
        if let Some(command) = &mut self.retiring
        {
            if retiring_ready != 0 && command.poll_hangup(system, retiring_ready)
            {
                self.retiring = None;
            }
            else
            {
                return Ok(());
            }
        }
        if let Some((window, receiver, ready)) = self.active.as_ref()
           && ready.load(Ordering::Acquire)
        {
            let result = match receiver.try_recv()
            {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => Err(Error::BadAbi)
            };
            if let Some(session) =
                sessions.iter_mut().find(|session| session.target.borrow().window == *window)
            {
                session.target.borrow_mut().finish_launch(system, result)?;
            }
            else if let Ok(mut command) = result
            {
                command.hang_up();
                self.retiring = Some(command);
            }
            self.active = None;
            if self.retiring.is_some()
            {
                return Ok(());
            }
        }

        Ok(())
    }

    fn start_pending(&mut self, system: &System<'static>, sessions: &mut [SessionRow],
                     notifier: sr_event_async::Notifier)
                     -> Result<(), Error>
    {
        if self.active.is_some() || self.retiring.is_some()
        {
            return Ok(());
        }
        let Some(session) =
            sessions.iter_mut().find(|session| session.target.borrow().pending.is_some())
        else
        {
            return Ok(());
        };
        let mut session = session.target.borrow_mut();
        let Some(target) = session.pending.take()
        else
        {
            return Ok(());
        };
        let (sender, receiver) = mpsc::channel();
        let ready = Arc::new(AtomicBool::new(false));
        let completed = ready.clone();
        let window = session.window;
        match thread::Builder::new().name("Terminal launch".into()).spawn(move || {
            let result = (|| {
                let system = System::new()?;
                let defaults = launch_defaults(&system)?;
                let entry = match target
                {
                    LaunchTarget::Shell =>
                    {
                        system.files().lookup(defaults.volume, mac_toolbox::Cnid(0),
                                              "/Applications/Utility/Cli/Shelly")?
                    }
                    LaunchTarget::File(entry) => entry
                };
                Command::start(&system, window, entry, &defaults.path)
            })();
            if let Err(mpsc::SendError(Ok(command))) = sender.send(result)
               && let Ok(system) = System::new()
            {
                let _ = system.processes().forget_child(command.id);
            }
            completed.store(true,Ordering::Release);
            if let Err(cause)=notifier.notify() {
                if let Ok(system)=System::new(){let _=system.log(&format!("TERMINAL_LAUNCH_WAKE_REFUSED cause={cause:?}\n"));}
            }
        })
        {
            Ok(_) => self.active = Some((window, receiver,ready)),
            Err(error) =>
            {
                session.output.line(true, &format!("Could not start launcher: {error}"));
                session.repaint(system)?;
            }
        }
        Ok(())
    }
}

struct Session<'a>
{
    window:                WindowId,
    focused:               bool,
    output:                Screen,
    command:               Option<Command<'a>>,
    pending:               Option<LaunchTarget>,
    scrollbar:             ControlId,
    bar_state:             (i16, i16, i16, i16),
    timer:                 TimeTask<'a>,
    cursor_on:             bool,
    closing:               bool,
    shell:                 bool,
    initial_frame_pending: bool
}

impl<'a> Session<'a>
{
    fn open(system: &System<'a>, target: LaunchTarget, title: &str, count: usize)
            -> Result<Self, Error>
    {
        let cell_width = system.quickdraw().fonts().text_width(FONT, " ")?;
        let offset = (count % 8) as i16 * 12;
        let window =
            system.new_window(Rect::new(64 + offset, 84 + offset, 704 + offset, 464 + offset),
                              title,
                              (420, 230),
                              0)?;
        if let Err(error) = system.windows().set_close_deferred(window, true)
        {
            let _ = system.close_window(window);
            return Err(error);
        }
        let bounds = system.window(window)?.bounds;
        let (width, height) = (bounds.right - bounds.left, bounds.bottom - bounds.top);
        let state = bar(width, height, 0, 0);
        let scrollbar = match system.controls().create_scrollbar(window,
                                                                 state,
                                                                 1,
                                                                 (height / ROW_HEIGHT).max(1),
                                                                 Reflow::No)
        {
            Ok(bar) => bar.into_id(),
            Err(error) =>
            {
                let _ = system.close_window(window);
                return Err(error);
            }
        };
        let mut output = Screen::new(cell_width, width, height);
        output.line(false, "Enter sends a line. Ctrl+D sends EOF.");
        let shell = matches!(target, LaunchTarget::Shell);
        let mut timer = TimeTask::new(system.events());
        if let Err(cause) = timer.insert()
        {
            if let Err(rollback) = system.close_window(window)
            {
                let _ = system.log(&format!("TERMINAL_TIMER_ADMISSION_ROLLBACK window={} \
                                             cause={cause:?} rollback={rollback:?}\n",
                                            window.0));
            }
            return Err(cause);
        }
        Ok(Self { window,
                  shell,
                  focused: true,
                  output,
                  command: None,
                  pending: Some(target),
                  scrollbar,
                  bar_state: (width, height, 0, 0),
                  timer,
                  cursor_on: true,
                  closing: false,
                  initial_frame_pending: true })
    }

    fn finish_launch(&mut self, system: &System<'a>, result: Result<Command<'a>, Error>)
                     -> Result<(), Error>
    {
        match result
        {
            Ok(mut command) =>
            {
                if self.closing
                {
                    command.hang_up();
                }
                self.command = Some(command);
                if !self.closing && self.focused
                {
                    self.reset_blink()?;
                }
            }
            Err(error) if self.closing =>
            {
                let _ = system.log(&format!("TERMINAL_CLOSED_LAUNCH cause={error:?}\n"));
            }
            Err(error) =>
            {
                self.output.line(true, &format!("Could not launch command: {error:?}"));
                self.repaint(system)?;
            }
        }
        Ok(())
    }

    fn reset_blink(&mut self) -> Result<(), Error>
    {
        self.timer.reset(500).map(|_| ())
    }

    fn close(&mut self)
    {
        self.closing = true;
        self.pending = None;
        if let Some(command) = &mut self.command
        {
            command.hang_up();
        }
    }

    fn update(&mut self, system: &System<'a>, event: &Event<'_>, position: PointerPosition,
              key_target: Option<WindowId>, send_eof: bool, service_io: u8, timer_event: bool)
              -> Result<(), Error>
    {
        if self.closing
        {
            if let Some(command) = &mut self.command
               && command.poll_hangup(system, service_io)
            {
                self.command = None;
            }
            return Ok(());
        }
        let full = matches!(event, Event::Update { window, .. } if *window == self.window);
        if let Event::Activate { window,
                                 active,
                                 .. } = event
           && *window == self.window
        {
            self.focused = *active;
            self.reset_blink()?;
            self.cursor_on = true;
        }
        let mut reset_blink = false;
        if let Some(child) = &mut self.command
        {
            if key_target == Some(self.window)
            {
                if send_eof
                {
                    child.submit(true, &mut self.output);
                }
                else if let Event::KeyDown { key,
                                               input,
                                               .. } = event
                {
                    self.output.browse(self.output.history().1);
                    child.key(*key, input.modifiers, &mut self.output);
                    reset_blink = true;
                    self.cursor_on = true;
                }
            }
            // EOF with no buffered bytes is a close operation, not a writable
            // wait. Do it immediately so an empty Ctrl+D cannot strand a reader.
            if child.eof && child.pending.is_empty() && child.input.is_some()
            {
                child.pump_input()?;
            }
            if service_io != 0
            {
                match child.poll(system, &mut self.output, service_io)
                {
                    Ok((_, true)) =>
                    {
                        self.command = None;
                        if self.shell
                        {
                            system.close_window(self.window)?;
                            self.closing = true;
                        }
                    }
                    Ok(_) => (),
                    Err(error) =>
                    {
                        if let Some(command) = &mut self.command
                        {
                            command.hang_up();
                            command.hangup_refusal_reported = true;
                        }
                        self.output.switch(false);
                        self.output.line(true, &format!("Command I/O failed: {error:?}"));
                    }
                }
            }
        }
        if reset_blink
        {
            self.reset_blink()?;
        }
        if self.focused && self.command.is_some() && timer_event && self.timer.fire()?
        {
            self.cursor_on = !self.cursor_on;
            self.timer.prime(500)?;
        }
        let (draft, cursor) = self.command.as_ref().map_or(("", false), |child| {
                                                       (child.line.as_str(),
                                                        self.focused
                                                        && self.cursor_on
                                                        && child.input.is_some()
                                                        && !child.eof)
                                                   });
        self.output.input(draft, cursor);
        let control = system.controls().control(self.scrollbar);
        if matches!(event, Event::MouseDown { window: Some(window), .. } if *window == self.window)
        {
            let anchor = position.in_window(self.window)?;
            let (width, height, _, _) = self.bar_state;
            if bar_bounds(width, height).contains(anchor.point()?)
            {
                control.track_at(anchor, |action| {
                           self.output.browse(action.scrollbar.value);
                           self.repaint(system)
                       })?;
            }
        }
        if full
        {
            self.paint_update(system)?;
        }
        else
        {
            self.repaint(system)?;
            let (w, h, _, _) = self.bar_state;
            self.scrollbar(system, w, h, false)?;
        }
        Ok(())
    }

    fn scrollbar(&mut self, system: &System<'_>, width: i16, height: i16, full: bool)
                 -> Result<(), Error>
    {
        let (value, max) = self.output.history();
        let next = (width, height, value, max);
        let control = system.controls().control(self.scrollbar);
        if self.bar_state != next
        {
            let info = control.configure_scrollbar(bar(width, height, value, max),
                                                   1,
                                                   (height / ROW_HEIGHT).max(1))?;
            self.bar_state = next;
            if full && !info.changed
            {
                control.draw()?;
            }
        }
        else if full
        {
            control.draw()?;
        }
        Ok(())
    }

    fn paint_update(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        let bounds = system.window(self.window)?.bounds;
        self.output.resize(bounds.right - bounds.left, bounds.bottom - bounds.top);
        let damage = system.begin_update(self.window)?;
        let result = self.output
                         .paint(system.quickdraw().graphics(self.window), damage)
                         .and_then(|()| {
                             self.scrollbar(system,
                                            bounds.right - bounds.left,
                                            bounds.bottom - bounds.top,
                                            true)
                         });
        let end = system.end_update(self.window);
        result.and(end)?;
        self.initial_frame_pending = false;
        self.output.take_damage();
        Ok(())
    }

    fn repaint(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        if self.initial_frame_pending
        {
            // Consume the real initial damage now through the normal Update
            // transaction. EndUpdate leaves any newly arrived damage pending.
            return self.paint_update(system);
        }
        if let Some(mut damage) = self.output.take_damage()
        {
            damage.right -= BAR_WIDTH;
            let port = system.quickdraw().graphics(self.window);
            let update = port.masked_rectangles(&[damage])?;
            self.output.paint(port, damage)?;
            update.commit()?;
        }
        Ok(())
    }
}

fn bar_bounds(width: i16, height: i16) -> Rect
{
    // Align with the window frame and leave the bottom-right grow box clear.
    Rect::new(width - BAR_WIDTH, -1, width + 1, height - 14)
}

fn bar(width: i16, height: i16, value: i16, maximum: i16) -> ScrollbarState
{
    ScrollbarState { bounds: bar_bounds(width, height),
                     value,
                     maximum,
                     active: u32::from(maximum > 0),
                     ..Default::default() }
}

enum SessionEvent
{
    Ui(Event<'static>, PointerPosition, Option<WindowId>, bool),
    Ready(u8),
    Timer,
    Close
}
struct SessionRow
{
    target:       Rc<RefCell<Session<'static>>>,
    handler:      Handler<SessionEvent>,
    scope:        ScopeOwner,
    registered:   std::cell::Cell<bool>,
    io_queued:    Rc<std::cell::Cell<u8>>,
    timer_queued: Rc<std::cell::Cell<bool>>,
    error:        Rc<RefCell<Option<Error>>>
}
impl SessionRow
{
    fn new(system: System<'static>, engine: &Engine, parent: &ScopeOwner,
           session: Session<'static>)
           -> Self
    {
        let target = Rc::new(RefCell::new(session));
        let scope = parent.child();
        let error = Rc::new(RefCell::new(None));
        let failure = error.clone();
        let cancellation = scope.scope();
        let io_queued = Rc::new(std::cell::Cell::new(0u8));
        let io_delivered = io_queued.clone();
        let timer_queued = Rc::new(std::cell::Cell::new(false));
        let timer_delivered = timer_queued.clone();
        let handler =
            Handler::new(Rc::downgrade(&target),
                         engine.spawner(),
                         scope.scope(),
                         move |session, event| {
                             let result = match event
                             {
                                 SessionEvent::Close
                                 | SessionEvent::Ui(Event::CloseRequested { .. }, _, _, _) =>
                                 {
                                     match system.close_window(session.window)
                                     {
                                         Ok(()) | Err(Error::NotFound) =>
                                         {
                                             session.close();
                                             cancellation.cancel();
                                             Ok(())
                                         }
                                         Err(cause) =>
                                         {
                                             session.output.line(true,
                                                                 &format!("Could not close \
                                                                           window: {cause:?}"));
                                             session.repaint(&system)
                                         }
                                     }
                                 }

                                 SessionEvent::Ui(event, position, key, eof) =>
                                 {
                                     session.update(&system, &event, position, key, eof, 0, false)
                                 }
                                 SessionEvent::Ready(mask) =>
                                 {
                                     io_delivered.set(io_delivered.get() & !mask);
                                     session.update(&system,
                                                    &Event::default(),
                                                    Default::default(),
                                                    None,
                                                    false,
                                                    mask,
                                                    false)
                                 }
                                 SessionEvent::Timer =>
                                 {
                                     timer_delivered.set(false);
                                     session.update(&system,
                                                    &Event::default(),
                                                    Default::default(),
                                                    None,
                                                    false,
                                                    0,
                                                    true)
                                 }
                             };
                             if let Err(cause) = result
                             {
                                 *failure.borrow_mut() = Some(cause);
                             }
                             Box::pin(async {})
                         });
        Self { target,
               handler,
               scope,
               registered: std::cell::Cell::new(false),
               io_queued,
               timer_queued,
               error }
    }

    fn close(&self)
    {
        self.scope.cancel();
        self.target.borrow_mut().close();
    }
}
fn add_session(system: &System<'static>, engine: &Engine, parent: &ScopeOwner,
               sessions: &mut Vec<SessionRow>, target: LaunchTarget, title: &str)
               -> Result<(), Error>
{
    let session = Session::open(system, target, title, sessions.len())?;
    sessions.push(SessionRow::new(*system, engine, parent, session));
    Ok(())
}
fn receive(system: &System<'static>, engine: &Engine, parent: &ScopeOwner,
           sessions: &mut Vec<SessionRow>)
           -> Result<(), Error>
{
    while let Some(batch) = system.processes().next_document_batch()?
    {
        for at in 0..batch.len()
        {
            let entry = batch.info(at)?;
            let title = entry.name().to_owned();
            add_session(system, engine, parent, sessions, LaunchTarget::File(entry), &title)?;
        }
        batch.finish()?;
    }
    Ok(())
}
fn command_sources(command: &Command<'_>, sources: &mut Vec<ReadySource>,
                   owners: &mut Vec<(Option<usize>, u8)>, owner: Option<usize>, queued: u8)
{
    if command.hangup_refusal_reported
    {
        return;
    }
    for (stream, reader) in command.readers.iter().enumerate()
    {
        if let Some(reader) = reader
           && queued & (1 << stream) == 0
        {
            sources.push(reader.readiness());
            owners.push((owner, 1 << stream));
        }
    }
    if let Some(writer) = &command.input
    {
        if !command.pending.is_empty() && queued & 4 == 0
        {
            let (first, _) = command.pending.as_slices();
            sources.push(writer.readiness(first.len().min(512)));
            owners.push((owner, 4));
        }
    }
    if command.readers.iter().all(Option::is_none) && queued & 8 == 0
    {
        sources.push(ReadySource { kind: CHILD,
                                   id: command.id.0,
                                   ..Default::default() });
        owners.push((owner, 8));
    }
}

fn main() -> Result<(), Error>
{
    let system = System::new()?;
    system.menus().clear()?;
    system.menus().install_menu_resource(ResourceScope::Application, 128)?;
    let mut location = None;
    let engine = Engine::new(&system)?;
    let app_scope = ScopeOwner::new(engine.notifier());
    let mut router = Router::default();
    let mut sessions = Vec::new();
    receive(&system, &engine, &app_scope, &mut sessions)?;
    if sessions.is_empty() && !system.processes().opened_for_documents()?
    {
        add_session(&system, &engine, &app_scope, &mut sessions, LaunchTarget::Shell, "Shelly")?;
    }
    system.log("TERMINAL_READY\n")?;
    let mut launcher = Launcher::default();
    let mut quitting = false;
    // Retain wait-set scratch capacity across input and cursor events.
    let mut sources = Vec::new();
    let mut owners = Vec::new();
    let mut ready = Vec::new();
    let mut timer = Vec::new();
    loop
    {
        // Only actual source changes and real cursor deadlines wake this loop.
        // Launches use a durable notifier; no fallback polling timer remains.
        engine.turn()?;
        sources.clear();
        owners.clear();
        for (index, row) in sessions.iter().enumerate()
        {
            let session = row.target.borrow();
            if let Some(command) = &session.command
            {
                command_sources(command,
                                &mut sources,
                                &mut owners,
                                Some(index),
                                if session.closing { 0 } else { row.io_queued.get() });
            }
            if !session.closing
               && session.focused
               && session.command.is_some()
               && !row.timer_queued.get()
            {
                if let Some(source) = session.timer.source()
                {
                    sources.push(source);
                    owners.push((Some(index), 0));
                }
            }
            if !row.registered.replace(true)
            {
                router.on(session.window.0, row.handler.clone())?;
            }
        }
        if let Some(command) = &launcher.retiring
        {
            command_sources(command, &mut sources, &mut owners, None, 0);
        }
        let (event, position) = engine.wait(&system, &mut sources, u32::MAX)?;
        if matches!(event, Event::Idle { .. })
        {
            router.prune();
        }
        ready.clear();
        ready.resize(sessions.len(), 0u8);
        let mut retiring_ready = 0u8;
        timer.clear();
        timer.resize(sessions.len(), false);
        for (source, (owner, mask)) in sources.iter().zip(&owners)
        {
            if source.ready != 0
            {
                if let Some(index) = owner
                {
                    if source.kind == TIMER
                    {
                        timer[*index] = true;
                    }
                    else
                    {
                        ready[*index] |= mask;
                    }
                }
                else
                {
                    retiring_ready |= mask;
                }
            }
        }
        // Closed windows have no live UI handler. Their EOF/drain/reap work is
        // explicitly app-owned and remains readiness-driven until real return.
        for (index, row) in sessions.iter().enumerate()
        {
            if ready[index] != 0
            {
                if row.target.borrow().closing
                {
                    let mut session = row.target.borrow_mut();
                    if let Some(command) = &mut session.command
                    {
                        if command.poll_hangup(&system, ready[index])
                        {
                            session.command = None;
                        }
                    }
                }
                else
                {
                    row.io_queued.set(row.io_queued.get() | ready[index]);
                    row.handler.send(SessionEvent::Ready(ready[index]))?;
                }
            }
            if timer[index]
            {
                row.timer_queued.set(true);
                row.handler.send(SessionEvent::Timer)?;
            }
        }
        engine.turn()?;
        if matches!(event, Event::DocumentsAvailable { .. })
        {
            quitting = false;
            receive(&system, &engine, &app_scope, &mut sessions)?;
        }
        let selection = system.menus().selection(&event)?;
        if let Some(selection) = selection
        {
            match selection.command
            {
                QUIT =>
                {
                    quitting = true;
                    if let Some(command) = &mut launcher.retiring
                    {
                        command.hang_up();
                    }
                    for row in &sessions
                    {
                        if row.target.borrow().closing
                        {
                            row.close();
                        }
                        else
                        {
                            row.handler.send(SessionEvent::Close)?;
                        }
                    }
                }
                NEW_SHELL =>
                {
                    quitting = false;
                    add_session(&system,
                                &engine,
                                &app_scope,
                                &mut sessions,
                                LaunchTarget::Shell,
                                "Shelly")?;
                }
                OPEN_COMMAND =>
                {
                    let initial = match location
                    {
                        Some(location) => location,
                        None =>
                        {
                            let volume = launch_defaults(&system)?.volume;
                            let directory =
                                system.files().lookup(volume, mac_toolbox::Cnid(0), CLI_FOLDER)?.id;
                            Location { volume,
                                       directory }
                        }
                    };
                    if let Some(choice) =
                        system.standard_file()
                              .standard_get_file(initial,
                                                 TypeFilter::Types(&[TypeCode::new(*b"CLAP")]))?
                    {
                        location = Some(choice.location());
                        add_session(&system,
                                    &engine,
                                    &app_scope,
                                    &mut sessions,
                                    LaunchTarget::File(choice.entry),
                                    choice.name())?;
                    }
                }
                _ => ()
            }
        }
        let send_eof = selection.is_some_and(|selection| selection.command == SEND_EOF);
        let key_target = if send_eof
                            || selection.is_none() && matches!(event, Event::KeyDown { .. })
        {
            system.window(WindowId(0)).ok().map(|info| info.id)
        }
        else
        {
            None
        };
        // Install completed launches before servicing this event's sessions.
        // A child's first output/query can then be consumed on its wake event.
        launcher.poll_completion(&system, &mut sessions, retiring_ready)?;
        let target = if send_eof
        {
            key_target
        }
        else
        {
            match &event
            {
                Event::CloseRequested { window,
                                        .. } => Some(*window),
                Event::Update { window,
                                .. }
                | Event::Activate { window,
                                  .. } => Some(*window),
                Event::MouseDown { window,
                                   .. } => *window,
                Event::KeyDown { .. } => key_target,
                Event::WindowGeometryChanged { window,
                                               .. } => Some(*window),
                _ => None
            }
        };
        if let Some(window) = target
        {
            router.route(&window.0, SessionEvent::Ui(event, position, key_target, send_eof))?;
        }
        engine.turn()?;
        for row in &sessions
        {
            if let Some(error) = row.error.borrow_mut().take()
            {
                if error == Error::NotFound
                {
                    row.close();
                }
                else
                {
                    return Err(error);
                }
            }
        }
        // Commit each new window's first frame before starting work that may
        // contend for resident services. Completions were installed above.
        launcher.start_pending(&system, &mut sessions, engine.notifier())?;
        sessions.retain(|row| {
                    let session = row.target.borrow();
                    !session.closing
                    || session.command.is_some()
                    || launcher.active
                               .as_ref()
                               .is_some_and(|(window, _, _)| *window == session.window)
                });
        if quitting
           && sessions.is_empty()
           && launcher.active.is_none()
           && launcher.retiring.is_none()
        {
            return Ok(());
        }
    }
}

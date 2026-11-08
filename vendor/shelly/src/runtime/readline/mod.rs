#[cfg(not(target_os = "systemrisc"))]
use std::io::IsTerminal;

use std::{
    any::Any,
    borrow::Cow,
    collections::{HashMap, VecDeque},
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex, MutexGuard, OnceLock, TryLockError,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    thread,
    time::Duration,
};

use nu_ansi_term::Style;

use reedline::{
    Color, ColumnarMenu, Completer, CompletionResult, EditCommand, EditMode, Emacs,
    ExternalPrinter, Highlighter, History, HistoryItem, HistoryItemId, HistorySessionId, InputMode,
    MenuBuilder, Prompt, PromptEditMode, PromptHistorySearch, Reedline, ReedlineEvent,
    ReedlineMenu, ReedlineRawEvent, SearchQuery, Signal, Span, StyledText, Suggestion,
    ValidationResult, Validator, Vi, default_emacs_keybindings, default_vi_insert_keybindings,
    default_vi_normal_keybindings, default_vi_visual_keybindings,
};

use tokio::sync::{mpsc as asynchronous, oneshot};

use crate::runtime::{color::{TtyColorMode, adapt_color}, history::ReplHistory};

pub(crate) mod output;
mod terminal;
pub(crate) use terminal::TerminalGuard;

static OWNER: OnceLock<Result<SyncSender<Command>, String>> = OnceLock::new();
static NEXT: AtomicU64 = AtomicU64::new(1);
static EDITING: AtomicBool = AtomicBool::new(false);
static MODE: Mutex<()> = Mutex::new(());
pub(crate) fn mode_lock() -> std::sync::MutexGuard<'static, ()>
{
    MODE.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn try_mode_lock() -> Option<MutexGuard<'static, ()>>
{
    match MODE.try_lock()
    {
        Ok(guard) => Some(guard),
        Err(TryLockError::Poisoned(error)) => Some(error.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }
}

pub(crate) fn editing() -> bool
{
    EDITING.load(Ordering::Acquire)
}

pub(crate) struct EditingLease;
impl EditingLease
{
    pub(crate) fn acquire() -> Result<Self, Fault>
    {
        if EDITING.swap(true, Ordering::AcqRel)
        {
            Err(Fault::new("BusyError", "Another terminal editor is active"))
        }
        else
        {
            Ok(Self)
        }
    }
}
impl Drop for EditingLease
{
    fn drop(&mut self)
    {
        EDITING.store(false, Ordering::Release);
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Fault
{
    pub kind: &'static str,
    pub message: String,
    pub code: Option<i32>,
    pub hook: Option<&'static str>,
    pub path: Option<String>,
    pub file: &'static str,
    pub line: u32,
    pub column: u32,
}
impl Fault
{
    #[track_caller]
    pub(crate) fn new(kind: &'static str, message: impl Into<String>) -> Self
    {
        let location = std::panic::Location::caller();
        Self {
            kind,
            message: message.into(),
            code: None,
            hook: None,
            path: None,
            file: location.file(),
            line: location.line(),
            column: location.column(),
        }
    }
    #[track_caller]
    pub(crate) fn io(kind: &'static str, error: impl std::error::Error + 'static) -> Self
    {
        let mut fault = Self::new(kind, error.to_string());
        fault.code = (&error as &dyn Any)
            .downcast_ref::<io::Error>()
            .and_then(io::Error::raw_os_error);
        fault
    }
}

#[derive(Clone, Default)]
pub(crate) struct Text(pub Vec<(Style, String)>);
impl Text
{
    pub(crate) fn plain(text: &str) -> Self
    {
        Self(vec![(Style::default(), visible(text))])
    }
    pub(crate) fn render(&self, mode: TtyColorMode) -> String
    {
        self.0
            .iter()
            .map(|(style, text)| {
                if mode == TtyColorMode::TtyMonochrome
                {
                    text.clone()
                }
                else
                {
                    let mut style = *style;
                    style.foreground = style
                        .foreground
                        .and_then(|color| adapt_color(color, mode));
                    style.background = style
                        .background
                        .and_then(|color| adapt_color(color, mode));
                    style.paint(text).to_string()
                }
            })
            .collect()
    }
    pub(crate) fn bytes(&self) -> usize
    {
        self.0.iter().map(|(_, text)| text.len()).sum()
    }
}

pub(crate) fn visible(text: &str) -> String
{
    let mut out = String::new();
    for character in text.chars()
    {
        match character
        {
            '\r' => out.push('\n'),
            '\n' | '\t' => out.push(character),
            value if value.is_control() => out.extend(value.escape_default()),
            value => out.push(value),
        }
    }
    out
}

pub(crate) struct PromptSnapshot
{
    pub left: Text,
    pub right: Text,
    pub continuation: Text,
    pub mode: TtyColorMode,
}
impl Prompt for PromptSnapshot
{
    fn render_prompt_left(&self) -> Cow<'_, str>
    {
        Cow::Owned(self.left.render(self.mode))
    }
    fn render_prompt_right(&self) -> Cow<'_, str>
    {
        Cow::Owned(self.right.render(self.mode))
    }
    fn render_prompt_indicator(&self, _: PromptEditMode) -> Cow<'_, str>
    {
        Cow::Borrowed("")
    }
    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str>
    {
        Cow::Owned(self.continuation.render(self.mode))
    }
    fn render_prompt_history_search_indicator(&self, _: PromptHistorySearch) -> Cow<'_, str>
    {
        Cow::Borrowed("search> ")
    }
    fn get_prompt_color(&self) -> Color
    {
        Color::Default
    }
    fn get_prompt_right_color(&self) -> Color
    {
        Color::Default
    }
    fn get_indicator_color(&self) -> Color
    {
        Color::Default
    }
    fn get_prompt_multiline_color(&self) -> Color
    {
        Color::Default
    }
}

#[derive(Clone)]
pub(crate) struct Limits
{
    pub input: usize,
    pub completions: usize,
    pub completion_bytes: usize,
    pub spans: usize,
    pub milliseconds: u64,
    pub instructions: usize,
}
impl Default for Limits
{
    fn default() -> Self
    {
        Self {
            input: 65536,
            completions: 1000,
            completion_bytes: 1048576,
            spans: 4096,
            milliseconds: 50,
            instructions: 100000,
        }
    }
}

pub(crate) struct Config
{
    pub manual_history: bool,
    pub complete: bool,
    pub validate: bool,
    pub highlight: bool,
    pub vi: bool,
    pub limits: Limits,
}

pub(crate) struct Control
{
    pub id: u64,
    pub closed: AtomicBool,
    pub busy: AtomicBool,
    pub ended: AtomicBool,
    pub cancel: AtomicBool,
    pub interrupt: AtomicBool,
    pub break_signal: Arc<AtomicBool>,
    pub finished: AtomicBool,
    pub restored: AtomicBool,
    pub exceeded: AtomicBool,
    pub revision: AtomicU64,
    pub draft: Mutex<String>,
    pub problem: Mutex<Option<String>>,
    pub path: Option<PathBuf>,
    pub history: Arc<Mutex<ReplHistory>>,
    pub strict_history: bool,
    pub warned: AtomicBool,
}
impl Control
{
    pub(crate) fn cancel(&self)
    {
        self.cancel.store(true, Ordering::Release);
        self.break_signal.store(true, Ordering::Release);
    }
    pub(crate) fn close(self: &Arc<Self>)
    {
        if !self.closed.swap(true, Ordering::AcqRel)
        {
            self.cancel();
            let _ = send(Command::Close(self.id));
        }
    }
    pub(crate) async fn sync(self: &Arc<Self>, strict: bool) -> Result<(), Fault>
    {
        if strict
            && !self.strict_history
            && let Some(problem) = self.problem.lock().unwrap().clone()
        {
            let mut fault = Fault::new("HistoryError", problem);
            fault.path = self.path.as_ref().map(|path| path.display().to_string());
            return Err(fault);
        }
        if self.path.is_none() || !self.strict_history && self.problem.lock().unwrap().is_some()
        {
            return Ok(());
        }
        let history = self.history.clone();
        let outcome = crate::runtime::blocking::run(move || history.lock().unwrap().sync())
            .await
            .map_err(|error| Fault::new("HistoryError", error.message))?;
        match outcome
        {
            Ok(()) =>
            {
                if self.strict_history
                {
                    *self.problem.lock().unwrap() = None;
                }
                Ok(())
            }
            Err(error) =>
            {
                *self.problem.lock().unwrap() = Some(error.to_string());
                if strict || self.strict_history
                {
                    let mut fault = Fault::io("HistoryError", error);
                    fault.path = self.path.as_ref().map(|path| path.display().to_string());
                    Err(fault)
                }
                else
                {
                    Ok(())
                }
            }
        }
    }
    pub(crate) fn warn(&self)
    {
        if !self.strict_history
            && !editing()
            && let Some(problem) = self.problem.lock().unwrap().as_ref()
            && !self.warned.swap(true, Ordering::AcqRel)
        {
            eprintln!("History unavailable; using in-memory history: {}.", problem);
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct Edit
{
    pub text: String,
    pub cursor: usize,
    pub revision: u64,
}
impl Edit
{
    pub(crate) fn word(&self) -> (usize, usize)
    {
        let start = self.text[..self.cursor]
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map_or(0, |(i, c)| i + c.len_utf8());
        let end = self.text[self.cursor..]
            .char_indices()
            .find(|(_, c)| c.is_whitespace())
            .map_or(self.text.len(), |(i, _)| self.cursor + i);
        (start, end)
    }
}
pub(crate) enum Answer
{
    Complete(Vec<Suggestion>),
    Highlight(Vec<(Span, Style)>),
    Validate
    {
        kind: usize,
        message: String,
        span: Option<Span>,
    },
    Stop,
}
pub(crate) enum Event
{
    Callback
    {
        hook: &'static str,
        edit: Edit,
        answer: SyncSender<Answer>,
    },
    Done(Result<(usize, String), Fault>),
}

type StyleCache = Option<(u64, Vec<(Span, Style)>)>;

struct Bridge
{
    events: asynchronous::UnboundedSender<Event>,
    control: Arc<Control>,
    view: Mutex<Edit>,
    validation: Mutex<Option<Span>>,
    limits: Limits,
    printer: SyncSender<String>,
    complete: bool,
    validate: bool,
    highlight: bool,
    submit: AtomicU64,
    decision: Mutex<Option<usize>>,
    menu: AtomicBool,
    mode: TtyColorMode,
    styles: Mutex<StyleCache>,
}
impl Bridge
{
    fn snapshot(&self, text: &str, cursor: usize) -> Edit
    {
        let mut view = self.view.lock().unwrap();
        if text.len() > self.limits.input
        {
            self.control.exceeded.store(true, Ordering::Release);
            self.control.cancel();
            return Edit::default();
        }
        if view.text != text || view.cursor != cursor
        {
            if view.text != text
            {
                *self.validation.lock().unwrap() = None;
            }
            view.revision = self.control.revision.fetch_add(1, Ordering::AcqRel) + 1;
            view.text = text.into();
            view.cursor = cursor;
        }
        *self.control.draft.lock().unwrap() = text.into();
        view.clone()
    }
    fn request(&self, hook: &'static str, edit: Edit) -> Answer
    {
        if edit.text.len() > self.limits.input
        {
            self.control.cancel();
            return Answer::Stop;
        }
        if self.control.cancel.load(Ordering::Acquire)
        {
            return Answer::Stop;
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        if self
            .events
            .send(Event::Callback {
                hook,
                edit,
                answer: sender,
            })
            .is_err()
        {
            self.control.cancel();
            return Answer::Stop;
        }
        loop
        {
            match receiver.recv_timeout(Duration::from_millis(10))
            {
                Ok(answer) => return answer,
                Err(mpsc::RecvTimeoutError::Timeout)
                    if !self.control.cancel.load(Ordering::Acquire) =>
                {}
                _ =>
                {
                    self.control.cancel();
                    return Answer::Stop;
                }
            }
        }
    }
}
struct CompletionAdapter(Arc<Bridge>);
impl Completer for CompletionAdapter
{
    fn complete(&mut self, text: &str, cursor: usize) -> CompletionResult
    {
        let edit = self.0.snapshot(text, cursor);
        let suggestions = if self.0.complete
        {
            match self.0.request("complete", edit)
            {
                Answer::Complete(items) => items,
                _ => Vec::new(),
            }
        }
        else
        {
            Vec::new()
        };
        self.0.menu.store(suggestions.len() > 1, Ordering::Release);
        CompletionResult::fresh(suggestions)
    }
}
struct FirstTabAdapter(Arc<Bridge>);
impl Completer for FirstTabAdapter
{
    fn complete(&mut self, text: &str, cursor: usize) -> CompletionResult
    {
        let result = CompletionAdapter(self.0.clone()).complete(text, cursor);
        let items = result.suggestions();
        self.0.menu.store(false, Ordering::Release);
        if items.len() == 1
        {
            return result;
        }
        let mut suggestion = Suggestion {
            span: Span::new(cursor, cursor),
            ..Suggestion::default()
        };
        if let Some(first) = items.first()
        {
            let mut prefix = first.value.clone();
            for item in &items[1..]
            {
                let bytes = prefix
                    .chars()
                    .zip(item.value.chars())
                    .take_while(|(left, right)| left == right)
                    .map(|(character, _)| character.len_utf8())
                    .sum();
                prefix.truncate(bytes);
            }
            if prefix.starts_with(&text[first.span.start..cursor])
                && prefix.len() > cursor - first.span.start
            {
                suggestion.span = first.span;
                suggestion.value = prefix;
            }
        }
        CompletionResult::fresh(vec![suggestion])
    }
}
struct SnapshotAdapter(Arc<Bridge>);
impl Completer for SnapshotAdapter
{
    fn complete(&mut self, text: &str, cursor: usize) -> CompletionResult
    {
        self.0.snapshot(text, cursor);
        CompletionResult::fresh(Vec::<Suggestion>::new())
    }
}
struct HighlightAdapter(Arc<Bridge>);
impl Highlighter for HighlightAdapter
{
    fn highlight(&self, text: &str, cursor: usize) -> StyledText
    {
        let edit = self.0.snapshot(text, cursor);
        let submit = self.0.submit.swap(0, Ordering::AcqRel);
        if submit != 0
        {
            // EditMode sees an entire key batch before Reedline applies its edits. Repaint
            // supplies the actual buffer/cursor, avoiding a second text buffer in the adapter.
            let decision = match self.0.request("validate", edit.clone())
            {
                Answer::Validate { kind: 1, .. } if submit != 2 => 1,
                Answer::Validate {
                    kind: 2,
                    message,
                    span,
                } =>
                {
                    *self.0.validation.lock().unwrap() = span;
                    let _ = self.0.printer.try_send(visible(&message));
                    2
                }
                Answer::Stop => 2,
                _ => 0,
            };
            *self.0.decision.lock().unwrap() = Some(decision);
            self.0.control.break_signal.store(true, Ordering::Release);
        }
        let mut spans = if self.0.highlight
        {
            let cached = self.0.styles.lock().unwrap().clone();
            if let Some((_, spans)) = cached.filter(|(revision, _)| *revision == edit.revision)
            {
                spans
            }
            else
            {
                let revision = edit.revision;
                let spans = match self.0.request("highlight", edit)
                {
                    Answer::Highlight(spans) => spans,
                    _ => Vec::new(),
                };
                *self.0.styles.lock().unwrap() = Some((revision, spans.clone()));
                spans
            }
        }
        else
        {
            Vec::new()
        };
        if let Some(span) = *self.0.validation.lock().unwrap()
        {
            spans = vec![(span, Style::new().fg(Color::Red).underline())];
        }
        let mut result = StyledText::new();
        let mut offset = 0;
        for (span, mut style) in spans
        {
            style.foreground = style
                .foreground
                .and_then(|color| adapt_color(color, self.0.mode));
            style.background = style
                .background
                .and_then(|color| adapt_color(color, self.0.mode));
            result.push((Style::default(), text[offset..span.start].into()));
            result.push((style, text[span.start..span.end].into()));
            offset = span.end;
        }
        result.push((Style::default(), text[offset..].into()));
        result
    }
}
struct ModeAdapter
{
    inner: Box<dyn EditMode>,
    bridge: Arc<Bridge>,
    manual: bool,
    previous_tab: bool,
    startup: Arc<Mutex<VecDeque<ReedlineRawEvent>>>,
}
impl EditMode for ModeAdapter
{
    fn edit_mode(&self) -> PromptEditMode
    {
        self.inner.edit_mode()
    }
    fn parse_event(&mut self, event: ReedlineRawEvent) -> ReedlineEvent
    {
        if self.startup.lock().unwrap().is_empty()
        {
            return self.parse_key(event);
        }
        let mut events = Vec::new();
        loop
        {
            let key = self.startup.lock().unwrap().pop_front();
            if let Some(key) = key
            {
                events.push(self.parse_key(key));
            }
            else
            {
                break;
            }
        }
        events.push(self.parse_key(event));
        ReedlineEvent::Multiple(events)
    }
}
impl ModeAdapter
{
    fn parse_key(&mut self, event: ReedlineRawEvent) -> ReedlineEvent
    {
        let event = self.inner.parse_event(event);
        let tab = matches!(&event, ReedlineEvent::UntilFound(events)
            if matches!(events.first(), Some(ReedlineEvent::Menu(name))
                if name == "completion_menu"));
        let first_tab = tab && !self.previous_tab;
        self.previous_tab = tab;
        if first_tab
        {
            return ReedlineEvent::UntilFound(vec![
                ReedlineEvent::Menu("first_tab".into()),
                ReedlineEvent::MenuNext,
            ]);
        }
        if matches!(event, ReedlineEvent::CtrlC | ReedlineEvent::CtrlD)
        {
            self.manual = false;
            self.bridge.menu.store(false, Ordering::Release);
        }
        if matches!(event, ReedlineEvent::CtrlC)
        {
            // A menu completer receives the current buffer after preceding batched edits.
            // Snapshot through that native interface before Reedline clears the draft.
            return ReedlineEvent::Multiple(vec![
                ReedlineEvent::Menu("__capture_draft".into()),
                ReedlineEvent::CtrlC,
            ]);
        }
        let force = matches!(event, ReedlineEvent::Submit);
        if force && !self.manual
        {
            self.manual = true;
            return ReedlineEvent::Repaint;
        }
        if matches!(event, ReedlineEvent::Enter) && self.manual
        {
            return ReedlineEvent::Edit(vec![EditCommand::InsertNewline]);
        }
        if force || matches!(event, ReedlineEvent::Enter)
        {
            if !force && self.bridge.menu.swap(false, Ordering::AcqRel)
            {
                return ReedlineEvent::Enter;
            }
            *self.bridge.validation.lock().unwrap() = None;
            if self.bridge.validate
            {
                self.bridge
                    .submit
                    .store(if force { 2 } else { 1 }, Ordering::Release);
                if force
                {
                    self.manual = false;
                }
                return ReedlineEvent::Repaint;
            }
            self.manual = false;
            return if force
            {
                ReedlineEvent::Submit
            }
            else
            {
                ReedlineEvent::Enter
            };
        }
        event
    }
}

struct ReadyValidator;
impl Validator for ReadyValidator
{
    fn validate(&self, _: &str) -> ValidationResult
    {
        ValidationResult::Complete
    }
}

// The native History object shares storage with the owning session, while automatic save
// is intercepted so persistence always happens after Reedline restores terminal settings.
struct HistoryAdapter
{
    history: Arc<Mutex<ReplHistory>>,
    manual: bool,
}
impl History for HistoryAdapter
{
    fn save(&mut self, item: HistoryItem) -> reedline::Result<HistoryItem>
    {
        if self.manual
        {
            Ok(HistoryItem::from_command_line(item.command_line))
        }
        else
        {
            self.history.lock().unwrap().save(item)
        }
    }
    fn load(&self, id: HistoryItemId) -> reedline::Result<HistoryItem>
    {
        self.history.lock().unwrap().load(id)
    }
    fn count(&self, query: SearchQuery) -> reedline::Result<i64>
    {
        self.history.lock().unwrap().count(query)
    }
    fn search(&self, query: SearchQuery) -> reedline::Result<Vec<HistoryItem>>
    {
        self.history.lock().unwrap().search(query)
    }
    fn update(
        &mut self,
        id: HistoryItemId,
        updater: &dyn Fn(HistoryItem) -> HistoryItem,
    ) -> reedline::Result<()>
    {
        self.history.lock().unwrap().update(id, updater)
    }
    fn delete(&mut self, id: HistoryItemId) -> reedline::Result<()>
    {
        self.history.lock().unwrap().delete(id)
    }
    fn clear(&mut self) -> reedline::Result<()>
    {
        self.history.lock().unwrap().clear()
    }
    fn session(&self) -> Option<HistorySessionId>
    {
        None
    }
    fn sync(&mut self) -> io::Result<()>
    {
        Ok(())
    }
}

struct Session
{
    editor: Reedline,
    history: Arc<Mutex<ReplHistory>>,
    config: Config,
    control: Arc<Control>,
    startup_input: VecDeque<ReedlineEvent>,
    startup_keys: Arc<Mutex<VecDeque<ReedlineRawEvent>>>,
}
pub(crate) enum Command
{
    Create
    {
        config: Config,
        control: Arc<Control>,
        reply: oneshot::Sender<Result<(), Fault>>,
    },
    Read
    {
        id: u64,
        prompt: PromptSnapshot,
        initial: String,
        cursor: usize,
        lease: EditingLease,
        startup: Vec<crossterm::event::Event>,
        events: asynchronous::UnboundedSender<Event>,
    },
    Remember
    {
        id: u64,
        text: String,
        reply: oneshot::Sender<Result<(), Fault>>,
    },
    Close(u64),
}

pub(crate) fn send(command: Command) -> Result<(), Fault>
{
    let sender = OWNER
        .get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel(32);
            thread::Builder::new()
                .name("shelly-readline".into())
                .spawn(move || {
                    let mut sessions: HashMap<u64, Session> = HashMap::new();
                    while let Ok(command) = receiver.recv()
                    {
                        let _maintenance = if !matches!(command, Command::Read { .. })
                        {
                            Some(mode_lock())
                        }
                        else
                        {
                            None
                        };
                        match command
                        {
                            Command::Create {
                                config,
                                control,
                                reply,
                            } =>
                            {
                                if sessions.len() >= 32
                                {
                                    let _ = reply.send(Err(Fault::new(
                                        "LimitError",
                                        "Editor registry is full",
                                    )));
                                }
                                else
                                {
                                    let history = control.history.clone();
                                    let editor = Reedline::create()
                                        .with_history(Box::new(HistoryAdapter {
                                            history: history.clone(),
                                            manual: config.manual_history,
                                        }))
                                        .use_kitty_keyboard_enhancement(false)
                                        .use_bracketed_paste(false)
                                        .with_validator(Box::new(ReadyValidator))
                                        .with_quick_completions(true)
                                        .with_partial_completions(true);
                                    sessions.insert(
                                        control.id,
                                        Session {
                                            editor,
                                            history,
                                            config,
                                            control,
                                            startup_input: VecDeque::new(),
                                            startup_keys: Arc::new(Mutex::new(VecDeque::new())),
                                        },
                                    );
                                    let _ = reply.send(Ok(()));
                                }
                            }
                            Command::Read {
                                id,
                                prompt,
                                initial,
                                cursor,
                                lease,
                                startup,
                                events,
                            } =>
                            {
                                if let Some(session) = sessions.get_mut(&id)
                                {
                                    let result = std::panic::catch_unwind(
                                        std::panic::AssertUnwindSafe(|| {
                                            session.read(
                                                prompt,
                                                initial,
                                                cursor,
                                                events.clone(),
                                                lease,
                                                startup,
                                            )
                                        }),
                                    )
                                    .unwrap_or_else(|_| {
                                        session.control.closed.store(true, Ordering::Release);
                                        Err(Fault::new("TerminalError", "Native editor failed"))
                                    });
                                    session.control.finished.store(true, Ordering::Release);
                                    session.control.restored.store(true, Ordering::Release);
                                    let _ = events.send(Event::Done(result));
                                }
                                else
                                {
                                    let _ = events.send(Event::Done(Err(Fault::new(
                                        "ClosedError",
                                        "Editor is closed",
                                    ))));
                                }
                            }
                            Command::Remember { id, text, reply } =>
                            {
                                let result = sessions
                                    .get_mut(&id)
                                    .ok_or_else(|| Fault::new("ClosedError", "Editor is closed"))
                                    .and_then(|session| {
                                        session
                                            .history
                                            .lock()
                                            .unwrap()
                                            .save(HistoryItem::from_command_line(text))
                                            .map_err(|error| Fault::io("HistoryError", error))?;
                                        Ok(())
                                    });
                                let _ = reply.send(result);
                            }
                            Command::Close(id) =>
                            {
                                sessions.remove(&id);
                            }
                        }
                        // Closing remains reliable even when its wake-up command could not
                        // enter a full queue. Closed control flags are the retirement authority.
                        let _retirement = if _maintenance.is_none()
                        {
                            Some(mode_lock())
                        }
                        else
                        {
                            None
                        };
                        sessions
                            .retain(|_, session| !session.control.closed.load(Ordering::Acquire));
                    }
                })
                .map_err(|error| error.to_string())?;
            Ok(sender)
        })
        .as_ref()
        .map_err(|message| Fault::new("TerminalError", message.clone()))?;
    sender
        .try_send(command)
        .map_err(|_| Fault::new("BusyError", "Editor owner is busy"))
}

pub(crate) fn control(path: Option<PathBuf>, limit: usize, strict_history: bool) -> Arc<Control>
{
    Arc::new(Control {
        id: NEXT.fetch_add(1, Ordering::Relaxed),
        closed: AtomicBool::new(false),
        busy: AtomicBool::new(false),
        ended: AtomicBool::new(false),
        cancel: AtomicBool::new(false),
        interrupt: AtomicBool::new(false),
        break_signal: Arc::new(AtomicBool::new(false)),
        finished: AtomicBool::new(true),
        restored: AtomicBool::new(true),
        exceeded: AtomicBool::new(false),
        revision: AtomicU64::new(0),
        draft: Mutex::new(String::new()),
        problem: Mutex::new(None),
        history: Arc::new(Mutex::new(
            ReplHistory::new(path.clone(), limit).bounded(16777216, !strict_history),
        )),
        strict_history,
        warned: AtomicBool::new(false),
        path,
    })
}

impl Session
{
    fn read(
        &mut self,
        prompt: PromptSnapshot,
        initial: String,
        cursor: usize,
        events: asynchronous::UnboundedSender<Event>,
        lease: EditingLease,
        startup: Vec<crossterm::event::Event>,
    ) -> Result<(usize, String), Fault>
    {
        if self.control.cancel.load(Ordering::Acquire)
            || self.control.closed.load(Ordering::Acquire)
        {
            return Err(Fault::new("CancelledError", "Read was cancelled"));
        }
        #[cfg(target_os = "systemrisc")]
        let attached = crate::runtime::terminal::attached();
        #[cfg(not(target_os = "systemrisc"))]
        let attached = io::stdin().is_terminal() && io::stderr().is_terminal();
        if !attached
        {
            return Err(Fault::new(
                "TerminalError",
                "NotTerminal: stdin and stderr must be terminals",
            ));
        }
        let terminal_mode = mode_lock();
        let guard = TerminalGuard::new()?;
        let printer = ExternalPrinter::new(8);
        let bridge = Arc::new(Bridge {
            events,
            control: self.control.clone(),
            view: Mutex::new(Edit {
                revision: self.control.revision.fetch_add(1, Ordering::AcqRel) + 1,
                ..Edit::default()
            }),
            validation: Mutex::default(),
            limits: self.config.limits.clone(),
            printer: printer.sender(),
            complete: self.config.complete,
            validate: self.config.validate,
            highlight: self.config.highlight,
            submit: AtomicU64::new(0),
            decision: Mutex::default(),
            menu: AtomicBool::new(false),
            mode: prompt.mode,
            styles: Mutex::default(),
        });
        let mut keys = default_emacs_keybindings();
        crate::runtime::repl::Repl::apply_keybindings(&mut keys);
        let mut startup_mode = Emacs::new(keys.clone());
        for event in startup
        {
            let Ok(raw) = ReedlineRawEvent::try_from(event.clone())
            else
            {
                continue;
            };
            match startup_mode.parse_event(raw)
            {
                event @ (ReedlineEvent::Edit(_)
                | ReedlineEvent::Enter
                | ReedlineEvent::CtrlC
                | ReedlineEvent::CtrlD) => self.startup_input.push_back(event),
                _ =>
                {
                    if let Ok(raw) = ReedlineRawEvent::try_from(event)
                    {
                        self.startup_keys.lock().unwrap().push_back(raw);
                    }
                }
            }
        }
        let mode: Box<dyn EditMode> = if self.config.vi
        {
            let mut insert = default_vi_insert_keybindings();
            crate::runtime::repl::Repl::apply_keybindings(&mut insert);
            Box::new(Vi::new(
                insert,
                default_vi_normal_keybindings(),
                default_vi_visual_keybindings(),
            ))
        }
        else
        {
            Box::new(Emacs::new(keys))
        };
        // Builder methods consume self; retain one native editor and its editing/history state.
        let placeholder = Reedline::create();
        let editor = std::mem::replace(&mut self.editor, placeholder);
        self.editor = editor
            .with_completer(Box::new(CompletionAdapter(bridge.clone())))
            .with_highlighter(Box::new(HighlightAdapter(bridge.clone())))
            .with_edit_mode(Box::new(ModeAdapter {
                inner: mode,
                bridge: bridge.clone(),
                manual: false,
                previous_tab: false,
                startup: self.startup_keys.clone(),
            }))
            .with_break_signal(self.control.break_signal.clone())
            .with_external_printer(printer)
            .with_poll_interval(Duration::from_millis(20))
            .with_ansi_colors(prompt.mode != TtyColorMode::TtyMonochrome)
            .clear_menus()
            .with_menu(ReedlineMenu::EngineCompleter(Box::new(
                ColumnarMenu::default()
                    .with_name("completion_menu")
                    .with_input_mode(InputMode::FullBuffer),
            )))
            .with_menu(ReedlineMenu::WithCompleter {
                menu: Box::new(
                    ColumnarMenu::default()
                        .with_name("first_tab")
                        .with_input_mode(InputMode::FullBuffer),
                ),
                completer: Box::new(FirstTabAdapter(bridge.clone())),
            })
            .with_menu(ReedlineMenu::WithCompleter {
                menu: Box::new(
                    ColumnarMenu::default()
                        .with_name("__capture_draft")
                        .with_input_mode(InputMode::FullBuffer),
                ),
                completer: Box::new(SnapshotAdapter(bridge.clone())),
            });
        self.editor.run_edit_commands(&[
            EditCommand::Clear,
            EditCommand::InsertString(initial),
            EditCommand::MoveToPosition {
                position: cursor,
                select: false,
            },
        ]);
        guard.enable_protocols()?;
        while let Some(event) = self.startup_input.pop_front()
        {
            match event
            {
                ReedlineEvent::Edit(commands) => self.editor.run_edit_commands(&commands),
                ReedlineEvent::Enter =>
                {
                    if self.config.validate
                    {
                        bridge.submit.store(1, Ordering::Release);
                    }
                    else
                    {
                        let editor = std::mem::replace(&mut self.editor, Reedline::create());
                        self.editor = editor.with_immediately_accept(true);
                    }
                    break;
                }
                ReedlineEvent::CtrlC =>
                {
                    return Ok((1, self.editor.current_buffer_contents().into()));
                }
                ReedlineEvent::CtrlD if self.editor.current_buffer_contents().is_empty() =>
                {
                    self.control.ended.store(true, Ordering::Release);
                    return Ok((2, String::new()));
                }
                _ =>
                {}
            }
        }
        let result = loop
        {
            let result = self.editor.read_line(&prompt);
            if matches!(&result, Ok(Signal::ExternalBreak(_)))
                && !self.control.cancel.load(Ordering::Acquire)
                && let Some(decision) = bridge.decision.lock().unwrap().take()
            {
                if decision == 1
                {
                    self.editor.run_edit_commands(&[EditCommand::InsertNewline]);
                }
                let placeholder = Reedline::create();
                let editor = std::mem::replace(&mut self.editor, placeholder);
                self.editor = editor.with_immediately_accept(decision == 0);
                continue;
            }
            break result;
        };
        let placeholder = Reedline::create();
        let editor = std::mem::replace(&mut self.editor, placeholder);
        self.editor = editor.with_immediately_accept(false);
        let restored = guard.restore();
        drop(terminal_mode);
        drop(lease);
        self.control.restored.store(true, Ordering::Release);
        restored?;
        if self.control.exceeded.load(Ordering::Acquire)
        {
            return Err(Fault::new("LimitError", "Input exceeds max_input_bytes"));
        }
        let (kind, text) = match result.map_err(|error| Fault::io("TerminalError", error))?
        {
            Signal::Success(text) => (0, text),
            Signal::CtrlC => (1, self.control.draft.lock().unwrap().clone()),
            Signal::CtrlD => (2, String::new()),
            Signal::ExternalBreak(text) if self.control.interrupt.load(Ordering::Acquire) =>
            {
                (1, text)
            }
            Signal::ExternalBreak(text) =>
            {
                *self.control.draft.lock().unwrap() = text;
                return Err(Fault::new("CancelledError", "Read was cancelled"));
            }
            _ => return Err(Fault::new("TerminalError", "Unsupported editor signal")),
        };
        if text.len() > self.config.limits.input
        {
            return Err(Fault::new("LimitError", "Input exceeds max_input_bytes"));
        }
        if kind == 0
        {
            *self.control.draft.lock().unwrap() = text.clone();
        }
        if kind != 1
        {
            self.control.draft.lock().unwrap().clear();
        }
        if kind == 2
        {
            self.control.ended.store(true, Ordering::Release);
        }
        Ok((kind, text))
    }
}

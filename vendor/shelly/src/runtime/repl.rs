use std::{borrow::Cow,
          collections::{BTreeMap, BTreeSet, VecDeque},
          env::{current_dir, home_dir},
          fs::read_dir,
          mem::take,
          path::{Path, PathBuf},
          process::ExitCode,
          sync::{Arc, Mutex,
                 atomic::{AtomicBool, Ordering}},
          time::{Duration, Instant}};

use reedline::{Color, ColumnarMenu, Completer, CompletionResult, EditCommand, EditMode, Emacs,
               InputMode, KeyCode, KeyModifiers, Keybindings, MenuBuilder, Prompt, PromptEditMode,
               PromptHistorySearch, Reedline, ReedlineEvent, ReedlineMenu, ReedlineRawEvent,
               Signal, Span, Suggestion, default_emacs_keybindings};
#[cfg(unix)]
use rustix::process::geteuid;

use crate::{language::{analysis::SymbolSnapshot,
                       interpreter::{Interactive, Interpreter, RcFile, Startup},
                       text::buffer::SimpleBuffer},
            location_here,
            runtime::{color::TtyColorMode,
                      highlighting::ShellyHighlighter,
                      history::{self, DEFAULT_LIMIT, ReplHistory},
                      paths::split_paths,
                      result::RuntimeResult,
                      signals::take_interrupt}};

struct ShellyPrompt
{
    prompt_text: String,
    multiline: Arc<AtomicBool>,
    color_mode: TtyColorMode
}


impl ShellyPrompt
{
    fn multiline_color(&self, normal: Color) -> Color
    {
        if    self.multiline.load(Ordering::Relaxed)
           && matches!(self.color_mode, TtyColorMode::Tty256 | TtyColorMode::TtyTrueColor)
        { Color::Yellow }
        else { normal }
    }
}


impl Prompt for ShellyPrompt
{
    fn render_prompt_left(&self) -> Cow<'_, str>
    {
        Cow::Owned(self.prompt_text.clone())
    }

    fn render_prompt_right(&self) -> Cow<'_, str>
    {
        Cow::Borrowed("")
    }

    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> Cow<'_, str>
    {
        #[cfg(unix)]
        if geteuid().is_root()
        {
            return Cow::Borrowed("# ");
        }

        Cow::Borrowed("$ ")
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str>
    {
        Cow::Borrowed("> ")
    }

    fn render_prompt_history_search_indicator(&self, _search: PromptHistorySearch) -> Cow<'_, str>
    {
        Cow::Borrowed("search> ")
    }

    fn get_prompt_color(&self) -> Color
    {
        Color::LightGray
    }

    fn get_indicator_color(&self) -> Color
    {
        self.multiline_color(Color::LightGray)
    }

    fn get_prompt_multiline_color(&self) -> Color
    {
        self.multiline_color(Color::DarkGray)
    }
}


#[derive(Clone)]
struct ShellyCompleter
{
    symbols: Arc<SymbolSnapshot>,
    commands: Arc<Mutex<BTreeSet<String>>>,
    search_path: Vec<PathBuf>,
    home: Option<PathBuf>,
    variables: Vec<String>
}


// Return only the variable reference at the cursor, preserving surrounding text.
fn completion_variable(line: &str, pos: usize) -> Option<(Span, &str, bool)>
{
    let before = line.get(..pos)?;
    let mut start = None;
    let mut quote = None;
    let mut escaped = false;
    let mut comment = false;

    for (index, character) in before.char_indices()
    {
        if comment
        {
            if character == '\n' { comment = false; }
            continue;
        }
        if escaped
        {
            escaped = false;
            continue;
        }
        if character == '\\' && quote.is_some()
        {
            escaped = true;
            continue;
        }
        if let Some(delimiter) = quote
        {
            if character == delimiter
            {
                quote = None;
                start = None;
                continue;
            }
            if delimiter == '\'' { continue; }
        }
        else if matches!(character, '\'' | '"')
        {
            quote = Some(character);
            start = None;
            continue;
        }
        else if character == '#'
        {
            comment = true;
            start = None;
            continue;
        }

        if character == '$' { start = Some(index); }
    }

    if comment || escaped || quote == Some('\'') { return None; }
    let start = start?;
    let reference = &before[start + 1..];
    let braced = reference.starts_with('{');
    let prefix = if braced { &reference[1..] } else { reference };
    let name_character = |c: char| c.is_alphanumeric() || c == '_';
    if !prefix.chars().all(name_character) { return None; }

    let mut end = pos;
    for character in line[pos..].chars().take_while(|c| name_character(*c))
    {
        end += character.len_utf8();
    }
    if braced && line[end..].starts_with('}') { end += 1; }

    Some((Span::new(start, end), prefix, braced))
}


// Find the word at the cursor without treating spaces inside quotes as separators.
fn completion_word(line: &str, pos: usize) -> Option<(Span, String, bool)>
{
    let before = line.get(..pos)?;
    let mut start = 0;
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut command = true;
    let mut comment = false;

    for (index, character) in before.char_indices()
    {
        if comment && character != '\n' { continue; }
        if escaped
        {
            word.push(match character { 'n' => '\n', 'r' => '\r', 't' => '\t', _ => character });
            escaped = false;
        }
        else if let Some(delimiter) = quote
        {
            if character == '\\' { escaped = true; }
            else if character == delimiter { quote = None; }
            else { word.push(character); }
        }
        else if matches!(character, '\'' | '"')
        {
            quote = Some(character);
        }
        else if character == '#'
        {
            comment = true;
        }
        else if matches!(character, ';' | '|' | '\n' | '{' | '}')
        {
            start = index + character.len_utf8();
            word.clear();
            command = true;
            comment = false;
        }
        else if character.is_whitespace()
        {
            if start < index { command = false; }
            start = index + character.len_utf8();
            word.clear();
        }
        else
        {
            word.push(character);
        }
    }

    if comment || escaped { return None; }

    // Replace the rest of the word too when completing in the middle of a line.
    let mut end = pos;
    for character in line[pos..].chars()
    {
        if escaped { escaped = false; }
        else if let Some(delimiter) = quote
        {
            if character == '\\' { escaped = true; }
            else if character == delimiter { quote = None; }
        }
        else if matches!(character, '\'' | '"') { quote = Some(character); }
        else if character.is_whitespace() || matches!(character, ';' | '|' | '{' | '}' | '#')
        {
            break;
        }
        end += character.len_utf8();
    }

    Some((Span::new(start, end), word, command))
}


fn completion_text(path: &str, directory: bool) -> String
{
    // Single quotes protect literal dollars and glob characters in filenames.
    if !path.is_empty() && path.chars().all(|c| c.is_alphanumeric() || "_./-".contains(c))
    {
        return path.to_string();
    }

    let escaped = path.replace('\\', "\\\\").replace('\'', "\\'")
        .replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t");
    // Leave directory quotes open so the next component can be typed inside them.
    format!("'{}{}", escaped, if directory { "" } else { "'" })
}


fn is_executable(path: &Path) -> bool
{
    let Ok(metadata) = path.metadata() else { return false; };
    if !metadata.is_file() { return false; }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}


impl Completer for ShellyCompleter
{
    fn complete(&mut self, line: &str, pos: usize) -> CompletionResult
    {
        if let Some((span, prefix, braced)) = completion_variable(line, pos)
        {
            return CompletionResult::fresh(self.variables.iter().filter_map(|variable|
                {
                    let name = variable.strip_prefix('$')?;
                    if !name.starts_with(prefix) { return None; }
                    if !braced && !name.chars().all(|c| c.is_alphanumeric() || c == '_')
                    {
                        return None;
                    }
                    Some(Suggestion
                        {
                            value: if braced { format!("${{{}}}", name) } else { variable.clone() },
                            span,
                            append_whitespace: false,
                            ..Suggestion::default()
                        })
                }).collect::<Vec<_>>());
        }

        let Some((span, word, command)) = completion_word(line, pos) else
        {
            return CompletionResult::fresh(Vec::new());
        };

        let mut matches = BTreeMap::new();

        if command && !word.contains('/')
        {
            // Discover matching names only on an explicit completion request. Scanning
            // every executable before each prompt is particularly costly on WSL mounts.
            let mut discovered = BTreeSet::new();
            for directory in &self.search_path
            {
                let directory = if directory.as_os_str().is_empty() { Path::new(".") }
                    else { directory.as_path() };
                let Ok(entries) = read_dir(directory) else { continue; };
                for entry in entries.flatten()
                {
                    let Ok(name) = entry.file_name().into_string() else { continue; };
                    if name.starts_with(&word) && is_executable(&entry.path())
                    {
                        matches.insert(name.clone(), false);
                        discovered.insert(name);
                    }
                }
            }
            self.commands.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
                .extend(discovered);
        }

        // Expand home prefixes while resolving completion candidates.
        let path = if    word == "~"
                      || word.starts_with("~/")
                      || word == "$HOME"
                      || word.starts_with("$HOME/")
            {
                let Some(home) = &self.home else { return CompletionResult::fresh(Vec::new()); };
                let suffix = word
                    .strip_prefix('~')
                    .unwrap_or_else(|| word.strip_prefix("$HOME").unwrap());
                format!("{}/{}", home.display(), suffix.trim_start_matches('/'))
            }
            else
            {
                word.clone()
            };

        let (prefix, filename) = path.rfind('/').map_or(("", path.as_str()),
            |index| (&path[..=index], &path[index + 1..]));
        let directory = if prefix.is_empty() { Path::new(".") } else { Path::new(prefix) };

        if let Ok(entries) = read_dir(directory)
        {
            for entry in entries.flatten()
            {
                let Ok(name) = entry.file_name().into_string() else { continue; };
                if    !name.starts_with(filename)
                   || (name.starts_with('.') && !filename.starts_with('.'))
                {
                    continue;
                }

                let directory = entry.path().is_dir();
                let local_prefix = if command && prefix.is_empty() { "./" } else { prefix };
                let value = format!("{}{}{}", local_prefix, name, if directory { "/" } else { "" });
                matches.insert(value, directory);
            }
        }

        CompletionResult::fresh(matches.into_iter().map(|(path, directory)| Suggestion
            {
                value: completion_text(&path, directory),
                display_override: Some(path),
                span,
                append_whitespace: !directory,
                ..Suggestion::default()
            }).collect::<Vec<_>>())
    }
}


struct FirstTabCompleter(ShellyCompleter);

impl Completer for FirstTabCompleter
{
    fn complete(&mut self, line: &str, pos: usize) -> CompletionResult
    {
        let result = self.0.complete(line, pos);
        let suggestions = result.suggestions();
        if suggestions.len() == 1 { return result; }

        // A single suggestion is accepted silently by Reedline's quick completion.
        // With no extension to offer, accept a no-op rather than opening a menu.
        let mut suggestion = Suggestion
            {
                span: Span::new(pos, pos),
                ..Suggestion::default()
            };

        if let Some(first) = suggestions.first()
        {
            let mut prefix = first.display_value().to_string();
            for other in &suggestions[1..]
            {
                let shared_bytes = prefix.chars().zip(other.display_value().chars())
                    .take_while(|(left, right)| left == right)
                    .map(|(character, _)| character.len_utf8()).sum();
                prefix.truncate(shared_bytes);
            }

            if !prefix.is_empty()
            {
                suggestion.span = first.span;
                if let Some((_, _, braced)) = completion_variable(line, pos)
                {
                    // Keep an existing closing brace beyond the cursor on partial completion.
                    if braced && line[..first.span.end].ends_with('}') && !prefix.ends_with('}')
                    {
                        suggestion.span.end -= 1;
                    }
                    suggestion.value = prefix;
                }
                else
                {
                    // A partial path may still need more characters inside its quotes.
                    suggestion.value = completion_text(&prefix, true);
                }
            }
        }

        CompletionResult::fresh(vec![suggestion])
    }
}


struct ShellyEditMode
{
    emacs: Emacs,
    previous_tab: bool,
    multiline: Arc<AtomicBool>,
    startup_keys: Arc<Mutex<VecDeque<ReedlineRawEvent>>>,
}

impl ShellyEditMode
{
    fn parse_key(&mut self, event: ReedlineRawEvent) -> ReedlineEvent
    {
        let event = self.emacs.parse_event(event);
        if matches!(event, ReedlineEvent::Submit)
        {
            self.previous_tab = false;
            let submitting = self.multiline.fetch_xor(true, Ordering::Relaxed);
            // Close completion menus so the second press always submits the buffer.
            return ReedlineEvent::Multiple(vec![
                    ReedlineEvent::Esc,
                    if submitting { ReedlineEvent::Submit } else { ReedlineEvent::Repaint }
                ]);
        }
        if matches!(event, ReedlineEvent::CtrlC)
        { self.multiline.store(false, Ordering::Relaxed); }
        if matches!(event, ReedlineEvent::Enter) && self.multiline.load(Ordering::Relaxed)
        {
            self.previous_tab = false;
            return ReedlineEvent::Edit(vec![EditCommand::InsertNewline]);
        }
        let tab = matches!(&event, ReedlineEvent::UntilFound(events)
            if matches!(events.first(), Some(ReedlineEvent::Menu(name))
                if name == "completion_menu"));
        let first_tab = tab && !self.previous_tab;
        self.previous_tab = tab;

        if first_tab
        {
            ReedlineEvent::UntilFound(vec![
                    ReedlineEvent::Menu("first_tab".to_string()),
                    ReedlineEvent::MenuNext
                ])
        }
        else { event }
    }
}

impl EditMode for ShellyEditMode
{
    fn parse_event(&mut self, event: ReedlineRawEvent) -> ReedlineEvent
    {
        let mut pending = take(&mut *self.startup_keys.lock().unwrap());
        if pending.is_empty() { return self.parse_key(event); }
        let mut events = Vec::new();
        while let Some(key) = pending.pop_front()
        { events.push(self.parse_key(key)); }
        events.push(self.parse_key(event));
        ReedlineEvent::Multiple(events)
    }

    fn edit_mode(&self) -> PromptEditMode
    {
        self.emacs.edit_mode()
    }
}


pub struct Repl
{
    tab_width: Option<usize>,
    color_mode: TtyColorMode,
    interpreter: Interpreter
}


impl Repl
{
    pub fn new(color_mode: TtyColorMode,
               tab_width: Option<usize>,
               startup: Startup,
               suppress_banner: bool,
               rc_file: RcFile,
               script_args: Vec<String>) -> Self
    {
        let interactive_mode = if suppress_banner
            {
                Interactive::YesWithoutBanner
            }
            else
            {
                Interactive::Yes
            };

        let interpreter = Interpreter::new(startup,
                                           interactive_mode,
                                           color_mode,
                                           tab_width,
                                           rc_file,
                                           script_args);

        Self
            {
                tab_width,
                color_mode,
                interpreter
            }
    }

    pub fn run(&mut self) -> RuntimeResult<ExitCode>
    {
        let initialization = crate::runtime::readline::mode_lock();
        let multiline = Arc::new(AtomicBool::new(false));
        let mut prompt = ShellyPrompt
            {
                prompt_text: String::new(),
                multiline: multiline.clone(),
                color_mode: self.color_mode
            };
        // Preserve Reedline's complete editing/history/search bindings, then
        // apply Shelly's language and completion overrides.
        let mut keybindings = default_emacs_keybindings();

        Self::apply_keybindings(&mut keybindings);

        let mut startup_mode = Emacs::new(keybindings.clone());
        let mut startup_input = VecDeque::new();
        let startup_keys = Arc::new(Mutex::new(VecDeque::new()));
        for event in self.interpreter.take_startup_input()
        {
            let Ok(key) = ReedlineRawEvent::try_from(event.clone()) else { continue; };
            let parsed = startup_mode.parse_event(key);
            match parsed
            {
                ReedlineEvent::Edit(_) | ReedlineEvent::Enter | ReedlineEvent::CtrlC
                    | ReedlineEvent::CtrlD => startup_input.push_back(parsed),
                // Let Reedline itself handle menus, history search and multiline controls.
                _ =>
                    {
                        if let Ok(key) = ReedlineRawEvent::try_from(event)
                        { startup_keys.lock().unwrap().push_back(key); }
                    },
            }
        }

        let commands = Arc::new(Mutex::new(BTreeSet::new()));
        let mut command_context = None;
        let mut history_warned = false;
        let (history_path, history_limit) = self.interpreter.history_settings()
            .unwrap_or_else(|error|
                {
                    history::report_error(error, &mut history_warned);
                    (None, DEFAULT_LIMIT)
                });
        crate::startup_trace::mark("reedline_setup");
        let mut editor = Reedline::create()
            .with_history(Box::new(ReplHistory::new(history_path, history_limit)))
            .with_ansi_colors(self.color_mode != TtyColorMode::TtyMonochrome)
            .use_kitty_keyboard_enhancement(true)
            .with_quick_completions(true)
            .with_edit_mode(Box::new(ShellyEditMode
                {
                    emacs: Emacs::new(keybindings),
                    previous_tab: false,
                    multiline: multiline.clone(),
                    startup_keys: startup_keys.clone(),
                }));
        drop(initialization);

        while !self.interpreter.halted
        {
            take_interrupt();
            history::sync(&mut editor, &mut history_warned);
            // Reset after submission, cancellation, or a read error.
            multiline.store(false, Ordering::Relaxed);
            let prompt_text = self.interpreter.evaluate_prompt(location_here!())
                .ok().flatten()
                .unwrap_or_else(|| self.interpreter.default_prompt());
            if self.interpreter.halted { break; }

            prompt.prompt_text = prompt_text;

            // Refresh after each command so changes to PATH, HOME, and cwd are respected.
            let search_path = self.interpreter.evaluate_path_variable("$PATH").unwrap_or_default();
            let home = self.interpreter.evaluate_path_variable("$HOME").ok()
                .filter(|home| !home.is_empty()).map(PathBuf::from).or_else(home_dir);
            let mut symbols = self.interpreter.symbol_snapshot();
            let context = (search_path.clone(), current_dir().ok());
            let mut names = commands.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if command_context.as_ref() != Some(&context)
            {
                names.clear();
                command_context = Some(context);
            }
            symbols.external_commands = names.clone();
            drop(names);
            let completer = ShellyCompleter
                {
                    symbols: Arc::new(symbols),
                    commands: commands.clone(),
                    search_path: split_paths(&search_path).collect(),
                    home,
                    variables: self.interpreter.variable_names()
                };
            editor = editor.clear_menus()
                .with_highlighter(Box::new(ShellyHighlighter::new(
                    completer.symbols.clone(), self.interpreter.syntax_settings())))
                .with_completer(Box::new(completer.clone()))
                .with_menu(ReedlineMenu::EngineCompleter(Box::new(
                    ColumnarMenu::default().with_name("completion_menu")
                        .with_input_mode(InputMode::FullBuffer))))
                .with_menu(ReedlineMenu::WithCompleter
                    {
                        menu: Box::new(ColumnarMenu::default().with_name("first_tab")
                            .with_input_mode(InputMode::FullBuffer)),
                        completer: Box::new(FirstTabCompleter(completer))
                    });

            // A key typed during the capability query precedes the keys still in the TTY.
            // Runtime cursor queries can also preserve keys after the REPL has started.
            for event in self.interpreter.take_startup_input()
            {
                let Ok(key) = ReedlineRawEvent::try_from(event.clone()) else { continue; };
                let Ok(raw) = ReedlineRawEvent::try_from(event) else { continue; };
                let parsed = startup_mode.parse_event(key);
                match parsed
                {
                    ReedlineEvent::Edit(_) | ReedlineEvent::Enter | ReedlineEvent::CtrlC
                        | ReedlineEvent::CtrlD => startup_input.push_back(parsed),
                    _ => startup_keys.lock().unwrap().push_back(raw),
                }
            }
            let mut startup_signal = None;
            let mut accept_startup = false;
            while let Some(event) = startup_input.pop_front()
            {
                match event
                {
                    ReedlineEvent::Edit(commands) => editor.run_edit_commands(&commands),
                    ReedlineEvent::Enter => { accept_startup = true; break; },
                    ReedlineEvent::CtrlC => { startup_signal = Some(Signal::CtrlC); break; },
                    ReedlineEvent::CtrlD => { startup_signal = Some(Signal::CtrlD); break; },
                    _ => {},
                }
            }
            editor = editor.with_immediately_accept(accept_startup);
            let input = if let Some(signal) = startup_signal { Ok(signal) }
                else
                {
                    let _lease = loop
                    {
                        if let Ok(lease) = crate::runtime::readline::EditingLease::acquire()
                        { break lease; }
                        std::thread::sleep(Duration::from_millis(5));
                    };
                    let _mode = crate::runtime::readline::mode_lock();
                    crate::startup_trace::finish();
                    editor.read_line(&prompt)
                };
            editor = editor.with_immediately_accept(false);
            if !matches!(&input, Ok(Signal::Success(text)) if !text.trim().is_empty())
            {
                self.interpreter.record_command_time(Duration::ZERO);
            }
            match input
            {
                Ok(Signal::Success(text)) =>
                    {
                        if text.trim().is_empty() { continue; }
                        // Persist submitted text before execution, including failing or
                        // interrupted commands. Startup scripts never enter this history.
                        history::sync(&mut editor, &mut history_warned);
                        let mut buffer = SimpleBuffer::new("<repl>", &text, self.tab_width);
                        let started = Instant::now();
                        let result = self.interpreter.execute_from_buffer(&mut buffer);
                        self.interpreter.record_command_time(started.elapsed());

                        if let Err(error) = result
                        {
                            self.interpreter.report_repl_error(&error);
                            continue;
                        }

                        if self.interpreter.halted
                        {
                            break;
                        }
                    },

                Ok(Signal::CtrlC) =>
                    {
                        println!("Ctrl+C pressed.");
                    }

                Ok(Signal::CtrlD) => break,
                Err(error) if matches!(error.kind(), std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::BrokenPipe) => break,

                Ok(_) =>
                    {
                        println!("Unhandled signal received.");
                    }

                Err(error) =>
                    {
                        println!("Error reading line: {:?}.", error);
                    }
            }
        }

        history::sync(&mut editor, &mut history_warned);
        Ok(ExitCode::from(self.interpreter.exit_code))
    }

    pub(crate) fn apply_keybindings(keybindings: &mut Keybindings)
    {
        fn simple(command: EditCommand) -> ReedlineEvent
        {
            ReedlineEvent::Edit(vec![command])
        }

        fn binding(keybindings: &mut Keybindings, key_code: KeyCode, event: ReedlineEvent)
        {
            keybindings.add_binding(KeyModifiers::NONE, key_code, event);
        }

        fn ctrl_binding(keybindings: &mut Keybindings, key_code: KeyCode, event: ReedlineEvent)
        {
            keybindings.add_binding(KeyModifiers::CONTROL, key_code, event);
        }

        fn shift_binding(keybindings: &mut Keybindings, key_code: KeyCode, event: ReedlineEvent)
        {
            keybindings.add_binding(KeyModifiers::SHIFT, key_code, event);
        }

        binding(keybindings, KeyCode::Enter, ReedlineEvent::Enter);
        binding(keybindings, KeyCode::Tab, ReedlineEvent::UntilFound(vec![
                ReedlineEvent::Menu("completion_menu".to_string()),
                ReedlineEvent::MenuNext
            ]));
        shift_binding(keybindings, KeyCode::BackTab, ReedlineEvent::MenuPrevious);

        binding(keybindings, KeyCode::Left, ReedlineEvent::UntilFound(vec![
                ReedlineEvent::MenuLeft, ReedlineEvent::Left
            ]));
        binding(keybindings, KeyCode::Right, ReedlineEvent::UntilFound(vec![
                ReedlineEvent::MenuRight, ReedlineEvent::Right
            ]));
        binding(keybindings, KeyCode::Up, ReedlineEvent::UntilFound(vec![
                ReedlineEvent::MenuUp, ReedlineEvent::Up
            ]));
        binding(keybindings, KeyCode::Down, ReedlineEvent::UntilFound(vec![
                ReedlineEvent::MenuDown, ReedlineEvent::Down
            ]));

        binding(keybindings, KeyCode::Backspace, simple(EditCommand::Backspace));
        binding(keybindings, KeyCode::Delete, simple(EditCommand::Delete));
        binding(keybindings, KeyCode::Home, simple(EditCommand::MoveToLineStart { select: false }));
        binding(keybindings, KeyCode::End, simple(EditCommand::MoveToLineEnd { select: false }));

        ctrl_binding(keybindings, KeyCode::Char('c'), ReedlineEvent::CtrlC);
        ctrl_binding(keybindings, KeyCode::Char('d'), ReedlineEvent::CtrlD);
        ctrl_binding(keybindings, KeyCode::Char('z'), simple(EditCommand::Undo));
        ctrl_binding(keybindings, KeyCode::Char('y'), simple(EditCommand::Redo));
        ctrl_binding(
            keybindings,
            KeyCode::Left,
            simple(EditCommand::MoveWordLeft { select: false }),
        );
        ctrl_binding(
            keybindings,
            KeyCode::Right,
            simple(EditCommand::MoveWordRight { select: false }),
        );
        ctrl_binding(keybindings, KeyCode::Backspace, simple(EditCommand::BackspaceWord));
        ctrl_binding(keybindings, KeyCode::Delete, simple(EditCommand::DeleteWord));

        ctrl_binding(keybindings, KeyCode::Enter, ReedlineEvent::Submit);
        shift_binding(keybindings, KeyCode::Enter, simple(EditCommand::InsertNewline));
    }
}

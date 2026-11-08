
use std::{
    any::Any,
    cell::Cell,
    rc::Rc,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use tokio::sync::{mpsc, oneshot};

use crate::{
    language::{
        data::{native_value::NativeData, value::Value},
        interpreter::{ErrorWhat, Interpreter, InterpreterError},
        native::NativeOutcome,
        text::location::Location,
    },
    runtime::{
        readline::{self, Command, Control, Event, Fault, Limits, PromptSnapshot, Text},
        tasks::{self, NativeFault, NativeFuture, NativeReply},
    },
};

mod callbacks;
mod values;
use values::{data, enumeration, field, integer, native, record, string};

enum Failure
{
    Fault(Fault),
    Raised(InterpreterError),
}
type Result<T> = std::result::Result<T, Failure>;
impl From<Fault> for Failure
{
    fn from(value: Fault) -> Self
    {
        Self::Fault(value)
    }
}
impl From<InterpreterError> for Failure
{
    fn from(value: InterpreterError) -> Self
    {
        Self::Raised(value)
    }
}

struct Editor
{
    control: Arc<Control>,
    prompt: Value,
    complete: Value,
    validate: Value,
    highlight: Value,
    limits: Limits,
    leased: Cell<bool>,
    limit: usize,
}
impl Drop for Editor
{
    fn drop(&mut self)
    {
        self.control.close();
    }
}
struct Handle
{
    editor: Rc<Editor>,
    lease: bool,
    released: Cell<bool>,
}
impl NativeData for Handle
{
    fn type_name(&self) -> &'static str
    {
        "__readline_Editor"
    }
    fn display(&self) -> String
    {
        "<Reedline editor>".into()
    }
    fn as_any(&self) -> &dyn Any
    {
        self
    }
    fn key(&self) -> Option<Vec<u8>>
    {
        Some(self.editor.control.id.to_ne_bytes().to_vec())
    }
    fn operation(&self) -> bool
    {
        self.lease && !self.released.get()
    }
    fn release(&self)
    {
        if self.lease && !self.released.replace(true)
        {
            self.editor.leased.set(false);
            if self.editor.control.busy.load(Ordering::Acquire)
            {
                self.editor.control.cancel();
            }
        }
    }
}
struct Busy(Arc<Control>);
impl Drop for Busy
{
    fn drop(&mut self)
    {
        self.0.busy.store(false, Ordering::Release);
    }
}

impl Interpreter
{
    pub(in crate::language::interpreter) fn handle_readline(
        &mut self,
        location: &Location,
        args: &[Value],
    ) -> NativeOutcome
    {
        match self.readline_dispatch(location, args)
        {
            Ok(value) => NativeOutcome::Ready(value),
            Err(Failure::Fault(fault)) =>
            {
                let origin = Location::new(fault.file, fault.line as usize, fault.column as usize);
                let mut value = self.readline_fault(fault);
                if let Err(error) = self.activate_failure(location, &mut value, false)
                {
                    return NativeOutcome::Failed(error);
                }
                if let Some(failure) = crate::language::data::failure::occurrence(&value)
                {
                    failure.borrow_mut().native_origin = Some(origin);
                }
                NativeOutcome::Ready(value)
            }
            Err(Failure::Raised(error)) => NativeOutcome::Failed(error),
        }
    }
    fn readline_fault(&self, fault: Fault) -> Value
    {
        let code = fault
            .code
            .map_or(Value::None, |value| Value::Integer(value as i64));
        let message = Value::from_string(fault.message.clone());
        let (kind, fields) = match fault.kind
        {
            "TerminalError" =>
            {
                let name = if fault.message.starts_with("NotTerminal")
                {
                    "NotTerminal"
                }
                else if fault.message.starts_with("NotForeground")
                {
                    "NotForeground"
                }
                else if fault.message.starts_with("Restore")
                {
                    "Restore"
                }
                else
                {
                    "Read"
                };
                (
                    fault.kind,
                    vec![enumeration(self, "TerminalFailure", name), message, code],
                )
            }
            "HistoryError" => (
                fault.kind,
                vec![
                    Value::from_string(fault.path.unwrap_or_default()),
                    message,
                    code,
                ],
            ),
            "CallbackError" => (
                fault.kind,
                vec![
                    Value::from_string(fault.hook.unwrap_or("callback").into()),
                    message,
                ],
            ),
            _ => (fault.kind, vec![message]),
        };
        record(self, kind, fields)
    }
    fn readline_dispatch(&mut self, location: &Location, args: &[Value]) -> Result<Value>
    {
        let command = args
            .first()
            .and_then(string)
            .ok_or_else(|| Fault::new("ConfigError", "Missing readline operation"))?;
        let input = args.get(1).unwrap_or(&Value::None);
        let rest = &args[args.len().min(2)..];
        if let Some(value) = self.readline_value(command, input, rest)?
        {
            return Ok(value);
        }
        if command == "create"
        {
            return self.readline_create(location, input);
        }
        if command == "attach"
        {
            let lease = rest
                .first()
                .ok_or_else(|| Fault::new("ConfigError", "Missing line lease"))?;
            if native::<Handle>(lease).is_none_or(|handle| !handle.lease)
            {
                return Err(Fault::new("ConfigError", "Expected line lease").into());
            }
            let Value::Native(lease) = lease.underlying()
            else
            {
                unreachable!()
            };
            if !super::streams::attach_iteration_resource(input, lease.clone())
            {
                return Err(Fault::new("ConfigError", "Expected managed reader").into());
            }
            return Ok(input.clone());
        }
        let handle =
            native::<Handle>(input).ok_or_else(|| Fault::new("ConfigError", "Expected Editor"))?;
        let editor = handle.editor.clone();
        let control = editor.control.clone();
        if command == "close"
        {
            if !control.closed.load(Ordering::Acquire)
                && !control.busy.load(Ordering::Acquire)
                && !readline::editing()
            {
                let outcome = self.readline_flush(location, &editor, false);
                control.close();
                return outcome.map(|_| Value::None);
            }
            control.close();
            return Ok(Value::None);
        }
        if command == "release"
        {
            handle.release();
            return Ok(Value::None);
        }
        if command == "draft"
        {
            return Ok(Value::from_string(control.draft.lock().unwrap().clone()));
        }
        if command == "history_status"
        {
            let problem = control.problem.lock().unwrap().clone();
            let mode = if editor.limit == 0
            {
                "Disabled"
            }
            else if control.path.is_some() && problem.is_none()
            {
                "Persistent"
            }
            else
            {
                "Memory"
            };
            let problem = problem.map_or(Value::None, |message| {
                record(
                    self,
                    "HistoryProblem",
                    vec![
                        Value::from_string(
                            control
                                .path
                                .as_ref()
                                .map_or(String::new(), |p| p.display().to_string()),
                        ),
                        Value::from_string(message),
                        Value::None,
                    ],
                )
            });
            return Ok(record(
                self,
                "HistoryStatus",
                vec![enumeration(self, "HistoryMode", mode), problem],
            ));
        }
        if handle.released.get() || control.closed.load(Ordering::Acquire)
        {
            return Err(Fault::new("ClosedError", "Editor is closed").into());
        }
        if editor.leased.get() && !handle.lease
        {
            return Err(Fault::new("BusyError", "Editor belongs to an active line stream").into());
        }
        if command == "lease"
        {
            if control.busy.load(Ordering::Acquire) || editor.leased.replace(true)
            {
                return Err(Fault::new("BusyError", "Editor already has a consumer").into());
            }
            return Ok(data(Handle {
                editor,
                lease: true,
                released: Cell::new(false),
            }));
        }
        if control.busy.load(Ordering::Acquire) || readline::editing()
        {
            return Err(Fault::new("BusyError", "Terminal editor is active").into());
        }
        match command
        {
            "flush" =>
            {
                control.busy.store(true, Ordering::Release);
                let _busy = Busy(control.clone());
                self.readline_flush(location, &editor, true)?;
                Ok(Value::None)
            }
            "remember" =>
            {
                control.busy.store(true, Ordering::Release);
                let _busy = Busy(control.clone());
                let text = rest
                    .first()
                    .and_then(string)
                    .ok_or_else(|| Fault::new("ConfigError", "History entry must be String"))?;
                if text.len() > editor.limits.input
                {
                    return Err(
                        Fault::new("LimitError", "History entry exceeds input limit").into(),
                    );
                }
                let (reply, receiver) = oneshot::channel();
                readline::send(Command::Remember {
                    id: control.id,
                    text: text.into(),
                    reply,
                })?;
                self.readline_ack(location, receiver)?;
                self.readline_flush(location, &editor, false)?;
                Ok(Value::None)
            }
            "read" => self.readline_read(location, &editor, rest.first().unwrap_or(&Value::None)),
            _ => Err(Fault::new("ConfigError", "Unknown readline operation").into()),
        }
    }
    fn readline_ack(
        &mut self,
        location: &Location,
        receiver: oneshot::Receiver<std::result::Result<(), Fault>>,
    ) -> Result<()>
    {
        match self.readline_wait(Box::pin(async move {
            receiver
                .await
                .map(|value| Box::new(value) as Box<dyn Any + Send>)
                .map_err(|_| NativeFault::new("ReadError", "Editor owner stopped"))
        }))
        {
            Ok(value) =>
            {
                (*value.downcast::<std::result::Result<(), Fault>>().unwrap()).map_err(Into::into)
            }
            Err(error) => Err(InterpreterError::new(
                location.clone(),
                match error.kind.as_str()
                {
                    "Interrupted" if !error.cancelled => ErrorWhat::Interrupted,
                    "Interrupted" | "OperationCancelled" => ErrorWhat::OperationCancelled,
                    "CleanupAbort" => ErrorWhat::CleanupAbort,
                    "CleanupTimeout" => ErrorWhat::CleanupTimeout,
                    _ => ErrorWhat::InvalidOperand(error.message),
                },
            )
            .into()),
        }
    }
    fn readline_flush(&mut self, location: &Location, editor: &Editor, strict: bool) -> Result<()>
    {
        let control = editor.control.clone();
        match self.readline_wait(Box::pin(async move {
            Ok(Box::new(control.sync(strict).await) as Box<dyn Any + Send>)
        }))
        {
            Ok(reply) =>
            {
                editor.control.warn();
                (*reply.downcast::<std::result::Result<(), Fault>>().unwrap()).map_err(Into::into)
            }
            Err(error) => Err(InterpreterError::new(
                location.clone(),
                match error.kind.as_str()
                {
                    "Interrupted" if !error.cancelled => ErrorWhat::Interrupted,
                    "CleanupAbort" => ErrorWhat::CleanupAbort,
                    "CleanupTimeout" => ErrorWhat::CleanupTimeout,
                    _ => ErrorWhat::OperationCancelled,
                },
            )
            .into()),
        }
    }
    fn readline_create(&mut self, location: &Location, config: &Value) -> Result<Value>
    {
        if readline::editing()
        {
            return Err(Fault::new("BusyError", "Terminal editor is active").into());
        }
        let limits = values::limits(field(config, "limits"))?;
        let history = field(config, "history");
        let limit = values::bounded(field(history, "limit"), 1000, 0, 10000)? as usize;
        let path = if limit == 0
        {
            None
        }
        else
        {
            let path = field(history, "path");
            if matches!(path.underlying(), Value::None)
            {
                None
            }
            else
            {
                let path = string(path)
                    .filter(|s| !s.is_empty() && !s.contains('\0'))
                    .ok_or_else(|| Fault::new("ConfigError", "Invalid history path"))?;
                let path = self.eval_path_from(path);
                let path = std::path::PathBuf::from(path);
                let path = if path.is_absolute()
                {
                    path
                }
                else
                {
                    std::env::current_dir()
                        .map_err(|e| Fault::io("ConfigError", e))?
                        .join(path)
                };
                if path.file_name().is_none()
                {
                    return Err(Fault::new("ConfigError", "History path must name a file").into());
                }
                Some(path)
            }
        };
        let strict_history = values::enum_name(field(history, "failure")) == Some("Raise");
        let control = readline::control(path.clone(), limit, strict_history);
        let complete = field(config, "complete").clone();
        let validate = field(config, "validate").clone();
        let highlight = field(config, "highlight").clone();
        let native_config = readline::Config {
            manual_history: values::enum_name(field(history, "record")) == Some("Manual"),
            complete: !matches!(complete.underlying(), Value::None),
            validate: !matches!(validate.underlying(), Value::None),
            highlight: !matches!(highlight.underlying(), Value::None),
            vi: values::enum_name(field(config, "edit_mode")) == Some("Vi"),
            limits: limits.clone(),
        };
        let (reply, receiver) = oneshot::channel();
        readline::send(Command::Create {
            config: native_config,
            control: control.clone(),
            reply,
        })?;
        if let Err(error) = self.readline_ack(location, receiver)
        {
            control.close();
            return Err(error);
        }
        Ok(data(Handle {
            editor: Rc::new(Editor {
                control,
                prompt: field(config, "prompt").clone(),
                complete,
                validate,
                highlight,
                limits,
                leased: Cell::new(false),
                limit,
            }),
            lease: false,
            released: Cell::new(false),
        }))
    }
    fn readline_prompt_part(
        &mut self,
        location: &Location,
        value: &Value,
        default: &str,
    ) -> Result<Text>
    {
        if matches!(value.underlying(), Value::None)
        {
            return Ok(Text::plain(default));
        }
        let value = if value.is_callable() || value.underlying().is_callable()
        {
            self.readline_call(location, value.clone(), Vec::new(), None)?
        }
        else
        {
            value.clone()
        };
        values::text(&value).map_err(Into::into)
    }
    fn readline_read(
        &mut self,
        location: &Location,
        editor: &Rc<Editor>,
        options: &Value,
    ) -> Result<Value>
    {
        let control = editor.control.clone();
        if control.ended.load(Ordering::Acquire)
        {
            return Ok(self.readline_event(2, String::new()));
        }
        if control.busy.swap(true, Ordering::AcqRel)
        {
            return Err(Fault::new("BusyError", "Editor already has a read").into());
        }
        let _busy = Busy(control.clone());
        let initial = values::optional_string(field(options, "initial"))?.unwrap_or_default();
        let cursor = integer(field(options, "cursor")).unwrap_or(initial.len() as i64);
        if initial.len() > editor.limits.input
            || cursor < 0
            || cursor as usize > initial.len()
            || !initial.is_char_boundary(cursor as usize)
        {
            return Err(Fault::new("ConfigError", "Invalid initial input or UTF-8 cursor").into());
        }
        let override_prompt = field(options, "prompt");
        let prompt = if matches!(override_prompt.underlying(), Value::None)
        {
            &editor.prompt
        }
        else
        {
            override_prompt
        };
        let (left, right, continuation) = if string(prompt).is_some()
            || matches!(prompt.underlying(), Value::None)
        {
            (
                self.readline_prompt_part(location, prompt, "> ")?,
                Text::default(),
                Text::plain("> "),
            )
        }
        else
        {
            (
                self.readline_prompt_part(location, field(prompt, "left"), "> ")?,
                self.readline_prompt_part(location, field(prompt, "right"), "")?,
                self.readline_prompt_part(location, field(prompt, "continuation"), "> ")?,
            )
        };
        if left.bytes() + right.bytes() + continuation.bytes() > 16384
        {
            return Err(Fault::new("LimitError", "Prompt exceeds 16 KiB").into());
        }
        if control.closed.load(Ordering::Acquire)
        {
            return Err(
                Fault::new("ClosedError", "Editor was closed during prompt preparation").into(),
            );
        }
        if !std::io::IsTerminal::is_terminal(&std::io::stdin())
            || !std::io::IsTerminal::is_terminal(&std::io::stderr())
        {
            return Err(Fault::new(
                "TerminalError",
                "NotTerminal: stdin and stderr must be terminals",
            )
            .into());
        }
        self.readline_flush(location, editor, false)?;
        let lease = readline::EditingLease::acquire()?;
        control.cancel.store(false, Ordering::Release);
        control.exceeded.store(false, Ordering::Release);
        control.interrupt.store(false, Ordering::Release);
        control.break_signal.store(false, Ordering::Release);
        control.finished.store(false, Ordering::Release);
        control.restored.store(false, Ordering::Release);
        let (events, mut receiver) = mpsc::unbounded_channel();
        readline::send(Command::Read {
            id: control.id,
            prompt: PromptSnapshot {
                left,
                right,
                continuation,
                mode: self.color_mode,
            },
            initial,
            cursor: cursor as usize,
            lease,
            startup: self.take_startup_input(),
            events,
        })?;
        let mut failure: Option<Failure> = None;
        loop
        {
            let reply = self.readline_wait(Box::pin(async move {
                let event = receiver.recv().await;
                Ok(Box::new((event, receiver)) as Box<dyn Any + Send>)
            }));
            match reply
            {
                Ok(reply) =>
                {
                    let (event, next) = *reply
                        .downcast::<(Option<Event>, mpsc::UnboundedReceiver<Event>)>()
                        .expect("Readline event packet");
                    receiver = next;
                    match event
                    {
                        Some(Event::Callback { hook, edit, answer }) =>
                        {
                            if failure.is_some()
                            {
                                let _ = answer.send(readline::Answer::Stop);
                                continue;
                            }
                            match self.readline_callback(location, editor, hook, &edit)
                            {
                                Ok(value) =>
                                {
                                    let _ = answer.send(value);
                                }
                                Err(error) =>
                                {
                                    let error = if let Failure::Fault(mut fault) = error
                                    {
                                        fault.hook = Some(hook);
                                        Failure::Fault(fault)
                                    }
                                    else
                                    {
                                        error
                                    };
                                    if matches!(&error, Failure::Raised(error)
                                        if matches!(error.what, ErrorWhat::Interrupted))
                                    {
                                        control.interrupt.store(true, Ordering::Release);
                                    }
                                    else
                                    {
                                        failure = Some(error);
                                    }
                                    control.cancel();
                                    let _ = answer.send(readline::Answer::Stop);
                                }
                            }
                        }
                        Some(Event::Done(result)) =>
                        {
                            if let Some(error) = failure
                            {
                                return Err(error);
                            }
                            let (kind, text) = result?;
                            if kind == 0
                            {
                                *control.draft.lock().unwrap() = text.clone();
                                self.readline_flush(location, editor, false)?;
                                control.draft.lock().unwrap().clear();
                            }
                            return Ok(self.readline_event(kind, text));
                        }
                        None =>
                        {
                            return Err(Fault::new("TerminalError", "Editor owner stopped").into());
                        }
                    }
                }
                Err(error) =>
                {
                    let interrupted = error.kind == "Interrupted" && !error.cancelled;
                    control.interrupt.store(interrupted, Ordering::Release);
                    control.cancel();
                    // The abandoned channel cannot hold a callback hostage. Join restoration
                    // independently before handing any event/failure back to script code.
                    while !control.restored.load(Ordering::Acquire)
                    {
                        let joined = control.clone();
                        let _ = tasks::wait(Box::pin(async move {
                            while !joined.restored.load(Ordering::Acquire)
                            {
                                tokio::time::sleep(Duration::from_millis(5)).await;
                            }
                            Ok(Box::new(()) as Box<dyn Any + Send>)
                        }));
                    }
                    if interrupted
                    {
                        return Ok(self.readline_event(1, control.draft.lock().unwrap().clone()));
                    }
                    return Err(InterpreterError::new(
                        location.clone(),
                        match error.kind.as_str()
                        {
                            "CleanupAbort" => ErrorWhat::CleanupAbort,
                            "CleanupTimeout" => ErrorWhat::CleanupTimeout,
                            _ => ErrorWhat::OperationCancelled,
                        },
                    )
                    .into());
                }
            }
        }
    }
    fn readline_event(&self, kind: usize, text: String) -> Value
    {
        record(
            self,
            "ReadEvent",
            vec![
                enumeration(
                    self,
                    "EventKind",
                    match kind
                    {
                        0 => "Line",
                        1 => "Interrupt",
                        _ => "End",
                    },
                ),
                Value::from_string(text),
            ],
        )
    }
    fn readline_wait(&self, future: NativeFuture) -> NativeReply
    {
        let operation = self.exceptions.operation.clone();
        let cleanup = self
            .exceptions
            .cleanup
            .as_ref()
            .filter(|guard| !guard.hook)
            .map(|guard| guard.deadline);
        tasks::wait(Box::pin(async move {
            let mut future = future;
            let mut tick = tokio::time::interval(Duration::from_millis(10));
            loop
            {
                tokio::select! {
                    reply = &mut future => return reply,
                    _ = tick.tick() =>
                    {
                        if cleanup.is_some_and(|deadline| Instant::now() >= deadline)
                        {
                            return Err(NativeFault::new(
                                "CleanupTimeout", "Cleanup budget exhausted"));
                        }
                        if    operation.as_ref().is_some_and(|(flags, deadline)|
                            Instant::now() >= *deadline || flags.iter().any(|flag|
                                flag.load(Ordering::Acquire)))
                        {
                            return Err(NativeFault::new(
                                "OperationCancelled", "Operation cancelled"));
                        }
                    }
                }
            }
        }))
    }
    fn readline_call(
        &mut self,
        location: &Location,
        callback: Value,
        args: Vec<Value>,
        limits: Option<&Limits>,
    ) -> Result<Value>
    {
        let previous = self.last_result.take();
        let parent = self.exceptions.cleanup.take();
        let reporting = self.exceptions.reporting;
        let editing_callback = self.exceptions.readline_callback;
        let floor = self.exceptions.report_floor;
        let initial_budget = limits.map_or(0, |limits| {
            parent.as_ref().map_or(limits.instructions, |guard| {
                guard.instructions.min(limits.instructions)
            })
        });
        if let Some(limits) = limits
        {
            self.exceptions.reporting = true;
            self.exceptions.readline_callback = true;
            self.exceptions.report_floor = self.exceptions.regions.len();
            self.exceptions.cleanup = Some(super::exceptions::CleanupGuard {
                hook: true,
                deadline: parent.as_ref().map_or(
                    Instant::now() + Duration::from_millis(limits.milliseconds),
                    |guard| {
                        guard
                            .deadline
                            .min(Instant::now() + Duration::from_millis(limits.milliseconds))
                    },
                ),
                instructions: initial_budget,
                function_depth: self.exceptions.frames.len(),
                interrupted: parent.as_ref().is_some_and(|guard| guard.interrupted),
            });
        }
        else
        {
            self.exceptions.cleanup = parent.clone();
        }
        let outcome = self.execute_value(location, callback, args);
        let value = self.last_result.take().unwrap_or(Value::None);
        self.last_result = previous;
        let completed = self.exceptions.cleanup.take();
        let interrupted =
            limits.is_some() && completed.as_ref().is_some_and(|guard| guard.interrupted);
        self.exceptions.cleanup = parent.map(|mut parent| {
            if let (Some(_), Some(completed)) = (limits, completed.as_ref())
            {
                parent.instructions = parent
                    .instructions
                    .saturating_sub(initial_budget.saturating_sub(completed.instructions));
                parent.interrupted |= completed.interrupted;
            }
            parent
        });
        self.exceptions.reporting = reporting;
        self.exceptions.readline_callback = editing_callback;
        self.exceptions.report_floor = floor;
        if interrupted
        {
            return Err(InterpreterError::new(location.clone(), ErrorWhat::Interrupted).into());
        }
        if outcome
            .as_ref()
            .is_err_and(|error| matches!(error.what, ErrorWhat::HookTimeout))
        {
            return Err(Fault::new("CallbackError", "Editing callback budget exhausted").into());
        }
        outcome?;
        self.consume_success(location, &value)?;
        Ok(value)
    }
}

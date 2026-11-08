
use std::{
    cell::RefCell,
    io::{self, Write},
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::channel,
    },
    thread::Builder,
    time::{Duration, Instant},
};

use crate::language::{
    data::{
        failure::{FailureStatus, occurrence},
        value::Value,
    },
    interpreter::{
        ErrorWhat, Interpreter, InterpreterError, InterpreterResult,
        exceptions::{CleanupGuard, field, public_summary},
    },
    text::location::Location,
};

static STDERR_BUSY: AtomicBool = AtomicBool::new(false);

fn truncate_bytes(mut text: String, limit: usize) -> String
{
    let mut length = text.len().min(limit);
    while !text.is_char_boundary(length)
    {
        length -= 1;
    }
    text.truncate(length);
    text
}

fn formatter_input_bounded(
    value: &Value,
    bytes: &mut usize,
    nodes: &mut usize,
    depth: usize,
) -> bool
{
    if depth > 16 || *nodes == 0
    {
        return false;
    }
    *nodes -= 1;
    match value
    {
        Value::String(text, _) =>
        {
            if text.len() > *bytes
            {
                false
            }
            else
            {
                *bytes -= text.len();
                true
            }
        }
        Value::Named(item) => formatter_input_bounded(&item.value, bytes, nodes, depth + 1),
        Value::Struct(item) => item
            .fields
            .iter()
            .all(|value| formatter_input_bounded(value, bytes, nodes, depth + 1)),
        Value::Array(items) | Value::ArgumentExpansion(items) => items
            .iter()
            .all(|value| formatter_input_bounded(value, bytes, nodes, depth + 1)),
        Value::HashMap(items) => items
            .iter()
            .all(|(_, value)| formatter_input_bounded(value, bytes, nodes, depth + 1)),
        Value::Native(native) =>
        {
            if let Some(document) =
                native.downcast::<crate::language::data::formats::json::document::Document>()
            {
                fn bounded(
                    node: &crate::language::data::formats::json::document::JsonNode,
                    bytes: &mut usize,
                    nodes: &mut usize,
                    depth: usize,
                ) -> bool
                {
                    use crate::language::data::formats::json::document::JsonNode;
                    if depth > 16 || *nodes == 0
                    {
                        return false;
                    }
                    *nodes -= 1;
                    match node
                    {
                        JsonNode::String(text) | JsonNode::Number(text) =>
                        {
                            if text.len() > *bytes
                            {
                                false
                            }
                            else
                            {
                                *bytes -= text.len();
                                true
                            }
                        }
                        JsonNode::Array(items) => items
                            .iter()
                            .all(|node| bounded(node, bytes, nodes, depth + 1)),
                        JsonNode::Object(items) => items.iter().all(|(key, node)| {
                            if key.len() > *bytes
                            {
                                return false;
                            }
                            *bytes -= key.len();
                            bounded(node, bytes, nodes, depth + 1)
                        }),
                        _ => true,
                    }
                }
                bounded(&document.root, bytes, nodes, depth)
            }
            else
            {
                true
            }
        }
        _ => true,
    }
}

fn passive_view(value: &Value) -> Value
{
    let mut value = value.clone();
    if let Value::Struct(item) = &mut value
        && let Some(failure) = &item.failure
    {
        let mut failure = failure.borrow().clone();
        failure.status = FailureStatus::Acknowledged;
        failure.slots.clear();
        Rc::make_mut(item).failure = Some(Rc::new(RefCell::new(failure)));
    }
    value
}

fn bounded_stderr(text: String)
{
    if STDERR_BUSY
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let (sender, receiver) = channel();
    let worker = Builder::new()
        .name("shelly-error-report".into())
        .spawn(move || {
            let result = io::stderr().write_all(text.as_bytes());
            STDERR_BUSY.store(false, Ordering::Release);
            let _ = sender.send(result);
        });
    match worker
    {
        Ok(_) =>
        {
            let _ = receiver.recv_timeout(Duration::from_millis(200));
        }
        Err(_) =>
        {
            STDERR_BUSY.store(false, Ordering::Release);
        }
    }
}

fn location_text(value: &Value) -> String
{
    let origin = field(value, "origin").unwrap_or(Value::None).as_text();
    let line = field(value, "line").unwrap_or(Value::None).as_integer();
    let column = field(value, "column").unwrap_or(Value::None).as_integer();
    format!(
        "{}: ({}, {})",
        origin.chars().take(1024).collect::<String>(),
        line,
        column
    )
}

fn stack_text(stack: &Value) -> String
{
    let mut output = String::new();
    if let Some(origin) = field(stack, "native_origin").filter(|value| *value != Value::None)
    {
        output.push_str(&format!("\nNative error at {}", location_text(&origin)));
    }
    if let Some(Value::Array(frames)) = field(stack, "frames")
        && !frames.is_empty()
    {
        output.push_str("\nCall stack (most recent call first):");
        for frame in frames.iter().take(128)
        {
            let name = field(frame, "name").unwrap_or(Value::None).as_text();
            let native = field(frame, "native").is_some_and(|value| value.as_bool());
            let location = field(frame, "location").unwrap_or(Value::None);
            output.push_str(&format!(
                "\n    at {}{} ({})",
                name.chars().take(1024).collect::<String>(),
                if native { " [native]" } else { "" },
                location_text(&location)
            ));
            if let Some(boundary) =
                field(frame, "native_boundary").filter(|value| *value != Value::None)
            {
                output.push_str(&format!(
                    "\n        native boundary at {}",
                    location_text(&boundary)
                ));
            }
        }
    }
    if let Some(Value::Array(propagation)) = field(stack, "propagation")
    {
        for location in propagation.iter().take(128)
        {
            output.push_str(&format!("\nPropagated at {}", location_text(location)));
        }
    }
    output
}

impl Interpreter
{
    fn format_failure(&mut self, error: &Value) -> String
    {
        let fallback = public_summary(error);
        if !formatter_input_bounded(error, &mut 65536, &mut 1024, 0)
        {
            return fallback;
        }
        let Some(formatter) =
            occurrence(error).and_then(|failure| failure.borrow().formatter.clone())
        else
        {
            return fallback;
        };
        let previous = self.last_result.take();
        let reporting = self.exceptions.reporting;
        let floor = self.exceptions.report_floor;
        self.exceptions.reporting = true;
        self.exceptions.report_floor = self.exceptions.regions.len();
        let parent = self.exceptions.cleanup.take();
        let instructions = parent
            .as_ref()
            .map_or(50_000, |guard| guard.instructions.min(50_000));
        let deadline = Instant::now() + Duration::from_millis(100);
        self.exceptions.cleanup = Some(CleanupGuard {
            hook: false,
            deadline: parent
                .as_ref()
                .map_or(deadline, |guard| guard.deadline.min(deadline)),
            instructions,
            interrupted: false,
            function_depth: self.exceptions.frames.len(),
        });
        let result = self.execute_function(
            &Location::new("<error formatter>", 1, 1),
            "<error formatter>",
            &formatter,
            &[passive_view(error)],
        );
        let value = self.last_result.take();
        let consumed = instructions.saturating_sub(
            self.exceptions
                .cleanup
                .take()
                .map_or(0, |guard| guard.instructions),
        );
        self.exceptions.cleanup = parent.map(|mut guard| {
            guard.instructions = guard.instructions.saturating_sub(consumed);
            guard
        });
        self.exceptions.reporting = reporting;
        self.exceptions.report_floor = floor;
        self.last_result = previous;
        if result.is_ok()
            && let Some(Value::String(value, _)) = value
        {
            truncate_bytes(value, 32_768)
        }
        else
        {
            fallback
        }
    }

    pub(super) fn handle_report_error(
        &mut self,
        location: &Location,
        args: &[Value],
    ) -> InterpreterResult<()>
    {
        if crate::runtime::readline::editing()
        {
            return Err(InterpreterError::new(location.clone(),
                ErrorWhat::ReceiverBusy("Terminal is busy editing input".into())));
        }
        let [stack, error] = args
        else
        {
            return Err(InterpreterError::new(
                location.clone(),
                ErrorWhat::ArgumentMismatch("report_error expects a CallStack and an error".into()),
            ));
        };
        let stack_type = self.scope().types.native_types["CallStack"].id;
        let error_type = self.scope().types.builtin_id("ErrorValue").unwrap();
        self.scope()
            .types
            .validate(stack_type, stack)
            .and_then(|_| self.scope().types.validate(error_type, error))
            .map_err(|message| {
                InterpreterError::new(location.clone(), ErrorWhat::ArgumentMismatch(message))
            })?;
        let origin = field(stack, "origin").unwrap_or(Value::None);
        let expected = field(stack, "error_type");
        let compatible = matches!(expected, Some(Value::Type(definition))
            if definition.id == self.scope().types.value_type(error.underlying()));
        let native = self.scope().types.value_type(error.underlying())
            == self.scope().types.native_types["RuntimeError"].id;
        let custom = compatible
            && occurrence(error).is_some_and(|failure| failure.borrow().formatter.is_some());
        let message = if custom
        {
            self.format_failure(error)
        }
        else if native
        {
            field(error, "message").unwrap_or(Value::None).as_text()
        }
        else if compatible
        {
            self.format_failure(error)
        }
        else
        {
            public_summary(error)
        };
        let prefix = self
            .exceptions
            .report_prefix
            .as_deref()
            .unwrap_or("Error: ");
        let mut output = format!(
            "{}Interpreter error at {}: {}",
            prefix,
            location_text(&origin),
            message
        );
        output.push_str(&stack_text(stack));
        if let Some(Value::Array(related)) = field(stack, "related")
        {
            for related in related.iter().take(16)
            {
                let relation = field(related, "relation").unwrap_or(Value::None).as_text();
                let snapshot = field(related, "error").unwrap_or(Value::None);
                let message = field(&snapshot, "formatted")
                    .unwrap_or(Value::None)
                    .as_text();
                let stack = field(related, "stack").unwrap_or(Value::None);
                let origin = field(&stack, "origin").unwrap_or(Value::None);
                output.push_str(&format!(
                    "\n{} at {}: {}",
                    relation,
                    location_text(&origin),
                    message
                ));
                output.push_str(&stack_text(&stack));
            }
        }
        let mut output = truncate_bytes(output, 65_535);
        output.push('\n');
        bounded_stderr(output);
        self.last_result = Some(Value::None);
        Ok(())
    }

    pub fn report_uncaught(&mut self, error: &InterpreterError)
    {
        for occurrence in &error.trace.resolved
        {
            occurrence.borrow_mut().status = FailureStatus::Acknowledged;
        }
        let payload = self.caught_value(error);
        let id = self.scope().types.value_type(payload.underlying());
        let stack = self.call_stack_value(error, id);
        let previous = self.last_result.take();
        let parent = self.exceptions.cleanup.take();
        self.exceptions.cleanup = Some(CleanupGuard {
            hook: false,
            deadline: Instant::now() + Duration::from_millis(100),
            instructions: 50_000,
            function_depth: self.exceptions.frames.len(),
            interrupted: false,
        });
        let reporting = self.exceptions.reporting;
        let floor = self.exceptions.report_floor;
        self.exceptions.reporting = true;
        self.exceptions.report_floor = self.exceptions.regions.len();
        let halted = self.halted;
        self.halted = false;
        let result = if let Some(reporter) = self.module_function("report_error")
        {
            self.execute_function(
                &error.location,
                "report_error",
                &reporter,
                &[stack.clone(), payload.clone()],
            )
        }
        else
        {
            self.handle_report_error(&error.location, &[stack.clone(), payload.clone()])
        };
        if result.is_err()
        {
            let _ = self.handle_report_error(&error.location, &[stack, payload]);
        }
        self.exceptions.cleanup = parent;
        self.exceptions.reporting = reporting;
        self.exceptions.report_floor = floor;
        self.halted = halted;
        self.last_result = previous;
    }

    pub fn report_repl_error(&mut self, error: &InterpreterError)
    {
        let previous = self
            .exceptions
            .report_prefix
            .replace("".into());
        self.report_uncaught(error);
        self.exceptions.report_prefix = previous;
    }
}

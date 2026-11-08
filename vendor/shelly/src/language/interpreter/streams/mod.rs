
use std::{
    any::Any,
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use crate::language::{
    data::{
        map_key::MapKey,
        native_value::{NativeData, NativeValue},
        scoped_variables::ValueReference,
        types::{StructValue, TypeId},
        value::{Executable, Value},
    },
    interpreter::{
        ErrorWhat, Interpreter, InterpreterError, InterpreterResult, iteration::Iteration,
    },
    text::location::Location,
};

// This shared cursor survives the iterator protocol's private receiver copy. No script callback
// runs while a RefCell guard is held, including when that callback suspends in a native wait.
pub(super) struct Reader
{
    iteration: RefCell<Option<Iteration>>,
    callbacks: RefCell<Option<(Value, Value, Value)>>,
    busy: Cell<bool>,
    owner: Cell<u64>,
    leased: Cell<bool>,
    consumer: Cell<Option<u64>>,
    context: Value,
    cancel: Arc<AtomicBool>,
    closed: Cell<bool>,
    ended: Cell<bool>,
    item: TypeId,
    boxed: bool,
    source: RefCell<Option<Value>>,
    snapshot: Value,
    completion: RefCell<Option<InterpreterResult<Value>>>,
    iteration_resource: RefCell<Option<NativeValue>>,
}

impl NativeData for Rc<Reader>
{
    fn type_name(&self) -> &'static str
    {
        "__stream_Reader"
    }
    fn display(&self) -> String
    {
        "<Stream reader>".into()
    }
    fn as_any(&self) -> &dyn Any
    {
        self
    }
}

pub(super) struct IterationLease(Rc<Reader>);
impl IterationLease
{
    pub(super) fn enter(&self)
    {
        self.0
            .consumer
            .set(Some(crate::runtime::tasks::current_task()));
    }
}
impl Drop for IterationLease
{
    fn drop(&mut self)
    {
        self.0.leased.set(false);
        self.0.consumer.set(None);
        // Some borrowed native sources reserve a handle for exactly one whole iteration.
        // Their release is native-only, so break/return/error can release it even in Drop.
        if let Some(resource) = self.0.iteration_resource.borrow_mut().take()
        {
            resource.0.release();
            self.0.closed.set(true);
            self.0.cancel.store(true, Ordering::Release);
            self.0.callbacks.borrow_mut().take();
            self.0.source.borrow_mut().take();
            self.0.iteration.borrow_mut().take();
        }
    }
}
pub(super) fn iteration_lease(
    value: &Value,
    location: &Location,
) -> InterpreterResult<Option<IterationLease>>
{
    let Some(reader) = reader(value)
    else
    {
        return Ok(None);
    };
    if reader.leased.replace(true)
    {
        return Err(InterpreterError::new(
            location.clone(),
            ErrorWhat::IterationError("Stream already has an iteration consumer".into()),
        ));
    }
    Ok(Some(IterationLease(reader)))
}

fn reader(value: &Value) -> Option<Rc<Reader>>
{
    if let Value::Native(native) = value.underlying()
    {
        native.downcast::<Rc<Reader>>().cloned()
    }
    else
    {
        None
    }
}

pub(super) fn attach_iteration_resource(value: &Value, resource: NativeValue) -> bool
{
    if let Some(reader) = reader(value)
    {
        *reader.iteration_resource.borrow_mut() = Some(resource);
        true
    }
    else { false }
}

fn context_summary(value: &Value) -> String
{
    let scalar = |value: &Value| match value.underlying()
    {
        Value::String(text, _) => text.chars().take(256).collect::<String>(),
        Value::Integer(_) | Value::Float(_, _) | Value::Boolean(_) | Value::None => value.as_text(),
        _ => value.type_name(),
    };
    match value.underlying()
    {
        Value::HashMap(items) => items
            .iter()
            .take(8)
            .map(|(name, value)| format!("{}: {}", scalar(&name.to_value()), scalar(value)))
            .collect::<Vec<_>>()
            .join(", "),
        Value::Struct(_) => super::exceptions::public_summary(value),
        _ => scalar(value),
    }
}

impl Interpreter
{
    fn stream_callback(&mut self, location: &Location, callback: Value)
    -> InterpreterResult<Value>
    {
        if matches!(callback, Value::None)
        {
            return Ok(Value::None);
        }
        self.execute_value(location, callback, Vec::new())?;
        let value = self.last_result.take().unwrap_or(Value::None);
        self.consume_success(location, &value)?;
        Ok(value)
    }

    fn stream_release(&mut self, location: &Location, source: &Rc<Reader>)
    -> InterpreterResult<()>
    {
        if source.busy.get()
        {
            if source.owner.get() == crate::runtime::tasks::current_task()
            {
                return Err(InterpreterError::new(
                    location.clone(),
                    ErrorWhat::IterationError(
                        "Cannot release a stream from its active callback".into(),
                    ),
                ));
            }
            source.cancel.store(true, Ordering::Release);
            let deadline = Instant::now() + Duration::from_secs(5);
            while source.busy.get()
            {
                if Instant::now() >= deadline
                {
                    return Err(InterpreterError::new(
                        location.clone(),
                        ErrorWhat::CleanupTimeout,
                    ));
                }
                crate::runtime::tasks::cooperate().map_err(|error| {
                    InterpreterError::new(
                        location.clone(),
                        ErrorWhat::UnsupportedSuspension(error.message),
                    )
                })?;
            }
        }
        if source.closed.replace(true)
        {
            return Ok(());
        }
        source.leased.set(false);
        source.consumer.set(None);
        let inherited = self.exceptions.cleanup.is_some();
        if !inherited
        {
            self.exceptions.cleanup = Some(super::exceptions::CleanupGuard {
                hook: false,
                deadline: Instant::now() + Duration::from_secs(5),
                instructions: 1_000_000,
                function_depth: self.exceptions.frames.len(),
                interrupted: false,
            });
        }
        let result = self.stream_release_body(location, source);
        if !inherited
        {
            self.exceptions.cleanup = None;
        }
        result
    }

    fn stream_release_body(
        &mut self,
        location: &Location,
        source: &Rc<Reader>,
    ) -> InterpreterResult<()>
    {
        // Clear captures before calling cleanup, so a failed/reentrant close cannot replay it.
        let callbacks = source.callbacks.borrow_mut().take();
        if let Some(resource) = source.iteration_resource.borrow_mut().take()
        { resource.0.release(); }
        let value = source.source.borrow_mut().take();
        source.iteration.borrow_mut().take();
        if let Some((_, _, close)) = callbacks
        {
            self.stream_callback(location, close)?;
        }
        if let Some(value) = value
        {
            let reference = ValueReference::temporary(value.clone());
            if let Some(close) = self.resolve_method(&value, reference, "close", &source.snapshot)
            {
                self.execute_method(location, &close, &[])?;
            }
        }
        Ok(())
    }

    pub(super) fn release_stream_region(
        &mut self,
        region: u64,
        failed: bool,
        abort: bool,
    ) -> InterpreterResult<()>
    {
        let parent = self.exceptions.regions.iter().rev().nth(1).copied();
        if !failed && let Some(parent) = parent
        {
            for (owner, _) in &mut self.exceptions.readers
            {
                if *owner == region
                {
                    *owner = parent;
                }
            }
            return Ok(());
        }
        let mut release = Vec::new();
        self.exceptions.readers.retain(|(owner, source)| {
            if *owner == region
            {
                release.push(source.clone());
                false
            }
            else
            {
                true
            }
        });
        let saved = self.last_result.take();
        let inherited = self.exceptions.cleanup.is_some();
        if !inherited && !release.is_empty()
        {
            self.exceptions.cleanup = Some(super::exceptions::CleanupGuard {
                hook: false,
                deadline: Instant::now() + Duration::from_secs(5),
                instructions: 1_000_000,
                function_depth: self.exceptions.frames.len(),
                interrupted: false,
            });
        }
        let mut result = Ok(());
        for source in release.into_iter().rev()
        {
            if abort
            {
                source.cancel.store(true, Ordering::Release);
                source.closed.set(true);
                source.leased.set(false);
                source.consumer.set(None);
                source.callbacks.borrow_mut().take();
                source.source.borrow_mut().take();
                source.iteration.borrow_mut().take();
                continue;
            }
            let cleanup = self.stream_release(&Location::default(), &source);
            result = self.combine_failure(result, cleanup, "Cleanup");
        }
        if !inherited
        {
            self.exceptions.cleanup = None;
        }
        self.last_result = saved;
        result
    }

    pub(super) fn adopt_stream_operation(&mut self, value: &Value)
    {
        let region = *self.exceptions.regions.last().unwrap_or(&0);
        if let Some(source) = reader(value)
        {
            for (owner, registered) in &mut self.exceptions.readers
            {
                if Rc::ptr_eq(&source, registered)
                {
                    *owner = region;
                }
            }
        }
        if let Value::Native(native) = value.underlying()
        {
            for (owner, registered) in &mut self.exceptions.resources
            {
                if Rc::ptr_eq(&native.0, &registered.0)
                {
                    *owner = region;
                }
            }
        }
    }
    pub(super) fn cancel_stream_source(
        &mut self,
        location: &Location,
        value: &Value,
    ) -> InterpreterResult<()>
    {
        if let Some(source) = reader(value)
        {
            self.stream_release(location, &source)?;
        }
        Ok(())
    }

    pub(super) fn handle_stream(
        &mut self,
        location: &Location,
        args: &[Value],
    ) -> InterpreterResult<()>
    {
        let [Value::String(command, _), input, rest @ ..] = args
        else
        {
            return Err(InterpreterError::new(
                location.clone(),
                ErrorWhat::ArgumentMismatch(
                    "Stream operation requires a command and source".into(),
                ),
            ));
        };
        if matches!(command.as_str(), "next" | "result")
            && let Some(source) = reader(input)
            && source.leased.get()
            && source.consumer.get() != Some(crate::runtime::tasks::current_task())
        {
            self.last_result = Some(self.io_fault(
                "LeaseError",
                "stream.consume",
                "Stream is reserved by another iteration consumer".into(),
            ));
            return Ok(());
        }
        match command.as_str()
        {
            "inherit_iteration" =>
            {
                let source = rest.first().and_then(reader);
                if let (Some(target), Some(source)) = (reader(input), source)
                {
                    *target.iteration_resource.borrow_mut() =
                        source.iteration_resource.borrow().clone();
                }
                self.last_result = Some(input.clone());
            },
            "reader" | "from_iterable" =>
            {
                self.exceptions
                    .readers
                    .retain(|(_, source)| !source.closed.get());
                if self.exceptions.readers.len() >= 256
                {
                    self.last_result = Some(self.io_fault(
                        "LimitError",
                        "stream.reader",
                        "Too many active stream readers".into(),
                    ));
                    return Ok(());
                }
                let Some(Value::Type(item)) = rest.first()
                else
                {
                    return Err(InterpreterError::new(
                        location.clone(),
                        ErrorWhat::ArgumentMismatch("Missing Item type".into()),
                    ));
                };
                let caller = self
                    .exceptions
                    .callers
                    .iter()
                    .rev()
                    .find(|scope| **scope != self.current_scope)
                    .cloned()
                    .unwrap_or_else(|| self.current_scope.clone());
                let scope = &self.scopes[&caller];
                let snapshot = Value::from_hash_map(
                    scope
                        .conversion_snapshot()
                        .into_iter()
                        .map(|(name, function)| {
                            (
                                MapKey::String(name),
                                Value::String("method".into(), Executable::Function(function)),
                            )
                        })
                        .collect(),
                );
                let (iteration, callbacks, source, boxed) = if command == "reader"
                {
                    let Value::Array(values) = input.underlying()
                    else
                    {
                        return Err(InterpreterError::new(
                            location.clone(),
                            ErrorWhat::ArgumentMismatch("Missing reader callbacks".into()),
                        ));
                    };
                    let get = |index| values.get(index).cloned().unwrap_or(Value::None);
                    (
                        None,
                        Some((get(0), get(1), get(2))),
                        None,
                        matches!(get(4), Value::Boolean(true)),
                    )
                }
                else
                {
                    let previous = std::mem::replace(&mut self.current_scope, caller);
                    let iteration = Iteration::new(self, location, input.clone(), &snapshot);
                    self.current_scope = previous;
                    (Some(iteration?), None, Some(input.clone()), true)
                };
                let source = Rc::new(Reader {
                    iteration: RefCell::new(iteration),
                    callbacks: RefCell::new(callbacks),
                    busy: Cell::new(false),
                    owner: Cell::new(0),
                    leased: Cell::new(false),
                    consumer: Cell::new(None),
                    context: if command == "reader"
                    {
                        if let Value::Array(values) = input.underlying()
                        {
                            values.get(3).cloned().unwrap_or(Value::None)
                        }
                        else
                        {
                            Value::None
                        }
                    }
                    else
                    {
                        Value::None
                    },
                    cancel: Arc::new(AtomicBool::new(false)),
                    closed: Cell::new(false),
                    ended: Cell::new(false),
                    item: item.id,
                    boxed,
                    source: RefCell::new(source),
                    snapshot,
                    completion: RefCell::new(None),
                    iteration_resource: RefCell::new(None),
                });
                let region = *self.exceptions.regions.last().unwrap_or(&0);
                self.exceptions.readers.push((region, source.clone()));
                self.last_result = Some(Value::Native(NativeValue::new(source)));
            }
            "next" =>
            {
                let source = reader(input).ok_or_else(|| {
                    InterpreterError::new(
                        location.clone(),
                        ErrorWhat::InvalidOperand("Expected stream Reader".into()),
                    )
                })?;
                if let Some(Err(error)) = source.completion.borrow().as_ref()
                {
                    return Err(error.clone());
                }
                if source.ended.get()
                {
                    self.last_result = Some(Value::None);
                    return Ok(());
                }
                if source.closed.get()
                {
                    self.last_result = Some(self.io_fault(
                        "CancelledError",
                        "stream.next",
                        "Stream was closed before completion".into(),
                    ));
                }
                else if source.busy.replace(true)
                {
                    self.last_result = Some(self.io_fault(
                        "LeaseError",
                        "stream.next",
                        "Stream already has an active callback".into(),
                    ));
                }
                else
                {
                    source.owner.set(crate::runtime::tasks::current_task());
                    let previous = self.exceptions.operation.take();
                    let mut cancel = previous
                        .as_ref()
                        .map_or_else(Vec::new, |(flags, _)| flags.clone());
                    cancel.push(source.cancel.clone());
                    let deadline = previous.as_ref().map_or(
                        Instant::now() + Duration::from_secs(86400),
                        |(_, deadline)| *deadline,
                    );
                    self.exceptions.operation = Some((cancel, deadline));
                    let callback = source
                        .callbacks
                        .borrow()
                        .as_ref()
                        .map(|value| value.0.clone());
                    let result = if let Some(callback) = callback
                    {
                        self.stream_callback(location, callback).map(|value| {
                            if matches!(value, Value::None)
                            {
                                None
                            }
                            else
                            {
                                Some(value)
                            }
                        })
                    }
                    else
                    {
                        let mut iteration = source.iteration.borrow_mut().take().unwrap();
                        let result = iteration.next(self, location);
                        *source.iteration.borrow_mut() = Some(iteration);
                        result
                    };
                    self.exceptions.operation = previous;
                    source.busy.set(false);
                    let result = match result
                    {
                        Ok(None) =>
                        {
                            let callback = source
                                .callbacks
                                .borrow()
                                .as_ref()
                                .map(|value| value.1.clone())
                                .unwrap_or(Value::None);
                            source.busy.set(true);
                            let previous = self.exceptions.operation.take();
                            let mut cancel = previous
                                .as_ref()
                                .map_or_else(Vec::new, |(flags, _)| flags.clone());
                            cancel.push(source.cancel.clone());
                            let deadline = previous.as_ref().map_or(
                                Instant::now() + Duration::from_secs(86400),
                                |(_, deadline)| *deadline,
                            );
                            self.exceptions.operation = Some((cancel, deadline));
                            let result = self.stream_callback(location, callback);
                            self.exceptions.operation = previous;
                            source.busy.set(false);
                            let cleanup = self.stream_release(location, &source);
                            let result = match (result, cleanup)
                            {
                                (Ok(value), Ok(())) => Ok(value),
                                (Err(error), cleanup) => self
                                    .combine_failure(Err(error), cleanup, "Cleanup")
                                    .map(|_| Value::None),
                                (_, Err(error)) => Err(error),
                            };
                            *source.completion.borrow_mut() = Some(result.clone());
                            result.map(|_| {
                                source.ended.set(true);
                                Value::None
                            })
                        }
                        Ok(Some(value))
                            if !source.boxed
                                || self.scope().types.value_type(&value) == source.item =>
                        {
                            Ok(value)
                        }
                        Ok(Some(value)) => Ok(Value::Struct(Rc::new(StructValue {
                            definition: self.scope().types.get(source.item),
                            fields: vec![value],
                            constructing: false,
                            failure: None,
                        }))),
                        Err(error) => Err(error),
                    };
                    match result
                    {
                        Ok(value) => self.last_result = Some(value),
                        Err(error) =>
                        {
                            let mut error = error;
                            if !matches!(source.context, Value::None)
                                && error.trace.related.len() < 16
                            {
                                let context = InterpreterError::new(
                                    location.clone(),
                                    ErrorWhat::Raised(format!(
                                        "Stream context: {}",
                                        context_summary(&source.context)
                                    )),
                                );
                                error
                                    .trace
                                    .related
                                    .push(self.related_failure(&context, "Context"));
                            }
                            let cleanup = self.stream_release(location, &source);
                            let error = self
                                .combine_failure(Err(error), cleanup, "Cleanup")
                                .unwrap_err();
                            *source.completion.borrow_mut() = Some(Err(error.clone()));
                            return Err(error);
                        }
                    }
                }
            }
            "context" =>
            {
                let source = reader(input).ok_or_else(|| {
                    InterpreterError::new(
                        location.clone(),
                        ErrorWhat::InvalidOperand("Expected stream Reader".into()),
                    )
                })?;
                self.last_result = Some(source.context.clone());
            }
            "result" =>
            {
                let source = reader(input).ok_or_else(|| {
                    InterpreterError::new(
                        location.clone(),
                        ErrorWhat::InvalidOperand("Expected stream Reader".into()),
                    )
                })?;
                if source.busy.get()
                {
                    self.last_result = Some(self.io_fault(
                        "LeaseError",
                        "stream.result",
                        "Stream has an active finishing callback".into(),
                    ));
                    return Ok(());
                }
                let result = source.completion.borrow().clone();
                self.last_result = Some(match result
                {
                    Some(result) => result?,
                    None => self.io_fault(
                        "ClosedError",
                        "stream.result",
                        "Stream has not completed".into(),
                    ),
                });
            }
            "close" =>
            {
                if let Some(source) = reader(input)
                {
                    if source.busy.get()
                    {
                        if source.owner.get() == crate::runtime::tasks::current_task()
                        {
                            self.last_result = Some(self.io_fault(
                                "LeaseError",
                                "stream.close",
                                "Cannot close a stream from its own active callback".into(),
                            ));
                            return Ok(());
                        }
                        source.cancel.store(true, Ordering::Release);
                        let deadline = Instant::now() + Duration::from_secs(5);
                        while source.busy.get()
                        {
                            if Instant::now() >= deadline
                            {
                                return Err(InterpreterError::new(
                                    location.clone(),
                                    ErrorWhat::CleanupTimeout,
                                ));
                            }
                            crate::runtime::tasks::cooperate().map_err(|error| {
                                InterpreterError::new(
                                    location.clone(),
                                    ErrorWhat::UnsupportedSuspension(error.message),
                                )
                            })?;
                        }
                    }
                    self.stream_release(location, &source)?;
                }
                self.last_result = Some(Value::None);
            }
            _ =>
            {
                return Err(InterpreterError::new(
                    location.clone(),
                    ErrorWhat::InvalidOperand("Unknown stream operation".into()),
                ));
            }
        }
        if let Some(mut value) = self.last_result.take()
        {
            self.activate_failure(location, &mut value, false)?;
            self.last_result = Some(value);
        }
        Ok(())
    }
}

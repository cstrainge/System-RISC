
use std::{rc::Rc, sync::Arc};

use tokio::sync::mpsc::Sender;

use crate::{
    language::{
        data::{
            formats::json::{document::Document, values::encode_document},
            io::{
                adapters::ByteSource,
                queue::{self, Chunk},
            },
            native_value::NativeValue,
            value::{Executable, Value},
        },
        interpreter::{
            ErrorWhat, Interpreter, InterpreterError, InterpreterResult, iteration::Iteration,
        },
        native::NativeOutcome,
        text::location::Location,
    },
    runtime::tasks::{self, NativeFault},
};

use super::{Failure, Operation};

impl Interpreter
{
    pub(in crate::language::interpreter) fn handle_json_source(
        &mut self,
        location: &Location,
        args: &[Value],
    ) -> NativeOutcome
    {
        let [Value::String(format, _), items, rest @ ..] = args
        else
        {
            return NativeOutcome::Failed(InterpreterError::new(
                location.clone(),
                ErrorWhat::ArgumentMismatch("JSON source requires format and items".into()),
            ));
        };
        let Some(reactor) = self.reactor.clone().or_else(tasks::current_reactor)
        else
        {
            return NativeOutcome::Failed(InterpreterError::new(
                location.clone(),
                ErrorWhat::UnsupportedSuspension("No reactor for JSON source".into()),
            ));
        };
        if matches!(items.underlying(), Value::Array(_) | Value::HashMap(_))
            && let Err(message) = crate::language::data::retained::Budget::default().value(items)
        {
            return NativeOutcome::Ready(self.io_fault("LimitError", "json.source", message));
        }
        let caller = self
            .exceptions
            .callers
            .iter()
            .rev()
            .find(|scope| **scope != self.current_scope)
            .cloned()
            .unwrap_or_else(|| self.current_scope.clone());
        let mut producer = self.fork_task();
        producer.current_scope = caller;
        producer.exceptions.frames = self.exceptions.frames.clone();
        let options = rest.first().cloned().unwrap_or(Value::None);
        let operation = Operation::new(
            producer.scope().types.clone(),
            producer.current_scope.clone(),
            &options,
        );
        let mut operation = match operation
        {
            Ok(operation) => operation,
            Err(error) =>
            {
                return match self.json_result(location, Err(error))
                {
                    Ok(value) => NativeOutcome::Ready(value),
                    Err(error) => NativeOutcome::Failed(error),
                };
            }
        };
        if format == "lines" && super::field(&options, "max_bytes").is_none()
        {
            operation.limits.bytes = 1024 * 1024;
        }
        operation.methods = producer
            .scope()
            .conversion_snapshot()
            .into_iter()
            .filter_map(|(key, function)| {
                let mut parts = key.split(':');
                if parts.next()? != "\0method"
                {
                    return None;
                }
                let id = parts.next()?.parse::<usize>().ok()?;
                Some((
                    (
                        crate::language::data::types::TypeId(id),
                        parts.next()?.into(),
                    ),
                    Some(function),
                ))
            })
            .collect();
        operation.frozen = true;
        let snapshot = Value::from_hash_map(
            producer
                .scope()
                .conversion_snapshot()
                .into_iter()
                .map(|(name, function)| {
                    (
                        crate::language::data::map_key::MapKey::String(name),
                        Value::String("method".into(), Executable::Function(function)),
                    )
                })
                .collect(),
        );
        let mut iteration = match Iteration::new(&producer, location, items.clone(), &snapshot)
        {
            Ok(iteration) => iteration,
            Err(error) => return NativeOutcome::Failed(error),
        };
        let (reader, sender) = queue::reader();
        let cancel = reader.0.control.closed.clone();
        let source = match ByteSource::new(reader, true)
        {
            Ok(source) => source,
            Err(error) =>
            {
                return NativeOutcome::Ready(self.io_fault(
                    &error.kind,
                    "json.source",
                    error.message,
                ));
            }
        };
        let location = location.clone();
        let format = format.clone();
        let engine = Rc::downgrade(&reactor);
        let task = reactor.spawn(cancel, move || {
            let region = producer.enter_failure_region();
            let result =
                producer.produce_json(&location, &format, &mut iteration, operation, &sender);
            let result = producer.leave_failure_region(region, result, false);
            if let Err(error) = result
                && let Some(reactor) = engine.upgrade()
            {
                let id = reactor.store_error(error);
                let failed = sender.clone();
                let result = tasks::wait(Box::pin(async move {
                    failed
                        .send(Chunk::Failed(id))
                        .await
                        .map(|_| Box::new(()) as Box<dyn std::any::Any + Send>)
                        .map_err(|_| NativeFault::new("ClosedError", "JSON source consumer closed"))
                }));
                if result.is_err()
                {
                    reactor.take_error(id);
                }
            }
        });
        match task
        {
            Ok(_) => NativeOutcome::Ready(Value::Native(NativeValue::new(source))),
            Err(error) =>
            {
                source.close();
                NativeOutcome::Ready(self.io_fault("LimitError", "json.source", error.to_string()))
            }
        }
    }

    fn source_chunk(
        &mut self,
        location: &Location,
        sender: &Sender<Chunk>,
        bytes: &[u8],
    ) -> InterpreterResult<()>
    {
        for chunk in bytes.chunks(65536)
        {
            let sender = sender.clone();
            let bytes: Arc<[u8]> = Arc::from(chunk);
            let result = tasks::wait(Box::pin(async move {
                sender
                    .send(Chunk::Bytes(bytes))
                    .await
                    .map(|_| Box::new(()) as Box<dyn std::any::Any + Send>)
                    .map_err(|_| NativeFault::new("ClosedError", "JSON source consumer closed"))
            }));
            if let Err(error) = result
            {
                return Err(
                    InterpreterError::new(location.clone(), ErrorWhat::Interrupted).with_context(
                        InterpreterError::new(
                            location.clone(),
                            ErrorWhat::UnsupportedSuspension(error.message),
                        ),
                    ),
                );
            }
        }
        Ok(())
    }

    fn produce_json(
        &mut self,
        location: &Location,
        format: &str,
        iteration: &mut Iteration,
        mut operation: Operation,
        sender: &Sender<Chunk>,
    ) -> InterpreterResult<()>
    {
        let array = format == "array";
        let mut total = 0usize;
        if array
        {
            self.source_chunk(location, sender, b"[")?;
            total = 1;
        }
        let mut first = true;
        while let Some(item) = iteration.next(self, location)?
        {
            let node = self.json_wrap(location, &item, &mut operation, usize::from(array));
            let encoded = node.and_then(|node| {
                encode_document(&Document::new(node), &operation.limits, false)
                    .map(Value::from_string)
                    .map_err(Failure::from)
            });
            let encoded = self.json_result(location, encoded)?;
            self.consume_success(location, &encoded)?;
            let bytes = encoded.as_text().into_bytes();
            if array
            {
                total = total.saturating_add(bytes.len() + usize::from(!first));
                if total + 1 > operation.limits.bytes
                {
                    return Err(InterpreterError::new(
                        location.clone(),
                        ErrorWhat::InvalidOperand(
                            "JSON array source exceeds document byte limit".into(),
                        ),
                    ));
                }
                if !first
                {
                    self.source_chunk(location, sender, b",")?;
                }
            }
            self.source_chunk(location, sender, &bytes)?;
            if !array
            {
                self.source_chunk(location, sender, b"\n")?;
                operation.members = 0;
            }
            first = false;
        }
        if array
        {
            self.source_chunk(location, sender, b"]")?;
        }
        Ok(())
    }
}

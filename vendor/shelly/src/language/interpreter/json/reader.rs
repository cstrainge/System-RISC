
use std::{any::Any, time::Duration};

use crate::{
    language::{
        data::{
            formats::json::stream::{ArrayReader, Reply},
            io::handles::Reader,
            native_value::{NativeData, NativeValue},
            value::Value,
        },
        interpreter::Interpreter,
        native::{NativeOutcome, NativeWait},
        text::location::Location,
    },
    runtime::tasks::NativeFault,
};

use super::{Failure, Operation, text};

fn native<T: NativeData>(value: &Value) -> Option<&T>
{
    if let Value::Native(value) = value.underlying()
    {
        value.downcast()
    }
    else
    {
        None
    }
}

impl Interpreter
{
    pub(in crate::language::interpreter) fn handle_json_reader(
        &mut self,
        location: &Location,
        args: &[Value],
    ) -> NativeOutcome
    {
        let [command, input, rest @ ..] = args
        else
        {
            return NativeOutcome::Ready(self.io_fault(
                "ReadError",
                "json.reader",
                "Missing operation".into(),
            ));
        };
        let command = text(command).unwrap_or("");
        if command == "array"
        {
            let Some(reader) = native::<Reader>(input).cloned()
            else
            {
                return NativeOutcome::Ready(self.io_fault(
                    "ReadError",
                    command,
                    "Expected Reader".into(),
                ));
            };
            let pointer = rest
                .first()
                .filter(|value| !matches!(value, Value::None))
                .map(text)
                .transpose()
                .unwrap_or(None)
                .unwrap_or("");
            let options = rest.get(1).unwrap_or(&Value::None);
            let operation = Operation::new(
                self.scope().types.clone(),
                self.current_scope.clone(),
                options,
            );
            let result = operation.and_then(|operation| {
                ArrayReader::new(reader, pointer, operation.limits)
                    .map(|reader| Value::Native(NativeValue::new(reader)))
                    .map_err(Failure::from)
            });
            return match self.json_result(location, result)
            {
                Ok(value) => NativeOutcome::Ready(value),
                Err(error) => NativeOutcome::Failed(error),
            };
        }
        let Some(reader) = native::<ArrayReader>(input).cloned()
        else
        {
            return NativeOutcome::Ready(self.io_fault(
                "ReadError",
                command,
                "Expected JSON reader".into(),
            ));
        };
        if command == "close"
        {
            reader.close();
            return NativeOutcome::Ready(Value::None);
        }
        if command != "next"
        {
            return NativeOutcome::Ready(self.io_fault(
                "ReadError",
                command,
                "Unknown JSON reader operation".into(),
            ));
        }
        let location = location.clone();
        NativeOutcome::Wait(NativeWait {
            future: Box::pin(async move {
                tokio::time::timeout(Duration::from_secs(30), reader.next())
                    .await
                    .map_err(|_| NativeFault::new("TimeoutError", "JSON reader timed out"))?
                    .map(|reply| Box::new(reply) as Box<dyn Any + Send>)
            }),
            finish: Box::new(move |interpreter, reply| match reply
            {
                Ok(reply) => match *reply.downcast::<Reply>().unwrap()
                {
                    Reply::Item(document) => Ok(document.into_value()),
                    Reply::End => Ok(Value::None),
                    Reply::Failed(error) => interpreter.json_result(&location, Err(error.into())),
                },
                Err(error) =>
                {
                    if let Some(id) = error.script_error
                        && let Some(reactor) = crate::runtime::tasks::current_reactor()
                        && let Some(error) = reactor.take_error(id)
                    {
                        return Err(error);
                    }
                    Ok(interpreter.io_fault(&error.kind, "json.read_array", error.message))
                }
            }),
        })
    }
}

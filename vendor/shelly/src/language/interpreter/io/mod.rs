
use std::{any::Any, future::Future, rc::Rc, sync::Arc, time::Duration};

use crate::language::data::io::lines::Lines;

use base64::{Engine, engine::general_purpose::STANDARD};

use crate::{
    language::{
        data::{
            io::adapters::{ByteSink, ByteSource},
            io::{
                Bytes,
                handles::{Control, Reader, Writer},
            },
            native_value::{NativeData, NativeValue},
            types::StructValue,
            value::{Executable, Value},
        },
        interpreter::{Interpreter, InterpreterResult},
        native::{NativeOutcome, NativeWait},
        text::location::Location,
    },
    runtime::tasks::{NativeFault, NativeReply},
};

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

fn string(value: &Value) -> Option<&str>
{
    if let Value::String(value, Executable::No) = value.underlying()
    {
        Some(value)
    }
    else
    {
        None
    }
}

fn data(value: impl NativeData) -> Value
{
    Value::Native(NativeValue::new(value))
}

impl Interpreter
{
    pub(super) fn io_fault(&self, kind: &str, operation: &str, message: String) -> Value
    {
        let name = format!("__io_{}", kind);
        let definition = self.scope().types.get(
            self.scope()
                .types
                .native_types
                .get(&name)
                .unwrap_or(&self.scope().types.native_types["__io_ReadError"])
                .id,
        );
        Value::Struct(Rc::new(StructValue {
            definition,
            constructing: false,
            failure: None,
            fields: vec![
                Value::from_string(message),
                Value::from_string(operation.into()),
                Value::None,
            ],
        }))
    }

    pub(super) fn native_wait<T: Any + Send>(
        &self,
        operation: &str,
        future: impl Future<Output = Result<T, NativeFault>> + Send + 'static,
        finish: impl FnOnce(T) -> Value + 'static,
    ) -> NativeOutcome
    {
        let operation = operation.to_string();
        let location = self.exceptions.last_location.clone();
        NativeOutcome::Wait(NativeWait {
            future: Box::pin(async move {
                future
                    .await
                    .map(|value| Box::new(value) as Box<dyn Any + Send>)
            }),
            finish: Box::new(move |interpreter, reply: NativeReply| {
                Ok(match reply
                {
                    Ok(value) => finish(*value.downcast::<T>().expect("Native reply type")),
                    Err(error) =>
                    {
                        if let Some(id) = error.script_error
                            && let Some(reactor) = crate::runtime::tasks::current_reactor()
                            && let Some(error) = reactor.take_error(id)
                        {
                            return Err(error);
                        }
                        let http_delivery =
                            matches!(operation.as_str(), "http.send" | "http.upload.finish")
                                && matches!(error.kind.as_str(), "ReadError" | "TimeoutError");
                        let mut value = if http_delivery
                        {
                            let id =
                                interpreter.scope().types.native_types["__http_TransportError"].id;
                            Value::Struct(Rc::new(StructValue {
                                definition: interpreter.scope().types.get(id),
                                fields: vec![
                                    Value::from_string(error.message),
                                    Value::from_string(operation.clone()),
                                    Value::from_string("Unknown".into()),
                                    Value::None,
                                ],
                                constructing: false,
                                failure: None,
                            }))
                        }
                        else
                        {
                            interpreter.io_fault(&error.kind, &operation, error.message)
                        };
                        if let Value::Struct(item) = &mut value
                        {
                            Rc::make_mut(item).fields[if http_delivery { 3 } else { 2 }] = error
                                .code
                                .map_or(Value::None, |code| Value::Integer(code as i64));
                        }
                        interpreter.activate_failure(&location, &mut value, false)?;
                        if let Some(failure) = crate::language::data::failure::occurrence(&value)
                        {
                            failure.borrow_mut().native_origin = Some(Location::new(
                                error.file,
                                error.line as usize,
                                error.column as usize,
                            ));
                        }
                        value
                    }
                })
            }),
        })
    }

    pub(super) fn handle_bytes(&mut self, _: &Location, args: &[Value]) -> NativeOutcome
    {
        let result = (|| {
            let [command, input, rest @ ..] = args
            else
            {
                return Err("Missing byte operation");
            };
            let command = string(command).ok_or("Byte operation requires String")?;
            if command == "from_string"
            {
                let input = string(input).ok_or("Expected String")?;
                if input.len() > 64 * 1024 * 1024
                {
                    return Err("Byte value exceeds its limit");
                }
                return Ok(data(Bytes(Arc::from(input.as_bytes()))));
            }
            if command == "from_array"
            {
                let Value::Array(items) = input.underlying()
                else
                {
                    return Err("Expected array");
                };
                if items.len() > 64 * 1024 * 1024
                {
                    return Err("Byte value exceeds its limit");
                }
                let mut output = Vec::with_capacity(items.len());
                for value in items.iter()
                {
                    let Value::Integer(value) = value.underlying()
                    else
                    {
                        return Err("Expected Integer byte");
                    };
                    output
                        .push(u8::try_from(*value).map_err(|_| "Byte must be between 0 and 255")?);
                }
                return Ok(data(Bytes(output.into())));
            }
            if command == "from_base64"
            {
                let input = string(input).ok_or("Expected String")?;
                if input.len() > 64 * 1024 * 1024
                {
                    return Err("Base64 value exceeds its limit");
                }
                return STANDARD
                    .decode(input)
                    .map(|value| data(Bytes(value.into())))
                    .map_err(|_| "Invalid base64");
            }
            let input = native::<Bytes>(input).ok_or("Expected Bytes")?;
            Ok(match command
            {
                "count" => Value::Integer(input.0.len() as i64),
                "utf8" => Value::from_string(
                    std::str::from_utf8(&input.0)
                        .map_err(|_| "Invalid UTF-8")?
                        .into(),
                ),
                "base64" => Value::from_string(STANDARD.encode(&input.0)),
                "array" =>
                {
                    if input.0.len() > 100_000
                    {
                        return Err("Byte array exceeds 100000 elements");
                    }
                    Value::from_array(
                        input
                            .0
                            .iter()
                            .map(|byte| Value::Integer(*byte as i64))
                            .collect(),
                    )
                }
                "concat" =>
                {
                    let mut output = input.0.to_vec();
                    for value in rest
                    {
                        let next = native::<Bytes>(value).ok_or("Expected Bytes")?;
                        if output.len().saturating_add(next.0.len()) > 64 * 1024 * 1024
                        {
                            return Err("Byte value exceeds its limit");
                        }
                        output.extend_from_slice(&next.0);
                    }
                    data(Bytes(output.into()))
                }
                "slice" =>
                {
                    let [Value::Integer(start), Value::Integer(end)] = rest
                    else
                    {
                        return Err("Slice requires start and end Integers");
                    };
                    if *start < 0 || *end < *start || *end as u64 > input.0.len() as u64
                    {
                        return Err("Byte slice is out of range");
                    }
                    data(Bytes(Arc::from(&input.0[*start as usize..*end as usize])))
                }
                _ => return Err("Unknown byte operation"),
            })
        })();
        NativeOutcome::Ready(
            result.unwrap_or_else(|message| self.io_fault("DecodeError", "bytes", message.into())),
        )
    }

    pub(super) fn handle_io(&mut self, _: &Location, args: &[Value]) -> NativeOutcome
    {
        let [command, input, rest @ ..] = args
        else
        {
            return NativeOutcome::Ready(self.io_fault(
                "OpenError",
                "io",
                "Missing I/O operation".into(),
            ));
        };
        let command = string(command).unwrap_or("");
        match command
        {
            "lines" | "records" =>
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
                let maximum = rest
                    .first()
                    .and_then(|value| {
                        if let Value::Integer(value) = value
                        {
                            Some(*value)
                        }
                        else
                        {
                            None
                        }
                    })
                    .unwrap_or(1048576);
                if maximum < 1 || maximum > 16777216
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "LimitError",
                        command,
                        "Line maximum must be 1..16777216".into(),
                    ));
                }
                let separator = if command == "records"
                {
                    match rest.get(1)
                    {
                        Some(Value::Integer(value)) if (0..=255).contains(value) =>
                        {
                            Some(*value as u8)
                        }
                        _ =>
                        {
                            return NativeOutcome::Ready(self.io_fault(
                                "DecodeError",
                                command,
                                "Record separator must be a byte 0..255".into(),
                            ));
                        }
                    }
                }
                else
                {
                    None
                };
                NativeOutcome::Ready(
                    Lines::separated(
                        reader,
                        maximum as usize,
                        rest.get(1).is_some_and(Value::as_bool),
                        separator,
                    )
                    .map(data)
                    .unwrap_or_else(|error| self.io_fault(&error.kind, command, error.message)),
                )
            }
            "read_line" =>
            {
                let Some(lines) = native::<Lines>(input).cloned()
                else
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "ReadError",
                        command,
                        "Expected line reader".into(),
                    ));
                };
                self.native_wait(command, async move { lines.next().await }, |value| {
                    value.map_or(Value::None, data)
                })
            }
            "byte_sink" =>
            {
                let Some(writer) = native::<Writer>(input).cloned()
                else
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "WriteError",
                        command,
                        "Expected Writer".into(),
                    ));
                };
                NativeOutcome::Ready(
                    ByteSink::new(writer, rest.first().is_some_and(Value::as_bool))
                        .map(data)
                        .unwrap_or_else(|error| self.io_fault(&error.kind, command, error.message)),
                )
            }
            "lease_writer" =>
            {
                let Some(writer) = native::<Writer>(input)
                else
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "LeaseError",
                        command,
                        "Expected Writer".into(),
                    ));
                };
                NativeOutcome::Ready(
                    writer
                        .leased()
                        .map(data)
                        .unwrap_or_else(|error| self.io_fault(&error.kind, command, error.message)),
                )
            }
            "shutdown_write" =>
            {
                let Some(writer) = native::<Writer>(input).cloned()
                else
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "WriteError",
                        command,
                        "Expected Writer".into(),
                    ));
                };
                self.native_wait(command, async move { writer.shutdown().await }, |_| {
                    Value::None
                })
            }
            "lease_reader" =>
            {
                let Some(reader) = native::<Reader>(input)
                else
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "LeaseError",
                        command,
                        "Expected Reader".into(),
                    ));
                };
                NativeOutcome::Ready(
                    reader
                        .leased()
                        .map(data)
                        .unwrap_or_else(|error| self.io_fault(&error.kind, command, error.message)),
                )
            }
            "byte_source" =>
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
                let owned = rest.first().is_some_and(Value::as_bool);
                NativeOutcome::Ready(
                    ByteSource::new(reader, owned)
                        .map(data)
                        .unwrap_or_else(|error| self.io_fault(&error.kind, command, error.message)),
                )
            }
            "source_reader" =>
            {
                let Some(source) = native::<ByteSource>(input)
                else
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "ReadError",
                        command,
                        "Expected ByteSource".into(),
                    ));
                };
                if source.0.closed.load(std::sync::atomic::Ordering::Acquire)
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "ClosedError",
                        command,
                        "Byte source is closed".into(),
                    ));
                }
                NativeOutcome::Ready(data(source.0.reader.borrowed()))
            }
            "memory_reader" =>
            {
                if let Some(bytes) = native::<Bytes>(input)
                {
                    NativeOutcome::Ready(data(Reader::memory(bytes.clone())))
                }
                else
                {
                    NativeOutcome::Ready(self.io_fault(
                        "OpenError",
                        command,
                        "Expected Bytes".into(),
                    ))
                }
            }
            "open_reader" | "open_writer" =>
            {
                let Some(path) = string(input)
                else
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "OpenError",
                        command,
                        "Expected path String".into(),
                    ));
                };
                let path = path.to_string();
                if command == "open_reader"
                {
                    self.native_wait(
                        command,
                        async move {
                            let file = tokio::time::timeout(
                                Duration::from_secs(30),
                                crate::runtime::blocking::run(move || std::fs::File::open(path)),
                            )
                            .await
                            .map_err(|_| NativeFault::new("TimeoutError", "File open timed out"))??
                            .map_err(|error| NativeFault::io("OpenError", error))?;
                            Ok(Reader::new(
                                crate::language::data::io::file::File::new(file),
                                Control::new(),
                            ))
                        },
                        data,
                    )
                }
                else
                {
                    self.native_wait(
                        command,
                        async move {
                            let file = tokio::time::timeout(
                                Duration::from_secs(30),
                                crate::runtime::blocking::run(move || std::fs::File::create(path)),
                            )
                            .await
                            .map_err(|_| NativeFault::new("TimeoutError", "File open timed out"))??
                            .map_err(|error| NativeFault::io("OpenError", error))?;
                            Ok(Writer::new(
                                crate::language::data::io::file::File::new(file),
                                Control::new(),
                            ))
                        },
                        data,
                    )
                }
            }
            "read" =>
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
                let maximum = rest
                    .first()
                    .and_then(|value| {
                        if let Value::Integer(value) = value
                        {
                            Some(*value)
                        }
                        else
                        {
                            None
                        }
                    })
                    .unwrap_or(65536);
                if maximum < 1 || maximum > 16 * 1024 * 1024
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "LimitError",
                        command,
                        "read maximum must be 1..16777216".into(),
                    ));
                }
                self.native_wait(
                    command,
                    async move {
                        tokio::time::timeout(Duration::from_secs(30), reader.read(maximum as usize))
                            .await
                            .map_err(|_| NativeFault::new("TimeoutError", "Read timed out"))?
                    },
                    |value| value.map_or(Value::None, data),
                )
            }
            "write" =>
            {
                let writer = native::<Writer>(input).cloned();
                let bytes = rest.first().and_then(native::<Bytes>).cloned();
                let (Some(writer), Some(bytes)) = (writer, bytes)
                else
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "WriteError",
                        command,
                        "Expected Writer and Bytes".into(),
                    ));
                };
                self.native_wait(
                    command,
                    async move {
                        tokio::time::timeout(Duration::from_secs(30), writer.write(bytes))
                            .await
                            .map_err(|_| NativeFault::new("TimeoutError", "Write timed out"))?
                    },
                    |count| Value::Integer(count as i64),
                )
            }
            "flush" =>
            {
                let Some(writer) = native::<Writer>(input).cloned()
                else
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "WriteError",
                        command,
                        "Expected Writer".into(),
                    ));
                };
                self.native_wait(
                    command,
                    async move {
                        tokio::time::timeout(Duration::from_secs(30), writer.flush())
                            .await
                            .map_err(|_| NativeFault::new("TimeoutError", "Flush timed out"))?
                    },
                    |_| Value::None,
                )
            }
            _ => NativeOutcome::Ready(self.io_fault(
                "OpenError",
                command,
                "Unknown I/O operation".into(),
            )),
        }
    }

    pub(super) fn handle_io_close(
        &mut self,
        location: &Location,
        args: &[Value],
    ) -> InterpreterResult<()>
    {
        self.last_result = Some(match args
        {
            [value] if native::<Lines>(value).is_some() =>
            {
                native::<Lines>(value).unwrap().close();
                Value::None
            }
            [value] if native::<Reader>(value).is_some() =>
            {
                native::<Reader>(value).unwrap().close();
                Value::None
            }
            [value] if native::<Writer>(value).is_some() =>
            {
                native::<Writer>(value).unwrap().close();
                Value::None
            }
            [value] if native::<ByteSource>(value).is_some() =>
            {
                native::<ByteSource>(value).unwrap().close();
                Value::None
            }
            [value] if native::<ByteSink>(value).is_some() =>
            {
                native::<ByteSink>(value).unwrap().close();
                Value::None
            }
            _ => self.io_fault("ClosedError", "close", "Expected reader or writer".into()),
        });
        if let Some(mut value) = self.last_result.take()
        {
            self.activate_failure(location, &mut value, false)?;
            self.last_result = Some(value);
        }
        Ok(())
    }
}

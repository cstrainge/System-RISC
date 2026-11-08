use std::{
    rc::Rc,
    mem::take,
    sync::{Arc, atomic::Ordering},
};

use crate::language::{
    bytecode::PipelineStage,
    data::{
        types::{TypeId, TypeKind, TypeRegistry},
        io::{
            Bytes,
            handles::{Reader, Writer},
            adapters::{ByteSource, ByteSink},
        },
        native_value::NativeValue,
        value::{ExecResult, Executable, Value},
        methods::MethodDefinition,
    },
    interpreter::{ErrorWhat, Interpreter, InterpreterError, InterpreterResult},
    interpreter::iteration::Iteration,
    interpreter::pipeline::{Context, Data, Step, Stream},
};


fn array_type(types: &TypeRegistry, id: TypeId) -> Option<TypeId>
{
    let id = types.underlying_type(id);
    match types.get(id).kind
    {
        TypeKind::Array(_) | TypeKind::FixedArray(_) => Some(id),
        TypeKind::Optional(inner) => array_type(types, inner),
        _ => None,
    }
}


#[cfg(unix)]
use super::process::External;

pub(super) struct StageStream<'a>
{
    stage: &'a PipelineStage,
    index: usize,
    input: Option<Box<dyn Stream + 'a>>,
    #[cfg(unix)]
    external: Option<External>,
    bytes: Vec<u8>,
    pending: Option<Data>,
    source_done: bool,
    source_value: Option<Value>,
    acquired_input: bool,
    ended: bool,
    destination: Option<Vec<Value>>,
    destination_ready: bool,
    retained: crate::language::data::retained::Budget,
    buffering: bool,
    flushed: bool,
    prepared: Option<(Value, Vec<Value>)>,
    #[cfg(unix)]
    per_item_external: bool,
    external_started: bool,
    iterate_source: bool,
    iteration: Option<Iteration>,
    byte_source: Option<ByteSource>,
    byte_sink: Option<ByteSink>,
    #[cfg(not(target_os = "systemrisc"))]
    upload: Option<Value>,
}


impl<'a> StageStream<'a>
{
    pub fn new(
        stage: &'a PipelineStage,
        index: usize,
        input: Option<Box<dyn Stream + 'a>>,
        iterate_source: bool,
    ) -> Self
    {
        Self { stage, index, input,
            #[cfg(unix)]
            external: None,
            bytes: Vec::new(), pending: None,
            source_done: false, source_value: None, acquired_input: false, ended: false,
                destination: None, destination_ready: false, retained: Default::default(),
            buffering: false, flushed: false, prepared: None,
            #[cfg(unix)]
            per_item_external: false,
            external_started: false, iterate_source, iteration: None, byte_source: None,
            byte_sink: None,
            #[cfg(not(target_os = "systemrisc"))]
            upload: None }
    }

    #[track_caller]
    fn error(&self, message: impl Into<String>) -> InterpreterError
    {
        InterpreterError::new(
            self.stage.location.clone(),
            ErrorWhat::InvalidOperand(message.into())
        )
    }

    fn prepare(&self, interpreter: &mut Interpreter, input: Option<&Value>)
        -> InterpreterResult<(Value, Vec<Value>)>
    {
        let depth = interpreter.pipeline_inputs.len();
        if let Some(input) = input { interpreter.pipeline_inputs.push(input.clone()); }
        interpreter.last_result = None;
        let result = interpreter.execute_instructions(&self.stage.code);
        interpreter.pipeline_inputs.truncate(depth);
        result?;
        let value = interpreter.last_result.take().unwrap_or(Value::None);
        if self.stage.explicit_arguments
        {
            let Value::Array(parts) = value else { unreachable!(); };
            let Value::Array(arguments) = &parts[1] else { unreachable!(); };
            Ok((parts[0].clone(), arguments.to_vec()))
        }
        else { Ok((value, Vec::new())) }
    }

    fn sink(
        &mut self,
        interpreter: &mut Interpreter,
        value: &Value,
    ) -> InterpreterResult<Option<ByteSink>>
    {
        if let Some(sink) = &self.byte_sink
        {
            return Ok(Some(sink.clone()));
        }
        let Value::Native(native) = value.underlying()
        else
        {
            return Ok(None);
        };
        if let Some(sink) = native.downcast::<ByteSink>()
        {
            return Ok(Some(sink.clone()));
        }
        #[cfg(not(target_os = "systemrisc"))]
        if let Some(sink) = Interpreter::http_upload_sink(value)
        {
            return Ok(Some(sink));
        }
        let Some(writer) = native.downcast::<Writer>()
        else
        {
            return Ok(None);
        };
        match ByteSink::new(writer.clone(), false)
        {
            Ok(sink) =>
            {
                self.byte_sink = Some(sink.clone());
                Ok(Some(sink))
            }
            Err(error) =>
            {
                let mut value = interpreter.io_fault(&error.kind, "pipeline.writer", error.message);
                interpreter.activate_failure(&self.stage.location, &mut value, false)?;
                Err(interpreter.raise_failure(&self.stage.location, value)?)
            }
        }
    }

    fn invoke(
        &mut self,
        interpreter: &mut Interpreter,
        context: &mut Context,
        data: Option<Data>,
    ) -> InterpreterResult<Step>
    {
        let sink_value = self.stage.destination.as_ref().and_then(|name|
            interpreter.variable_binding(name)).map(|binding| binding.borrow().value.clone())
            .filter(|value| matches!(value.underlying(), Value::Native(native)
                if native.downcast::<ByteSink>().is_some() ||
                    native.downcast::<Writer>().is_some() || is_upload(value)));
        if self.stage.destination.is_some() && sink_value.is_none()
        {
            let Some(Data::Value(value)) = data else { unreachable!(); };
            self.assign(interpreter, value.clone())?;
            return Ok(Step::Data(Data::Value(value)));
        }
        let input = match &data { Some(Data::Value(value)) => Some(value.clone()), _ => None };
        let (mut executable, mut arguments) = if let Some(sink) = sink_value { (sink, Vec::new()) }
            else if let Some(prepared) = self.prepared.take()
            { prepared } else { self.prepare(interpreter, input.as_ref())? };
        if let Some(sink) = self.sink(interpreter, &executable)?
        {
            #[cfg(not(target_os = "systemrisc"))]
            if Interpreter::http_upload_sink(&executable).is_some()
            {
                self.upload = Some(executable.clone());
            }
            #[cfg(not(target_os = "systemrisc"))]
            if self
                .upload
                .as_ref()
                .is_some_and(Interpreter::http_upload_ready)
            {
                if let Some(input) = &mut self.input
                {
                    input.cancel(interpreter, context)?;
                }
                return self.finish(interpreter);
            }
            if sink.0.closed.load(Ordering::Acquire)
            {
                return Err(self.error("Byte sink is closed"));
            }
            self.byte_sink = Some(sink.clone());
            self.prepared = Some((executable.clone(), Vec::new()));
            if let Some(data) = data
            {
                let bytes = match data
                {
                    Data::Bytes(bytes) => bytes,
                    Data::Value(value) => interpreter.pipeline_bytes(self.stage, value)?,
                };
                let function = interpreter.native_functions["__io"].clone();
                interpreter.execute_value(
                    &self.stage.location,
                    Value::String("__io".into(), Executable::Native(function)),
                    vec![
                        Value::from_string("write".into()),
                        Value::Native(NativeValue::new(sink.0.writer.clone())),
                        Value::Native(NativeValue::new(Bytes(Arc::from(bytes)))),
                    ],
                )?;
                let result = interpreter.last_result.take().unwrap_or(Value::None);
                #[cfg(not(target_os = "systemrisc"))]
                if self
                    .upload
                    .as_ref()
                    .is_some_and(Interpreter::http_upload_ready)
                {
                    if let Some(failure) = crate::language::data::failure::occurrence(&result)
                    {
                        let mut failure = failure.borrow_mut();
                        failure.status =
                            crate::language::data::failure::FailureStatus::Acknowledged;
                        interpreter.exceptions.pending.remove(&failure.id);
                    }
                    if let Some(input) = &mut self.input
                    {
                        input.cancel(interpreter, context)?;
                    }
                    return self.finish(interpreter);
                }
                interpreter.consume_success(&self.stage.location, &result)?;
                sink.0
                    .count
                    .fetch_add(result.as_integer() as u64, Ordering::AcqRel);
            }
            return Ok(Step::Pending);
        }
        let mut external_name = None;
        if    self.stage.call && !executable.is_callable()
           && (self.stage.command_word || self.stage.explicit_arguments
                || executable.is_executable())
        {
            let name = Interpreter::command_name(&self.stage.location, executable.clone())?;
            let external_only = executable.is_external();
            let (name, defaults, home) = if external_only
                { (name, Vec::new(), interpreter.current_scope.clone()) }
                else { interpreter.resolve_alias(&self.stage.location, &name)? };
            let mut resolved = defaults.into_iter().map(Value::from_string).collect::<Vec<_>>();
            resolved.extend(arguments);
            arguments = resolved;
            let scope = interpreter.scopes.get(&home).unwrap();
            if !external_only && let Some(function) = scope.function(&name)
            { executable = Value::String(name, Executable::Function(function)); }
            else if !external_only && let Some(function) = interpreter.module_native_function(&name)
            { executable = Value::String(name, Executable::Native(function)); }
            else { external_name = Some(name); }
        }
        if self.stage.call && matches!(executable.underlying(),
            Value::String(_, Executable::Native(function)) if function.name == "exec")
        {
            // Pipeline stages are already child processes. Launch the exec target directly,
            // retaining the pipeline's input, output, status and cancellation ownership.
            if arguments.first().is_some_and(|value| value.as_text() == "--")
            { arguments.remove(0); }
            if arguments.is_empty()
            { return Err(self.error("exec in a pipeline requires a program")); }
            external_name = Some(arguments.remove(0).as_text());
        }
        #[cfg(not(unix))]
        if external_name.is_some()
        {
            return Err(self.error("External process stages are unavailable on this platform"));
        }
        #[cfg(unix)]
        if let Some(name) = external_name
        {
            let Some(mut external) = External::new(interpreter, self.stage, &name, &arguments,
                self.index == 0)? else { return Ok(Step::End); };
            self.per_item_external = self.index > 0 && self.stage.input_used;
            if self.per_item_external { external.close_input(); }
            else { self.pending = data; }
            if self.index == 0 { external.close_input(); }
            self.external = Some(external);
            self.external_started = true;
            return Ok(Step::Pending);
        }
        if let Some(Data::Bytes(bytes)) = data
        {
            // Unannotated byte streams become one String at a Shelly value boundary.
            if self.bytes.len().saturating_add(bytes.len()) > 16 * 1024 * 1024
            { return Err(self.error("Pipeline byte buffer exceeds 16 MiB")); }
            self.bytes.extend(bytes);
            self.buffering = true;
            self.prepared = Some((executable, arguments));
            return Ok(Step::Pending);
        }
        if self.stage.call && executable.is_callable()
        {
            if self.index > 0
            {
                if self.stage.explicit_arguments && !self.stage.input_used
                { return Err(self.error(
                    "A pipeline function with explicit arguments must use bare $")); }
                let getter = self.stage.property && match executable.underlying()
                    {
                        Value::String(_, Executable::Method(method)) => match &method.definition
                            {
                                MethodDefinition::Builtin(item) => item.argument_count == 0,
                                MethodDefinition::User(item) =>
                                    item.arguments.len() == 1 && !item.variadic,
                            },
                        _ => false,
                    };
                if !self.stage.explicit_arguments && !getter
                { arguments.push(input.clone().unwrap_or(Value::None)); }
            }
            let depth = interpreter.pipeline_inputs.len();
            if let Some(input) = input { interpreter.pipeline_inputs.push(input); }
            interpreter.last_result = None;
            let redirection_depth = interpreter.redirections.len();
            let result = (||
                {
                    self.begin_redirects(interpreter)?;
                    interpreter.execute_value(&self.stage.location, executable, arguments)
                })();
            let finished = interpreter.finish_redirections(redirection_depth);
            let result = result.and(finished);
            interpreter.pipeline_inputs.truncate(depth);
            match result
            {
                Err(InterpreterError { what: ErrorWhat::ExecutableBadReturn(code), .. }) =>
                    { context.record(self.index, ExecResult::Value(code)); return Ok(Step::End); },
                Err(InterpreterError { what: ErrorWhat::ExecutableSignaled, .. }) =>
                    { context.record(self.index, ExecResult::Signaled); return Ok(Step::End); },
                other => other?,
            }
            executable = interpreter.last_result.take().unwrap_or(Value::None);
        }
        else if !self.stage.redirects.is_empty()
        {
            let depth = interpreter.pipeline_inputs.len();
            if let Some(input) = input { interpreter.pipeline_inputs.push(input); }
            let redirection_depth = interpreter.redirections.len();
            let result = (||
                {
                    self.begin_redirects(interpreter)?;
                    interpreter.redirect_source(&self.stage.location, executable, false)
                })();
            let finished = interpreter.finish_redirections(redirection_depth);
            interpreter.pipeline_inputs.truncate(depth);
            result.and(finished)?;
            executable = interpreter.last_result.take().unwrap_or(Value::None);
        }
        interpreter.consume_success(&self.stage.location, &executable)?;
        Ok(Step::Data(Data::Value(executable)))
    }

    fn begin_redirects(&self, interpreter: &mut Interpreter) -> InterpreterResult<()>
    {
        for redirect in &self.stage.redirects
        {
            interpreter.execute_instructions(&redirect.code)?;
            let target = interpreter.last_result.take().unwrap_or(Value::None);
            interpreter.begin_redirection(&redirect.location, redirect.stream,
                redirect.variable, target)?;
        }
        Ok(())
    }

    fn initialize_destination(&mut self, interpreter: &Interpreter) -> InterpreterResult<()>
    {
        let name = self.stage.destination.as_ref().unwrap();
        if !self.destination_ready
        {
            let binding = interpreter.variable_binding(name)
                .ok_or_else(|| self.error(
                    format!("Variable '{}' not found; declare it with let", name)))?;
            let binding = binding.borrow();
            let array = binding.type_id.is_some_and(|id|
                array_type(&interpreter.scope().types, id).is_some())
                || matches!(binding.value.underlying(), Value::Array(_));
            if array { self.destination = Some(Vec::new()); }
            self.destination_ready = true;
        }
        Ok(())
    }

    fn assign(&mut self, interpreter: &mut Interpreter, mut value: Value) -> InterpreterResult<()>
    {
        self.initialize_destination(interpreter)?;
        let name = self.stage.destination.as_ref().unwrap();
        if let Some(values) = &mut self.destination
        {
            if let Some(id) = interpreter.variable_binding(name).unwrap().borrow().type_id
            {
                let types = &interpreter.scope().types;
                let definition = array_type(types, id).map(|id| types.get(id));
                let item = match definition.as_ref().map(|definition| &definition.kind)
                    { Some(TypeKind::Array(item)) => Some(*item),
                      Some(TypeKind::FixedArray(items)) => items.get(values.len()).copied(),
                      _ => None };
                if let Some(item) = item
                {
                    value = interpreter.scope().types.coerce(item, value).map_err(|message|
                        InterpreterError::new(
                            self.stage.location.clone(),
                            ErrorWhat::InvalidOperand(
                                format!("Pipeline destination: {}", message)
                            )
                        ))?;
                }
            }
            if let Err(message) = self.retained.value(&value)
            {
                let mut error = interpreter.io_fault("LimitError", "pipeline.collect", message);
                interpreter.activate_failure(&self.stage.location, &mut error, false)?;
                return Err(interpreter.raise_failure(&self.stage.location, error)?);
            }
            values.push(value);
            Ok(())
        }
        else { interpreter.write_variable(&self.stage.location, name, value) }
    }

    fn finish(&mut self, interpreter: &mut Interpreter) -> InterpreterResult<Step>
    {
        if let Some(source) = self.byte_source.take()
        {
            source.close();
        }
        #[cfg(not(target_os = "systemrisc"))]
        if let Some(upload) = self.upload.take()
        {
            let function = interpreter.native_functions["__http_upload"].clone();
            interpreter.execute_value(
                &self.stage.location,
                Value::String("__http_upload".into(), Executable::Native(function)),
                vec![upload],
            )?;
            let response = interpreter.last_result.take().unwrap_or(Value::None);
            interpreter.consume_success(&self.stage.location, &response)?;
            self.byte_sink.take();
            self.ended = true;
            return Ok(Step::Data(Data::Value(response)));
        }
        if let Some(sink) = self.byte_sink.clone()
        {
            let function = interpreter.native_functions["__io"].clone();
            interpreter.execute_value(
                &self.stage.location,
                Value::String("__io".into(), Executable::Native(function)),
                vec![
                    Value::from_string("flush".into()),
                    Value::Native(NativeValue::new(sink.0.writer.clone())),
                ],
            )?;
            let result = interpreter.last_result.take().unwrap_or(Value::None);
            interpreter.consume_success(&self.stage.location, &result)?;
            sink.close();
            self.byte_sink.take();
            self.ended = true;
            return Ok(Step::Data(Data::Value(Value::Integer(
                sink.0.count.load(Ordering::Acquire) as i64,
            ))));
        }
        if self.stage.destination.is_some() && !self.destination_ready
        { self.initialize_destination(interpreter)?; }
        if let Some(values) = self.destination.take()
        {
            interpreter.write_variable(&self.stage.location,
                self.stage.destination.as_ref().unwrap(),
                Value::from_array(values))?;
        }
        self.ended = true;
        Ok(Step::End)
    }
}


impl Stream for StageStream<'_>
{
    fn poll(
        &mut self,
        interpreter: &mut Interpreter,
        context: &mut Context,
    ) -> InterpreterResult<Step>
    {
        if self.ended { return Ok(Step::End); }
        if self.index > 0 && !self.acquired_input
        {
            let input = self.input.as_mut().unwrap().poll(interpreter, context)?;
            match input
            {
                Step::Pending => return Ok(Step::Pending),
                Step::Data(data) => self.pending = Some(data),
                Step::End =>
                {}
            }
            self.acquired_input = true;
        }
        if self.byte_sink.is_none()
            && let Some(value) = self
                .stage
                .destination
                .as_ref()
                .and_then(|name| interpreter.variable_binding(name))
                .map(|binding| binding.borrow().value.clone())
            && let Some(sink) = self.sink(interpreter, &value)?
        {
            self.byte_sink = Some(sink);
            #[cfg(not(target_os = "systemrisc"))]
            if Interpreter::http_upload_sink(&value).is_some()
            {
                self.upload = Some(value);
            }
        }
        if self.index > 0
            && !self.stage.input_used
            && self.stage.destination.is_none()
            && self.prepared.is_none()
            && !self.external_started
        {
            let mut prepared = self.prepare(interpreter, None)?;
            if let Value::String(_, Executable::Function(function)) = &prepared.0
                && function
                    .return_type
                    .and_then(|id| interpreter.scope().types.success_type(id))
                    .is_some_and(|id| {
                        matches!(
                            interpreter
                                .scope()
                                .types
                                .get(interpreter.scope().types.underlying_type(id))
                                .name
                                .as_str(),
                            "__io_ByteSink" | "__io_Writer" | "__http_Upload"
                        )
                    })
            {
                interpreter.execute_value(&self.stage.location, prepared.0, prepared.1)?;
                let value = interpreter.last_result.take().unwrap_or(Value::None);
                interpreter.consume_success(&self.stage.location, &value)?;
                prepared = (value, Vec::new());
            }
            if let Some(sink) = self.sink(interpreter, &prepared.0)?
            {
                self.byte_sink = Some(sink);
                #[cfg(not(target_os = "systemrisc"))]
                if Interpreter::http_upload_sink(&prepared.0).is_some()
                {
                    self.upload = Some(prepared.0.clone());
                }
            }
            self.prepared = Some(prepared);
        }
        #[cfg(unix)]
        if self.external.is_some()
        {
            let result = self.external.as_mut().unwrap().poll(interpreter, context, self.index)?;
            if !matches!(result, Step::Pending)
            {
                if matches!(result, Step::End)
                {
                    if self.per_item_external
                    {
                        self.external.take();
                        return Ok(Step::Pending);
                    }
                    if let Some(input) = self.input.as_mut()
                    { input.cancel(interpreter, context)?; }
                    return self.finish(interpreter);
                }
                return Ok(result);
            }
            if self.external.as_ref().unwrap().wants_input()
            {
                let next = if let Some(data) = self.pending.take() { Step::Data(data) }
                    else if let Some(input) = self.input.as_mut()
                    { input.poll(interpreter, context)? }
                    else { Step::End };
                match next
                {
                    Step::Data(data) =>
                        {
                            let bytes = match data { Data::Bytes(bytes) => bytes,
                                Data::Value(value) =>
                                    interpreter.pipeline_bytes(self.stage, value)? };
                            self.external.as_mut().unwrap().feed(bytes);
                            if self.per_item_external
                            { self.external.as_mut().unwrap().close_input(); }
                        },
                    Step::End => self.external.as_mut().unwrap().close_input(),
                    Step::Pending => {},
                }
            }
            return Ok(Step::Pending);
        }
        if self.index == 0
        {
            if let Some(iteration) = &mut self.iteration
            {
                return match iteration.next(interpreter, &self.stage.location)
                    {
                        Ok(Some(value)) =>
                            {
                                interpreter.consume_success(&self.stage.location, &value)?;
                                if self.byte_source.is_some()
                                    && let Value::Native(native) = value.underlying()
                                    && let Some(bytes) = native.downcast::<Bytes>()
                                { return Ok(Step::Data(Data::Bytes(bytes.0.to_vec()))); }
                                Ok(Step::Data(Data::Value(value)))
                            },
                        Ok(None) => self.finish(interpreter),
                        Err(InterpreterError { what: ErrorWhat::ExecutableBadReturn(code), .. }) =>
                            {
                                context.record(self.index, ExecResult::Value(code));
                                self.finish(interpreter)
                            },
                        Err(InterpreterError { what: ErrorWhat::ExecutableSignaled, .. }) =>
                            {
                                context.record(self.index, ExecResult::Signaled);
                                self.finish(interpreter)
                            },
                        Err(error) => Err(error),
                    };
            }
            if self.source_done { return self.finish(interpreter); }
            self.source_done = true;
            let result = self.invoke(interpreter, context, None)?;
            if self.iterate_source && let Step::Data(Data::Value(value)) = result
            {
                interpreter.adopt_stream_operation(&value);
                self.source_value = Some(value.clone());
                if let Value::Native(native) = value.underlying()
                {
                    if let Some(source) = native.downcast::<ByteSource>()
                    { self.byte_source = Some(source.clone()); }
                    else if let Some(reader) = native.downcast::<Reader>()
                    {
                        match ByteSource::new(reader.clone(), false)
                        {
                            Ok(source) => self.byte_source = Some(source),
                            Err(error) =>
                                {
                                    let mut value = interpreter.io_fault(&error.kind,
                                        "pipeline.reader", error.message);
                                    interpreter.activate_failure(&self.stage.location, &mut
                                        value, false)?;
                                    return Err(interpreter.raise_failure(&self.stage.location,
                                        value)?);
                                },
                        }
                    }
                }
                let value = if let Some(source) = &self.byte_source
                    && let Value::Native(native) = value.underlying()
                    && native.downcast::<Reader>().is_some()
                {
                    if let Value::Named(alias) = &value
                    { Value::Named(Rc::new(crate::language::data::types::NamedValue {
                        definition: alias.definition.clone(), value:
                            Value::Native(NativeValue::new(source.0.reader.clone())) })) }
                    else { Value::Native(NativeValue::new(source.0.reader.clone())) }
                }
                else { value.clone() };
                self.iteration = Iteration::try_new(interpreter, &self.stage.location,
                    value.clone(), &self.stage.conversions)?;
                if self.iteration.is_some() { return Ok(Step::Pending); }
                return Ok(Step::Data(Data::Value(value)));
            }
            return Ok(result);
        }
        let next = if let Some(data) = self.pending.take()
        {
            Step::Data(data)
        }
        else
        {
            self.input.as_mut().unwrap().poll(interpreter, context)?
        };
        match next
        {
            Step::Pending => Ok(Step::Pending),
            Step::End =>
                {
                    if    !self.flushed
                       && (self.buffering || self.input.as_ref().unwrap().bytes_output())
                    {
                        self.flushed = true;
                        let bytes = take(&mut self.bytes);
                        let binary = self.stage.destination.as_ref().and_then(|name|
                            interpreter.variable_binding(name)).and_then(|binding|
                            binding.borrow().type_id).is_some_and(|id|
                                interpreter.scope().types.get(
                                    interpreter.scope().types.underlying_type(id)).name ==
                                        "__io_Bytes");
                        let value = if binary {
                            Value::Native(NativeValue::new(Bytes(Arc::from(bytes)))) }
                            else { Value::from_string(String::from_utf8(bytes)
                                .map_err(|error| self.error(error.to_string()))?) };
                        return self.invoke(interpreter, context,
                            Some(Data::Value(value)));
                    }
                    self.finish(interpreter)
                },
            Step::Data(Data::Bytes(bytes)) if self.byte_sink.is_none()
                && (self.buffering || self.stage.input_used || self.stage.destination.is_some()) =>
                {
                    if self.bytes.len().saturating_add(bytes.len()) > 16 * 1024 * 1024
                    { return Err(self.error("Pipeline byte buffer exceeds 16 MiB")); }
                    self.bytes.extend(bytes); self.buffering = true; Ok(Step::Pending)
                },
            Step::Data(data) => self.invoke(interpreter, context, Some(data)),
        }
    }

    fn cancel(
        &mut self,
        interpreter: &mut Interpreter,
        context: &mut Context,
    ) -> InterpreterResult<()>
    {
        #[cfg(not(unix))]
        let result = Ok(());
        #[cfg(unix)]
        let result = if let Some(external) = self.external.as_mut()
            { external.cancel(interpreter, context, self.index) } else { Ok(()) };
        let upstream = if let Some(input) = self.input.as_mut()
            { input.cancel(interpreter, context) } else { Ok(()) };
        self.iteration = None;
        let script_cleanup = if let Some(value) = self.source_value.take()
        {
            interpreter.cancel_stream_source(&self.stage.location, &value)
        }
        else
        {
            Ok(())
        };
        if let Some(source) = &self.byte_source
        {
            source.close();
        }
        if let Some(sink) = &self.byte_sink
        {
            sink.close();
        }
        self.ended = true;
        let result = interpreter.combine_failure(result, upstream, "Cleanup");
        interpreter.combine_failure(result, script_cleanup, "Cleanup")
    }

    fn bytes_output(&self) -> bool
    {
        self.external_started || self.byte_source.is_some()
    }
}

fn is_upload(_value: &Value) -> bool
{
    #[cfg(not(target_os = "systemrisc"))]
    return Interpreter::http_upload_sink(_value).is_some();
    #[cfg(target_os = "systemrisc")]
    false
}

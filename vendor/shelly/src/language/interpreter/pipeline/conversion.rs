
use std::{ collections::HashSet, fmt::Display, mem::take };

use crate::language::{ bytecode::{ PipelineBoundary, PipelineStage },
                       data::{ conversion::conversion_type_name,
                               formats::{ RecordDecoder, map_value }, map_key::MapKey,
                               methods::method_key, types::{ TypeId, TypeKind },
                               value::{ Executable, Value } },
                       interpreter::{ ErrorWhat, Interpreter, InterpreterError, InterpreterResult },
                       interpreter::pipeline::{ Context, Data, Step, Stream } };


pub(super) struct BoundaryStream<'a>
{
    boundary: &'a PipelineBoundary,
    input: Box<dyn Stream + 'a>,
    encoder: Option<String>,
    target: Option<TypeId>,
    rows: Option<RecordDecoder>,
    header: Option<Vec<String>>,
    bytes: Vec<u8>,
    ended: bool,
}


#[track_caller]
fn error(boundary: &PipelineBoundary, source: impl Display,
         message: impl Display)
    -> InterpreterError
{
    let format = boundary.decode.as_deref().or(boundary.annotation.as_deref()).unwrap_or("default");
    let target = boundary.type_id.map(|_| boundary.annotation.as_deref().unwrap())
        .unwrap_or("inferred");
    InterpreterError::new(
        boundary.location.clone(),
        ErrorWhat::InvalidOperand(
            format!(
                "Pipe conversion format '{}', source {}, destination {}: {}",
                format,
                source,
                target,
                message
            )
        )
    )
}


fn conversion_error(boundary: &PipelineBoundary, source: impl Display,
                    cause: InterpreterError) -> InterpreterError
{
    let context = error(boundary, source, &cause.what);
    cause.with_context(context)
}


impl<'a> BoundaryStream<'a>
{
    pub fn new(interpreter: &Interpreter, boundary: &'a PipelineBoundary,
               input: Box<dyn Stream + 'a>) -> InterpreterResult<Self>
    {
        if let Some(format) = &boundary.decode
            && !interpreter.pipeline_format_supported(boundary, format, false)
        { return Err(error(boundary, "bytes", format!("Unsupported format '{}'", format))); }
        let mut encoder = None;
        let mut target = None;
        if let Some(annotation) = &boundary.annotation
        {
            let format = interpreter.pipeline_format_supported(boundary, annotation, true);
            if boundary.decode.is_some() && format && boundary.type_id.is_some()
            { return Err(error(boundary, "bytes",
                format!("Ambiguous format/type '{}'", annotation))); }
            if boundary.decode.is_some() && !format && boundary.type_id.is_some()
            { target = boundary.type_id; }
            else if format { encoder = Some(annotation.clone()); }
            else
            { return Err(error(boundary, "unknown",
                format!("Unsupported format '{}'", annotation))); }
        }
        let rows = boundary.decode.as_deref()
            .filter(|format| matches!(*format, "csv" | "tsv" | "ssv"))
            .map(RecordDecoder::new);
        // An array-of-struct annotation describes a stream of its element records.
        // The final array destination owns collection; the boundary maps one row.
        if rows.is_some() && let Some(id) = target
        {
            let types = &interpreter.scope().types;
            if let TypeKind::Array(item) = types.get(types.underlying_type(id)).kind
                && matches!(types.get(types.underlying_type(item)).kind, TypeKind::Struct(_))
            { target = Some(item); }
        }
        Ok(Self { boundary, input, encoder, target, rows, header: None,
            bytes: Vec::new(), ended: false })
    }

    fn map(&mut self, interpreter: &mut Interpreter, mut value: Value) -> InterpreterResult<Data>
    {
        let source = value.type_name();
        if let Some(target) = self.target
        {
            value = interpreter.pipeline_target(self.boundary, target, value)
                .map_err(|error_value| conversion_error(self.boundary, &source, error_value))?;
        }
        if let Some(format) = &self.encoder
        {
            let source = value.type_name();
            let encoding_error = |message: String| InterpreterError::new(
                self.boundary.location.clone(),
                ErrorWhat::InvalidOperand(
                    format!(
                        "Pipe conversion format '{}', source {}, destination String: {}",
                        format,
                        source,
                        message
                    )
                )
            );
            value = interpreter.pipeline_codec(self.boundary, format, true, value)
                .map_err(|error|
                    {
                        let context = encoding_error(error.what.to_string());
                        error.with_context(context)
                    })?;
            let Value::String(text, Executable::No) = value.underlying() else
                { return Err(encoding_error("An encoder must return String".into())); };
            return Ok(Data::Bytes(text.as_bytes().to_vec()));
        }
        Ok(Data::Value(value))
    }

    fn row(&mut self, interpreter: &mut Interpreter, record: String) -> InterpreterResult<Step>
    {
        let format = self.boundary.decode.as_ref().unwrap();
        let mut value = interpreter.pipeline_codec(self.boundary, format, false,
            Value::from_string(record))
            .map_err(|cause| conversion_error(self.boundary, "String", cause))?;
        if let Some(target) = self.target
            && matches!(interpreter.scope().types.get(
                interpreter.scope().types.underlying_type(target)).kind, TypeKind::Struct(_))
        {
            let Value::Array(fields) = value.underlying() else
                { return Err(error(self.boundary, value.type_name(), "Expected a [String] row")); };
            if self.header.is_none()
            {
                let header = fields.iter().map(|field| match field
                    { Value::String(text, _) => Ok(text.clone()),
                      _ => Err(error(self.boundary, field.type_name(),
                          "Header must contain Strings")) })
                    .collect::<InterpreterResult<Vec<_>>>()?;
                let mut unique = HashSet::new();
                if header.iter().any(|field| !unique.insert(field))
                { return Err(error(self.boundary, "[String]", "Duplicate header field")); }
                self.header = Some(header);
                return Ok(Step::Pending);
            }
            let header = self.header.as_ref().unwrap();
            if header.len() != fields.len()
            { return Err(error(self.boundary, "[String]", "Row length differs from header")); }
            value = Value::from_hash_map(header.iter().zip(fields.iter())
                .map(|(key, value)| (MapKey::String(key.clone()), value.clone())).collect());
        }
        Ok(Step::Data(self.map(interpreter, value)?))
    }
}


impl Stream for BoundaryStream<'_>
{
    fn poll(
        &mut self,
        interpreter: &mut Interpreter,
        context: &mut Context,
    ) -> InterpreterResult<Step>
    {
        if let Some(rows) = self.rows.as_mut()
        {
            if let Some(record) = rows.next()
                .map_err(|message| error(self.boundary, "String", message))?
            { return self.row(interpreter, record); }
            if rows.ended() { return Ok(Step::End); }
        }
        else if self.ended { return Ok(Step::End); }
        match self.input.poll(interpreter, context)?
        {
            Step::Pending => Ok(Step::Pending),
            Step::Data(data) =>
                {
                    if self.boundary.decode.is_none()
                    {
                        return match data
                            { Data::Value(value) => Ok(Step::Data(self.map(interpreter, value)?)),
                              Data::Bytes(bytes) if self.encoder.is_none() =>
                                  Ok(Step::Data(Data::Bytes(bytes))),
                              Data::Bytes(_) => Err(error(self.boundary, "bytes",
                                  "Decode byte data before encoding another format")) };
                    }
                    let bytes = match data
                        { Data::Bytes(bytes) => bytes,
                          Data::Value(Value::String(text, Executable::No)) => text.into_bytes(),
                          Data::Value(value) => return Err(error(self.boundary, value.type_name(),
                              "A decoder requires String or byte data")) };
                    if let Some(rows) = self.rows.as_mut() { rows.push(&bytes); }
                    else
                    {
                        if self.bytes.len().saturating_add(bytes.len()) > 16 * 1024 * 1024
                        { return Err(error(self.boundary, "bytes",
                            "Decoder buffer exceeds 16 MiB")); }
                        self.bytes.extend(bytes);
                    }
                    Ok(Step::Pending)
                },
            Step::End =>
                {
                    if let Some(rows) = self.rows.as_mut()
                    { rows.finish(); return Ok(Step::Pending); }
                    self.ended = true;
                    if let Some(format) = &self.boundary.decode
                    {
                        let text = String::from_utf8(take(&mut self.bytes))
                            .map_err(|message| error(self.boundary, "bytes", message))?;
                        let value = interpreter.pipeline_codec(self.boundary, format, false,
                            Value::from_string(text))
                            .map_err(|cause| conversion_error(self.boundary, "String", cause))?;
                        Ok(Step::Data(self.map(interpreter, value)?))
                    }
                    else { Ok(Step::End) }
                },
        }
    }

    fn cancel(&mut self, interpreter: &mut Interpreter, context: &mut Context)
        -> InterpreterResult<()>
    { self.input.cancel(interpreter, context) }
    fn bytes_output(&self) -> bool
    { self.encoder.is_some() || (self.boundary.decode.is_none() && self.input.bytes_output()) }
}


impl Interpreter
{
    fn pipeline_factory(&self, boundary: &PipelineBoundary, owner: &str, name: &str)
        -> Option<Value>
    {
        if let Some(id) = self.scope().types.names.get(owner)
            && let Some(factory) = self.pipeline_type_factory(boundary, *id, name)
        { return Some(factory); }
        let qualified = format!("{}::{}", owner, name);
        self.module_function(&qualified).map(|function|
            Value::String(qualified.clone(), Executable::Function(function)))
            .or_else(|| self.module_native_function(&qualified)
                .map(|function| Value::String(qualified, Executable::Native(function))))
    }

    fn pipeline_type_factory(&self, boundary: &PipelineBoundary, target: TypeId, name: &str)
        -> Option<Value>
    {
        let key = method_key(target, name);
        let Value::HashMap(methods) = &boundary.conversions else { return None; };
        let value = methods.get(&MapKey::String(key.clone()))?;
        if matches!(value, Value::None)
        { self.scope().lexical_function(&key).map(|function|
            Value::String(name.into(), Executable::Function(function))) }
        else { Some(value.clone()) }
    }

    pub(super) fn pipeline_format_supported(
        &self,
        boundary: &PipelineBoundary,
        format: &str,
        encode: bool,
    ) -> bool
    {
        if format == "json"
        {
            return true;
        }
        let (owner, name) = if encode
            {
                ("String", format!("from_{}", conversion_type_name(format)))
            }
            else
            {
                (format, "from_string".into())
            };
        self.pipeline_factory(boundary, owner, &name).is_some()
    }

    pub(super) fn pipeline_codec(
        &mut self,
        boundary: &PipelineBoundary,
        format: &str,
        encode: bool,
        value: Value,
    ) -> InterpreterResult<Value>
    {
        if format == "json"
        {
            return self.pipeline_json_codec(
                &boundary.location,
                encode,
                boundary.type_id.is_some(),
                value,
                &boundary.conversions,
            );
        }
        let (owner, name) = if encode
            {
                ("String", format!("from_{}", conversion_type_name(format)))
            }
            else
            {
                (format, "from_string".into())
            };
        let factory = self.pipeline_factory(boundary, owner, &name).ok_or_else(||
            error(boundary, value.type_name(), format!("Missing {}::{}", owner, name)))?;
        self.last_result = None;
        self.execute_value(&boundary.location, factory, vec![value])?;
        Ok(self.last_result.take().unwrap_or(Value::None))
    }

    pub(super) fn pipeline_target(
        &mut self,
        boundary: &PipelineBoundary,
        target: TypeId,
        value: Value,
    ) -> InterpreterResult<Value>
    {
        if boundary.decode.as_deref() == Some("json")
        {
            return self.pipeline_json_target(
                &boundary.location,
                target,
                &value,
                &boundary.conversions,
            );
        }
        let types = self.scope().types.clone();
        let mut failure = None;
        map_value(&types, target, value, &mut |target, value|
            {
                if let Value::Struct(candidate) = value
                    && candidate.constructing
                    && let Some(signature) = types.constructors.get(&target)
                {
                    let factory = self.pipeline_type_factory(boundary, target, "new")
                        .or_else(|| self.module_method(target, &method_key(target, "new"))
                            .map(|function| Value::String(
                                "new".into(), Executable::Function(function))))
                        .ok_or_else(|| "Constructor factory is unavailable".to_string())?;
                    if !types.constructor_permissions.contains(&target)
                    { return Err("Constructor is private to its module".into()); }
                    let arguments = signature.arguments(candidate.fields.clone())?;
                    self.last_result = None;
                    self.execute_value(&boundary.location, factory, arguments)
                        .map_err(|error|
                            {
                                let message = error.what.to_string();
                                failure = Some(error);
                                message
                            })?;
                    let value = self.last_result.take().unwrap_or(Value::None);
                    return types.coerce(signature.result, value).map(Some);
                }
                let source = types.inferred_value_type(value);
                let available = self.has_captured_conversion(&boundary.location, target, source,
                    &boundary.conversions).map_err(|error|
                        {
                            let message = error.what.to_string();
                            failure = Some(error);
                            message
                        })?;
                if !available { return Ok(None); }
                self.last_result = None;
                self.convert_value(&boundary.location, target, value.clone(),
                    &boundary.conversions, &Value::Integer(source.0 as i64))
                    .map_err(|error|
                        {
                            let message = error.what.to_string();
                            failure = Some(error);
                            message
                        })?;
                let converted = self.last_result.take().unwrap_or(Value::None);
                types.coerce(target, converted).map(Some)
            }).map_err(|message|
                {
                    let context = error(boundary, "decoded value", message);
                    if let Some(cause) = failure { cause.with_context(context) } else { context }
                })
    }


    pub(super) fn pipeline_bytes(
        &mut self,
        stage: &PipelineStage,
        value: Value,
    ) -> InterpreterResult<Vec<u8>>
    {
        if let Value::Native(native) = value.underlying()
            && let Some(bytes) = native.downcast::<crate::language::data::io::Bytes>()
        {
            return Ok(bytes.0.to_vec());
        }
        if let Value::String(text, Executable::No) = &value
        { return Ok(text.as_bytes().to_vec()); }
        let source = self.scope().types.inferred_value_type(&value);
        let boundary = PipelineBoundary
            {
                location: stage.location.clone(), decode: None, annotation: None, type_id: None,
                conversions: stage.conversions.clone(),
            };
        let string = self.scope().types.builtin_id("String").unwrap();
        if self.has_captured_conversion(&stage.location, string, source, &stage.conversions)?
        {
            self.convert_value(&stage.location, string, value, &stage.conversions,
                &Value::Integer(source.0 as i64))?;
            if let Some(Value::String(text, Executable::No)) = self.last_result.take()
            { return Ok(text.into_bytes()); }
        }
        else if let Value::String(text, Executable::No) = value.underlying()
        { return Ok(text.as_bytes().to_vec()); }
        else if matches!(value.underlying(), Value::Integer(_) | Value::Float(_, _)
            | Value::Boolean(_) | Value::None)
        {
            let string = self.scope().types.builtin_id("String").unwrap();
            let converted = self.scope().types.convert(string, &value)
                .map_err(|message| error(&boundary, value.type_name(), message))?;
            return Ok(converted.as_text().into_bytes());
        }
        Err(InterpreterError::new(
            stage.location.clone(),
            ErrorWhat::InvalidOperand(
                "External input requires String or a String conversion; specify an encoding format"
                .into()
            )
        ))
    }
}

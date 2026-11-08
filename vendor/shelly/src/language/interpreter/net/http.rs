
use std::{
    any::Any,
    rc::Rc,
    time::{Duration, Instant},
};

use crate::{
    language::{
        bytecode::FunctionRef,
        data::{
            conversion::conversion_type_name,
            failure::{FailureStatus, occurrence},
            io::{Bytes, adapters::ByteSource, handles::Reader},
            map_key::MapKey,
            methods::method_key,
            native_value::{NativeData, NativeValue},
            net::http::{
                Client as TransportClient, Diagnostic, Request, RequestBody,
                Response as TransportResponse,
            },
            net::upload::Upload as TransportUpload,
            types::{NamedValue, StructValue, TypeDefinition, TypeId},
            value::{Executable, Value},
        },
        interpreter::{ErrorWhat, Interpreter, InterpreterError, InterpreterResult},
        native::{NativeOutcome, NativeWait},
        text::location::Location,
    },
    runtime::tasks::{NativeFault, NativeReply},
};

#[derive(Clone)]
struct Decoder
{
    target: TypeId,
    function: FunctionRef,
    name: String,
}
#[derive(Clone)]
struct Client
{
    transport: TransportClient,
    decoder: Option<Decoder>,
}
#[derive(Clone)]
struct Response
{
    transport: TransportResponse,
    decoder: Option<Decoder>,
}
#[derive(Clone)]
struct Upload
{
    transport: TransportUpload,
    decoder: Option<Decoder>,
    response: Rc<TypeDefinition>,
}
impl NativeData for Upload
{
    fn type_name(&self) -> &'static str
    {
        "__http_Upload"
    }
    fn display(&self) -> String
    {
        "<HTTP upload>".into()
    }
    fn as_any(&self) -> &dyn Any
    {
        self
    }
    fn operation(&self) -> bool
    {
        self.transport.active()
    }
    fn release(&self)
    {
        self.transport.close();
    }
}

impl NativeData for Client
{
    fn type_name(&self) -> &'static str
    {
        "__http_Client"
    }
    fn display(&self) -> String
    {
        "<HTTP client>".into()
    }
    fn as_any(&self) -> &dyn Any
    {
        self
    }
}
impl NativeData for Response
{
    fn retained_bytes(&self) -> Option<usize>
    {
        Some(
            65536
                + self
                    .transport
                    .0
                    .headers
                    .iter()
                    .map(|(name, values)| {
                        name.len() + values.iter().map(String::len).sum::<usize>()
                    })
                    .sum::<usize>(),
        )
    }
    fn operation(&self) -> bool
    {
        !self
            .transport
            .0
            .control
            .closed
            .load(std::sync::atomic::Ordering::Acquire)
    }
    fn release(&self)
    {
        self.transport.close();
    }
    fn type_name(&self) -> &'static str
    {
        "__http_Response"
    }
    fn display(&self) -> String
    {
        format!("<HTTP {}>", self.transport.0.status)
    }
    fn as_any(&self) -> &dyn Any
    {
        self
    }
}

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
fn data(value: impl NativeData) -> Value
{
    Value::Native(NativeValue::new(value))
}
pub(super) fn string(value: &Value) -> Result<&str, NativeFault>
{
    if let Value::String(value, Executable::No) = value.underlying()
    {
        Ok(value)
    }
    else
    {
        Err(NativeFault::new("OpenError", "Expected String"))
    }
}
pub(super) fn field<'a>(value: &'a Value, name: &str) -> Option<&'a Value>
{
    match value.underlying()
    {
        Value::HashMap(items) => items.get(&MapKey::String(name.into())),
        Value::Struct(item) =>
        {
            if let crate::language::data::types::TypeKind::Struct(fields) = &item.definition.kind
            {
                fields
                    .iter()
                    .position(|field| field.name == name)
                    .map(|index| &item.fields[index])
            }
            else
            {
                None
            }
        }
        _ => None,
    }
    .filter(|value| !matches!(value, Value::None))
}
fn duration(value: Option<&Value>, default: u64) -> Result<Duration, NativeFault>
{
    let value = match value.map(Value::underlying)
    {
        Some(Value::Integer(value)) if *value > 0 && *value <= 86_400_000 => *value as u64,
        None | Some(Value::None) => default,
        _ =>
        {
            return Err(NativeFault::new(
                "OpenError",
                "timeout_ms must be 1..86400000",
            ));
        }
    };
    Ok(Duration::from_millis(value))
}
fn timeout_policy(value: Option<&Value>, default: u64) -> Result<Option<Duration>, NativeFault>
{
    if let Some(Value::Enum(item)) = value.map(Value::underlying)
        && let crate::language::data::types::TypeKind::Enum(variants) = &item.definition.kind
        && variants[item.variant].name == "Unbounded"
    {
        return Ok(None);
    }
    duration(value, default).map(Some)
}

pub(super) fn headers(value: Option<&Value>) -> Result<Vec<(String, Vec<String>)>, NativeFault>
{
    let Some(value) = value
    else
    {
        return Ok(Vec::new());
    };
    let Value::HashMap(items) = value.underlying()
    else
    {
        return Err(NativeFault::new("OpenError", "Headers require a hash"));
    };
    items
        .iter()
        .map(|(key, value)| {
            let key = key.to_value();
            let key = string(&key)?.to_string();
            let values = if let Value::Array(values) = value.underlying()
            {
                values
                    .iter()
                    .map(|value| string(value).map(str::to_string))
                    .collect::<Result<_, _>>()?
            }
            else
            {
                vec![string(value)?.into()]
            };
            Ok((key, values))
        })
        .collect()
}
pub(super) fn body_content_type(
    headers: &mut Vec<(String, Vec<String>)>,
    content_type: &str,
) -> Result<(), NativeFault>
{
    let existing: Vec<_> = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .flat_map(|(_, values)| values)
        .collect();
    if existing.is_empty()
    {
        headers.push(("content-type".into(), vec![content_type.into()]));
    }
    else if existing.len() != 1 || !existing[0].eq_ignore_ascii_case(content_type)
    {
        return Err(NativeFault::new(
            "OpenError",
            "Body content type conflicts with request headers",
        ));
    }
    Ok(())
}

pub(super) fn header_values(headers: &[(String, Vec<String>)]) -> Value
{
    Value::from_hash_map(
        headers
            .iter()
            .map(|(name, values)| {
                (
                    MapKey::String(name.clone()),
                    Value::from_array(values.iter().cloned().map(Value::from_string).collect()),
                )
            })
            .collect(),
    )
}

impl Interpreter
{
    fn http_protocol(&mut self, location: &Location, message: String) -> Value
    {
        let id = self.scope().types.native_types["__http_ProtocolError"].id;
        let mut value = Value::Struct(Rc::new(StructValue {
            definition: self.scope().types.get(id),
            fields: vec![Value::from_string(message)],
            constructing: false,
            failure: None,
        }));
        let _ = self.activate_failure(location, &mut value, false);
        value
    }

    fn http_decoder(&self, value: Option<&Value>) -> Result<Option<Decoder>, NativeFault>
    {
        let Some(value) = value
        else
        {
            return Ok(None);
        };
        let Value::Type(target) = value
        else
        {
            return Err(NativeFault::new(
                "OpenError",
                "error_decoder must be a marked target Type",
            ));
        };
        let caller = self
            .exceptions
            .callers
            .iter()
            .rev()
            .find(|scope| **scope != self.current_scope)
            .unwrap_or(&self.current_scope);
        let scope = &self.scopes[caller];
        if !scope.types.has_error_type(target.id) || scope.types.success_type(target.id).is_some()
        {
            return Err(NativeFault::new(
                "OpenError",
                "error_decoder must name only marked errors",
            ));
        }
        let method = format!("from_{}", conversion_type_name("StatusError"));
        let key = method_key(target.id, &method);
        let function = scope
            .lexical_function(&key)
            .or_else(|| scope.imported_methods.get(&key).cloned())
            .ok_or_else(|| {
                NativeFault::new(
                    "OpenError",
                    format!("error_decoder has no accessible {}", method),
                )
            })?;
        Ok(Some(Decoder {
            target: target.id,
            function,
            name: format!("{}::{}", target.name, method),
        }))
    }

    fn http_status(
        &mut self,
        location: &Location,
        diagnostic: Diagnostic,
        decoder: Option<Decoder>,
    ) -> InterpreterResult<Value>
    {
        let id = self.scope().types.native_types["__http_StatusError"].id;
        let diagnostic_alias = self.scope().types.names.get("DiagnosticResponse").copied();
        let diagnostic_value = data(diagnostic.clone());
        let diagnostic_value = if let Some(id) = diagnostic_alias
        {
            self.scope()
                .types
                .coerce(id, diagnostic_value)
                .map_err(|message| {
                    InterpreterError::new(location.clone(), ErrorWhat::InvalidOperand(message))
                })?
        }
        else
        {
            diagnostic_value
        };
        let mut status = Value::Struct(Rc::new(StructValue {
            definition: self.scope().types.get(id),
            fields: vec![
                Value::from_string(format!("HTTP status {}", diagnostic.status)),
                diagnostic_value,
            ],
            constructing: false,
            failure: None,
        }));
        self.activate_failure(location, &mut status, false)?;
        if let Some(failure) = occurrence(&status)
        {
            let mut failure = failure.borrow_mut();
            let boundary = Location::new(file!(), line!() as usize, column!() as usize);
            failure.native_origin = Some(boundary.clone());
            failure.frames.insert(
                0,
                crate::language::interpreter::CallFrame {
                    name: "__http".into(),
                    location: location.clone(),
                    native: true,
                    native_location: Some(boundary),
                },
            );
        }
        let Some(decoder) = decoder
        else
        {
            return Ok(status);
        };
        let previous = self.last_result.take();
        let parent = self.exceptions.cleanup.take();
        let budget = parent
            .as_ref()
            .map_or(5_000_000, |guard| guard.instructions.min(5_000_000));
        let deadline = diagnostic
            .deadline
            .map_or(Instant::now() + Duration::from_secs(5), |deadline| {
                deadline.min(Instant::now() + Duration::from_secs(5))
            });
        self.exceptions.cleanup = Some(super::super::exceptions::CleanupGuard {
            hook: true,
            deadline: parent
                .as_ref()
                .map_or(deadline, |guard| guard.deadline.min(deadline)),
            instructions: budget,
            function_depth: self.exceptions.frames.len(),
            interrupted: false,
        });
        let outcome = self.execute_function(
            location,
            &decoder.name,
            &decoder.function,
            &[status.clone()],
        );
        let consumed = budget.saturating_sub(
            self.exceptions
                .cleanup
                .take()
                .map_or(0, |guard| guard.instructions),
        );
        self.exceptions.cleanup = parent.map(|mut guard| {
            guard.instructions = guard.instructions.saturating_sub(consumed);
            guard
        });
        let replacement = self.last_result.take().unwrap_or(Value::None);
        self.last_result = previous;
        if outcome.is_ok()
            && crate::language::data::failure::marked(&replacement)
            && self.scope().types.catch_matches(
                decoder.target,
                self.scope().types.value_type(replacement.underlying()),
            )
        {
            return Ok(replacement);
        }
        let failure = match outcome
        {
            Err(error) => error,
            Ok(()) if crate::language::data::failure::marked(&replacement) =>
            {
                self.raise_failure(location, replacement)?
            }
            Ok(()) => InterpreterError::new(
                location.clone(),
                ErrorWhat::InvalidOperand("HTTP error decoder returned a successful value".into()),
            ),
        };
        let related = self.related_failure(&failure, "Decoding");
        if let Some(occurrence) = occurrence(&status)
        {
            let mut occurrence = occurrence.borrow_mut();
            occurrence.status = FailureStatus::Pending;
            occurrence.related.push(related);
            self.exceptions
                .pending
                .insert(occurrence.id, status.clone());
        }
        Ok(status)
    }

    pub(in crate::language::interpreter) fn http_upload_sink(
        value: &Value,
    ) -> Option<crate::language::data::io::adapters::ByteSink>
    {
        native::<Upload>(value).map(|upload| upload.transport.sink())
    }
    pub(in crate::language::interpreter) fn http_upload_ready(value: &Value) -> bool
    {
        native::<Upload>(value).is_some_and(|upload| upload.transport.ready())
    }
    pub(in crate::language::interpreter) fn handle_http_upload(
        &mut self,
        _: &Location,
        args: &[Value],
    ) -> NativeOutcome
    {
        let Some(upload) = args.first().and_then(native::<Upload>).cloned()
        else
        {
            return NativeOutcome::Ready(self.io_fault(
                "WriteError",
                "http.upload",
                "Expected Upload".into(),
            ));
        };
        let decoder = upload.decoder;
        let definition = upload.response;
        self.native_wait(
            "http.upload.finish",
            async move { upload.transport.finish().await },
            move |transport| {
                Value::Named(Rc::new(NamedValue {
                    definition,
                    value: data(Response {
                        transport,
                        decoder,
                    }),
                }))
            },
        )
    }

    pub(in crate::language::interpreter) fn handle_http_apply(
        &mut self,
        location: &Location,
        args: &[Value],
    ) -> InterpreterResult<()>
    {
        let [document, conversion, options, response] = args
        else
        {
            return Err(InterpreterError::new(
                location.clone(),
                ErrorWhat::ArgumentMismatch(
                    "HTTP conversion requires a response and bound target".into(),
                ),
            ));
        };
        let deadline =
            native::<Response>(response).and_then(|response| response.transport.0.deadline);
        let previous = self.exceptions.operation.take();
        self.exceptions.operation = deadline
            .map(|deadline| {
                (
                    previous
                        .as_ref()
                        .map_or_else(Vec::new, |(flags, _)| flags.clone()),
                    previous
                        .as_ref()
                        .map_or(deadline, |(_, parent)| deadline.min(*parent)),
                )
            })
            .or_else(|| previous.clone());
        let result = self.handle_json_apply(
            location,
            &[document.clone(), conversion.clone(), options.clone()],
        );
        self.exceptions.operation = previous;
        if deadline.is_some_and(|deadline| Instant::now() >= deadline)
            && result
                .as_ref()
                .err()
                .is_none_or(|error| matches!(error.what, ErrorWhat::OperationCancelled))
        {
            let secondary = if let Err(error) = result
            {
                Some(error)
            }
            else
            {
                self.last_result
                    .take()
                    .filter(|value| crate::language::data::failure::pending(value).is_some())
                    .map(|value| self.raise_failure(location, value))
                    .transpose()?
            };
            let mut error = self.io_fault(
                "TimeoutError",
                "http.decode",
                "HTTP operation deadline expired during conversion".into(),
            );
            self.activate_failure(location, &mut error, false)?;
            if let Some(failure) = occurrence(&error)
            {
                failure.borrow_mut().native_origin =
                    Some(Location::new(file!(), line!() as usize, column!() as usize));
            }
            let error = self.raise_failure(location, error)?;
            return self.combine_failure(Err(error), secondary.map_or(Ok(()), Err), "Cause");
        }
        result
    }

    pub(in crate::language::interpreter) fn handle_http(
        &mut self,
        location: &Location,
        args: &[Value],
    ) -> NativeOutcome
    {
        let result = self.http_command(location, args);
        result.unwrap_or_else(|error| {
            NativeOutcome::Ready(self.io_fault(&error.kind, "http", error.message))
        })
    }

    fn http_command(
        &mut self,
        location: &Location,
        args: &[Value],
    ) -> Result<NativeOutcome, NativeFault>
    {
        let [command, input, rest @ ..] = args
        else
        {
            return Err(NativeFault::new("OpenError", "Missing HTTP operation"));
        };
        let command = string(command)?;
        match command
        {
            "path" | "form" =>
            {
                let Value::Array(values) = input.underlying()
                else
                {
                    return Err(NativeFault::new(
                        "OpenError",
                        "URL helper requires an array",
                    ));
                };
                if values.len() > 1024
                {
                    return Err(NativeFault::new("LimitError", "Too many URL components"));
                }
                let mut url = reqwest::Url::parse("https://shelly.invalid/").unwrap();
                if command == "path"
                {
                    let mut segments = url.path_segments_mut().unwrap();
                    segments.pop_if_empty();
                    for value in values.iter()
                    {
                        let value = string(value)?;
                        if matches!(value, "." | "..")
                        {
                            return Err(NativeFault::new(
                                "OpenError",
                                "Dot path segments are ambiguous",
                            ));
                        }
                        segments.push(value);
                    }
                }
                else
                {
                    let mut query = url.query_pairs_mut();
                    for value in values.iter()
                    {
                        let Value::Array(pair) = value.underlying()
                        else
                        {
                            return Err(NativeFault::new(
                                "OpenError",
                                "Form requires string pairs",
                            ));
                        };
                        let [key, value] = pair.as_slice()
                        else
                        {
                            return Err(NativeFault::new(
                                "OpenError",
                                "Form pair must have two strings",
                            ));
                        };
                        query.append_pair(string(key)?, string(value)?);
                    }
                }
                let encoded = if command == "path"
                {
                    url.path().trim_start_matches('/')
                }
                else
                {
                    url.query().unwrap_or("")
                };
                if encoded.len() > 1048576
                {
                    return Err(NativeFault::new(
                        "LimitError",
                        "URL helper exceeds byte limit",
                    ));
                }
                Ok(NativeOutcome::Ready(Value::from_string(encoded.into())))
            }
            "multipart" =>
            {
                let Value::Array(parts) = input.underlying()
                else
                {
                    return Err(NativeFault::new(
                        "OpenError",
                        "Multipart requires Part values",
                    ));
                };
                if parts.len() > 128
                {
                    return Err(NativeFault::new(
                        "LimitError",
                        "Multipart exceeds 128 parts",
                    ));
                }
                let mut prepared = Vec::new();
                let mut total = 0usize;
                for part in parts.iter()
                {
                    let name = string(
                        field(part, "name")
                            .ok_or_else(|| NativeFault::new("OpenError", "Part requires name"))?,
                    )?;
                    let bytes = field(part, "data")
                        .and_then(native::<Bytes>)
                        .ok_or_else(|| NativeFault::new("OpenError", "Part requires Bytes"))?;
                    let filename = field(part, "filename").map(string).transpose()?;
                    let content_type = field(part, "content_type").map(string).transpose()?;
                    if [Some(name), filename, content_type]
                        .iter()
                        .flatten()
                        .any(|text| {
                            text.len() > 4096 || text.bytes().any(|byte| byte < 32 || byte == 127)
                        })
                    {
                        return Err(NativeFault::new(
                            "OpenError",
                            "Invalid multipart header field",
                        ));
                    }
                    total = total.saturating_add(bytes.0.len() + 16384);
                    if total > 16777216
                    {
                        return Err(NativeFault::new("LimitError", "Multipart exceeds 16 MiB"));
                    }
                    prepared.push((name, bytes, filename, content_type));
                }
                static NEXT_BOUNDARY: std::sync::atomic::AtomicU64 =
                    std::sync::atomic::AtomicU64::new(1);
                let mut selected = None;
                for _ in 0..8
                {
                    let next = NEXT_BOUNDARY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let boundary = format!("shelly-{}-{}", std::process::id(), next);
                    if !prepared.iter().any(|(_, bytes, _, _)| {
                        bytes
                            .0
                            .windows(boundary.len())
                            .any(|window| window == boundary.as_bytes())
                    })
                    {
                        selected = Some(boundary);
                        break;
                    }
                }
                let boundary = selected.ok_or_else(|| {
                    NativeFault::new("LimitError", "Multipart boundary collision limit exceeded")
                })?;
                let quote = |text: &str| text.replace('\\', "\\\\").replace('"', "\\\"");
                let mut body = Vec::with_capacity(total);
                for (name, bytes, filename, content_type) in prepared
                {
                    let mut header = format!(
                        "--{}\r\nContent-Disposition: form-data; name=\"{}\"",
                        boundary,
                        quote(name)
                    );
                    if let Some(filename) = filename
                    {
                        header.push_str(&format!("; filename=\"{}\"", quote(filename)));
                    }
                    header.push_str("\r\n");
                    if let Some(content_type) = content_type
                    {
                        header.push_str(&format!("Content-Type: {}\r\n", content_type));
                    }
                    header.push_str("\r\n");
                    body.extend_from_slice(header.as_bytes());
                    body.extend_from_slice(&bytes.0);
                    body.extend_from_slice(b"\r\n");
                }
                body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());
                Ok(NativeOutcome::Ready(Value::from_array(vec![
                    data(Bytes(body.into())),
                    Value::from_string(format!("multipart/form-data; boundary={}", boundary)),
                ])))
            }
            "client" =>
            {
                let defaults = headers(field(input, "headers"))?;
                let mut defaults = defaults;
                if let Some(auth) = field(input, "auth")
                {
                    defaults.push(("authorization".into(), vec![string(auth)?.into()]));
                }
                let decoder = self.http_decoder(field(input, "error_decoder"))?;
                let base = field(input, "base_url")
                    .ok_or_else(|| NativeFault::new("OpenError", "Missing base_url"))?;
                let transport = TransportClient::new(
                    string(base)?,
                    defaults,
                    timeout_policy(field(input, "timeout_ms"), 30000)?,
                    duration(field(input, "connect_timeout_ms"), 10000)?,
                    timeout_policy(field(input, "idle_timeout_ms"), 30000)?,
                )?;
                Ok(NativeOutcome::Ready(data(Client {
                    transport,
                    decoder,
                })))
            }
            "send" | "upload" =>
            {
                let client = native::<Client>(input)
                    .ok_or_else(|| NativeFault::new("OpenError", "Expected HTTP client"))?;
                let request = rest
                    .first()
                    .ok_or_else(|| NativeFault::new("OpenError", "Missing request"))?;
                let method = string(
                    field(request, "method")
                        .ok_or_else(|| NativeFault::new("OpenError", "Missing method"))?,
                )?
                .into();
                let path = string(
                    field(request, "path")
                        .ok_or_else(|| NativeFault::new("OpenError", "Missing path"))?,
                )?
                .into();
                let mut headers = headers(field(request, "headers"))?;
                let body = match field(request, "body")
                {
                    Some(body) =>
                    {
                        let value = field(body, "value").unwrap_or(body);
                        if let Some(content_type) = field(body, "content_type")
                        {
                            body_content_type(&mut headers, string(content_type)?)?;
                        }
                        if let Some(bytes) = native::<Bytes>(value)
                        {
                            RequestBody::Bytes(bytes.clone())
                        }
                        else if let Some(reader) = native::<Reader>(value)
                        {
                            RequestBody::Reader(reader.clone())
                        }
                        else if let Some(source) = native::<ByteSource>(value)
                        {
                            RequestBody::Source(source.clone())
                        }
                        else
                        {
                            return Err(NativeFault::new(
                                "OpenError",
                                "Body must contain Bytes or Reader",
                            ));
                        }
                    }
                    None => RequestBody::Empty,
                };
                let query = if let Some(query) = field(request, "query")
                {
                    let Value::Array(pairs) = query.underlying()
                    else
                    {
                        return Err(NativeFault::new("OpenError", "Query requires pairs"));
                    };
                    pairs
                        .iter()
                        .map(|pair| {
                            let Value::Array(pair) = pair.underlying()
                            else
                            {
                                return Err(NativeFault::new(
                                    "OpenError",
                                    "Query pair must be array",
                                ));
                            };
                            let [key, value] = pair.as_slice()
                            else
                            {
                                return Err(NativeFault::new(
                                    "OpenError",
                                    "Query pair requires two strings",
                                ));
                            };
                            Ok((string(key)?.into(), string(value)?.into()))
                        })
                        .collect::<Result<_, NativeFault>>()?
                }
                else
                {
                    Vec::new()
                };
                let timeout = field(request, "timeout_ms")
                    .map(|value| timeout_policy(Some(value), 30000))
                    .transpose()?;
                let request = Request {
                    method,
                    path,
                    headers,
                    query,
                    body,
                    timeout,
                };
                let transport = client.transport.clone();
                let decoder = client.decoder.clone();
                if command == "upload"
                {
                    if !matches!(request.body, RequestBody::Empty)
                    {
                        return Err(NativeFault::new(
                            "OpenError",
                            "Upload request body comes from the pipeline",
                        ));
                    }
                    let response = self.scope().types.get(self.scope().types.names["Response"]);
                    Ok(self.native_wait(
                        "http.upload",
                        async move { TransportUpload::new(transport, request).await },
                        move |transport| {
                            data(Upload {
                                transport,
                                decoder,
                                response,
                            })
                        },
                    ))
                }
                else
                {
                    Ok(self.native_wait(
                        "http.send",
                        async move { transport.send(request).await },
                        move |transport| {
                            data(Response {
                                transport,
                                decoder,
                            })
                        },
                    ))
                }
            }
            "take_body" =>
            {
                let response = native::<Response>(input)
                    .ok_or_else(|| NativeFault::new("ReadError", "Expected HTTP response"))?;
                Ok(NativeOutcome::Ready(data(response.transport.take_body()?)))
            }
            "require_success" =>
            {
                let response = native::<Response>(input)
                    .cloned()
                    .ok_or_else(|| NativeFault::new("ReadError", "Expected HTTP response"))?;
                if (200..300).contains(&response.transport.0.status)
                {
                    return Ok(NativeOutcome::Ready(input.clone()));
                }
                let transport = response.transport.clone();
                let location = location.clone();
                Ok(NativeOutcome::Wait(NativeWait {
                    future: Box::pin(async move {
                        transport
                            .capture()
                            .await
                            .map(|value| Box::new(value) as Box<dyn Any + Send>)
                    }),
                    finish: Box::new(move |interpreter, reply: NativeReply| match reply
                    {
                        Ok(diagnostic) => interpreter.http_status(
                            &location,
                            *diagnostic
                                .downcast::<Diagnostic>()
                                .expect("Diagnostic reply"),
                            response.decoder,
                        ),
                        Err(error) =>
                        {
                            Ok(interpreter.io_fault(&error.kind, "http.capture", error.message))
                        }
                    }),
                }))
            }
            "status" | "headers" | "url" =>
            {
                let value = if let Some(response) = native::<Response>(input)
                {
                    match command
                    {
                        "status" => Value::Integer(response.transport.0.status as i64),
                        "headers" => header_values(&response.transport.0.headers),
                        _ => Value::from_string(response.transport.0.url.clone()),
                    }
                }
                else if let Some(response) = native::<Diagnostic>(input)
                {
                    match command
                    {
                        "status" => Value::Integer(response.status as i64),
                        "headers" => header_values(&response.headers),
                        _ => Value::from_string(response.url.clone()),
                    }
                }
                else
                {
                    return Err(NativeFault::new("ReadError", "Expected response"));
                };
                Ok(NativeOutcome::Ready(value))
            }
            "diagnostic_bytes"
            | "diagnostic_complete"
            | "diagnostic_reason"
            | "diagnostic_received" =>
            {
                let response = native::<Diagnostic>(input)
                    .ok_or_else(|| NativeFault::new("ReadError", "Expected DiagnosticResponse"))?;
                Ok(NativeOutcome::Ready(match command
                {
                    "diagnostic_bytes" => data(response.bytes.clone()),
                    "diagnostic_received" => Value::Integer(response.bytes_received as i64),
                    "diagnostic_complete" => Value::Boolean(response.complete),
                    _ => Value::from_string(response.reason.clone()),
                }))
            }
            _ => Ok(NativeOutcome::Ready(
                self.http_protocol(location, "Unknown HTTP operation".into()),
            )),
        }
    }

    pub(in crate::language::interpreter) fn handle_http_close(
        &mut self,
        location: &Location,
        args: &[Value],
    ) -> InterpreterResult<()>
    {
        let mut result = match args
        {
            [value] if native::<Upload>(value).is_some() =>
            {
                native::<Upload>(value).unwrap().transport.close();
                Value::None
            }
            [value] if native::<Client>(value).is_some() =>
            {
                native::<Client>(value).unwrap().transport.close();
                Value::None
            }
            [value] if native::<Response>(value).is_some() =>
            {
                native::<Response>(value).unwrap().transport.close();
                Value::None
            }
            _ => self.http_protocol(location, "Expected HTTP client or response".into()),
        };
        self.activate_failure(location, &mut result, false)?;
        self.last_result = Some(result);
        Ok(())
    }

    pub(in crate::language::interpreter) fn handle_http_header(
        &mut self,
        location: &Location,
        args: &[Value],
    ) -> InterpreterResult<()>
    {
        let [Value::HashMap(headers), Value::String(name, _), rest @ ..] = args
        else
        {
            self.last_result = Some(Value::None);
            return Ok(());
        };
        let mut values = Vec::new();
        for (key, value) in headers.iter()
        {
            if let MapKey::String(key) = key
                && key.eq_ignore_ascii_case(name)
                && let Value::Array(items) = value.underlying()
            {
                values.extend(items.iter().cloned());
            }
        }
        self.last_result = Some(
            if rest.first().is_some_and(Value::as_bool)
            {
                Value::from_array(values)
            }
            else if values.len() > 1
            {
                self.http_protocol(
                    location,
                    format!("Header '{}' has multiple values; use all", name),
                )
            }
            else
            {
                values.pop().unwrap_or(Value::None)
            },
        );
        Ok(())
    }
}

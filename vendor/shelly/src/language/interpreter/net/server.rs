
use crate::{
    language::{
        data::{
            io::{Bytes, adapters::ByteSource, handles::Reader},
            native_value::{NativeData, NativeValue},
            net::server::{Incoming, Outgoing, Reply, Server},
            value::Value,
        },
        interpreter::{ErrorWhat, Interpreter, InterpreterError, InterpreterResult},
        native::NativeOutcome,
        text::location::Location,
    },
    runtime::tasks::NativeFault,
};

use super::http::{body_content_type, field, header_values, headers, string};

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

impl Interpreter
{
    pub(in crate::language::interpreter) fn handle_server(
        &mut self,
        _: &Location,
        args: &[Value],
    ) -> NativeOutcome
    {
        let result = self.server_command(args);
        result.unwrap_or_else(|error| {
            NativeOutcome::Ready(self.io_fault(&error.kind, "http.server", error.message))
        })
    }
    fn server_command(&mut self, args: &[Value]) -> Result<NativeOutcome, NativeFault>
    {
        let [command, input, rest @ ..] = args
        else
        {
            return Err(NativeFault::new("OpenError", "Missing server operation"));
        };
        let command = string(command)?;
        match command
        {
            "listen" =>
            {
                let host = string(input)?.to_string();
                let Some(Value::Integer(port)) = rest.first()
                else
                {
                    return Err(NativeFault::new("OpenError", "Missing server port"));
                };
                let port = u16::try_from(*port)
                    .map_err(|_| NativeFault::new("OpenError", "Invalid port"))?;
                Ok(self.native_wait(command, async move { Server::bind(host, port).await }, data))
            }
            "address" =>
            {
                let server = native::<Server>(input)
                    .ok_or_else(|| NativeFault::new("OpenError", "Expected Server"))?;
                let address = server.address();
                Ok(NativeOutcome::Ready(Value::from_array(vec![
                    Value::from_string(address.ip().to_string()),
                    Value::Integer(address.port() as i64),
                ])))
            }
            "accept" =>
            {
                let server = native::<Server>(input)
                    .cloned()
                    .ok_or_else(|| NativeFault::new("OpenError", "Expected Server"))?;
                Ok(self.native_wait(command, async move { server.accept().await }, data))
            }
            "method" | "path" | "headers" | "take_body" =>
            {
                let request = native::<Incoming>(input)
                    .ok_or_else(|| NativeFault::new("OpenError", "Expected Incoming"))?;
                Ok(NativeOutcome::Ready(match command
                {
                    "method" => Value::from_string(request.method().into()),
                    "path" => Value::from_string(request.path().into()),
                    "headers" => header_values(request.headers()),
                    _ => data(request.take_body()?),
                }))
            }
            "respond" =>
            {
                let request = native::<Incoming>(input)
                    .cloned()
                    .ok_or_else(|| NativeFault::new("OpenError", "Expected Incoming"))?;
                let response = rest
                    .first()
                    .ok_or_else(|| NativeFault::new("OpenError", "Missing Response"))?;
                let status = field(response, "status")
                    .and_then(|value| {
                        if let Value::Integer(status) = value
                        {
                            Some(*status)
                        }
                        else
                        {
                            None
                        }
                    })
                    .unwrap_or(200);
                if !(200..=599).contains(&status)
                {
                    return Err(NativeFault::new(
                        "OpenError",
                        "Server status must be 200..599",
                    ));
                }
                let mut response_headers = headers(field(response, "headers"))?;
                let body = match field(response, "body")
                {
                    None => Outgoing::Empty,
                    Some(body) =>
                    {
                        if let Some(content_type) = field(body, "content_type")
                        {
                            body_content_type(&mut response_headers, string(content_type)?)?;
                        }
                        let value = field(body, "value").unwrap_or(body);
                        if let Some(bytes) = native::<Bytes>(value)
                        {
                            Outgoing::Bytes(bytes.clone())
                        }
                        else if let Some(reader) = native::<Reader>(value)
                        {
                            Outgoing::Reader(reader.clone())
                        }
                        else if let Some(source) = native::<ByteSource>(value)
                        {
                            Outgoing::Source(source.clone())
                        }
                        else
                        {
                            return Err(NativeFault::new("OpenError", "Unsupported server body"));
                        }
                    }
                };
                let response = Reply {
                    status: status as u16,
                    headers: response_headers,
                    body,
                };
                Ok(self.native_wait(
                    command,
                    async move { request.respond(response).await },
                    |_| Value::None,
                ))
            }
            _ => Err(NativeFault::new("OpenError", "Unknown server operation")),
        }
    }
    pub(in crate::language::interpreter) fn handle_server_close(
        &mut self,
        _: &Location,
        args: &[Value],
    ) -> NativeOutcome
    {
        if let Some(value) = args.first()
        {
            if let Some(server) = native::<Server>(value)
            {
                server.close();
            }
            if let Some(request) = native::<Incoming>(value).cloned()
            {
                return self.native_wait(
                    "http.request.close",
                    async move { request.finish_close().await },
                    |_| Value::None,
                );
            }
        }
        NativeOutcome::Ready(Value::None)
    }
    pub(in crate::language::interpreter) fn handle_server_invoke(
        &mut self,
        location: &Location,
        args: &[Value],
    ) -> InterpreterResult<()>
    {
        let [handler, request] = args
        else
        {
            return Err(InterpreterError::new(
                location.clone(),
                ErrorWhat::ArgumentMismatch("Handler requires request".into()),
            ));
        };
        let incoming = native::<Incoming>(request).ok_or_else(|| {
            InterpreterError::new(
                location.clone(),
                ErrorWhat::ArgumentMismatch("Expected Incoming".into()),
            )
        })?;
        let previous = self
            .exceptions
            .operation
            .replace((vec![incoming.cancelled()], incoming.deadline()));
        let result = self.execute_value(location, handler.clone(), vec![request.clone()]);
        self.exceptions.operation = previous;
        result
    }
}


use std::{net::SocketAddr, time::Duration};

use tokio::net::{TcpListener, TcpStream, lookup_host};

use crate::{
    language::{
        data::{
            native_value::{NativeData, NativeValue},
            net::tcp::{Connection, Listener},
            value::{Executable, Value},
        },
        interpreter::{Interpreter, InterpreterResult},
        native::NativeOutcome,
        text::location::Location,
    },
    runtime::tasks::NativeFault,
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
fn data(value: impl NativeData) -> Value
{
    Value::Native(NativeValue::new(value))
}
fn address(address: SocketAddr) -> Value
{
    Value::from_array(vec![
        Value::from_string(address.ip().to_string()),
        Value::Integer(address.port() as i64),
    ])
}

impl Interpreter
{
    pub(in crate::language::interpreter) fn handle_tcp(
        &mut self,
        _: &Location,
        args: &[Value],
    ) -> NativeOutcome
    {
        let [Value::String(command, _), input, rest @ ..] = args
        else
        {
            return NativeOutcome::Ready(self.io_fault(
                "OpenError",
                "tcp",
                "Missing TCP operation".into(),
            ));
        };
        match command.as_str()
        {
            "connect" | "listen" =>
            {
                let Value::String(host, Executable::No) = input.underlying()
                else
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "OpenError",
                        command,
                        "Host must be String".into(),
                    ));
                };
                let Some(Value::Integer(port)) = rest.first()
                else
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "OpenError",
                        command,
                        "Port must be Integer".into(),
                    ));
                };
                if !(0..=65535).contains(port) || command == "connect" && *port == 0
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "OpenError",
                        command,
                        "Port is out of range".into(),
                    ));
                }
                let endpoint = (host.clone(), *port as u16);
                if command == "connect"
                {
                    self.native_wait(
                        "tcp.connect",
                        async move {
                            let stream = tokio::time::timeout(
                                Duration::from_secs(30),
                                TcpStream::connect(endpoint),
                            )
                            .await
                            .map_err(|_| NativeFault::new("TimeoutError", "TCP connect timed out"))?
                            .map_err(|error| NativeFault::io("OpenError", error))?;
                            Connection::new(stream)
                        },
                        data,
                    )
                }
                else
                {
                    self.native_wait(
                        "tcp.listen",
                        async move {
                            Listener::new(
                                TcpListener::bind(endpoint)
                                    .await
                                    .map_err(|error| NativeFault::io("OpenError", error))?,
                            )
                        },
                        data,
                    )
                }
            }
            "accept" =>
            {
                let Some(listener) = native::<Listener>(input).cloned()
                else
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "OpenError",
                        command,
                        "Expected Listener".into(),
                    ));
                };
                self.native_wait("tcp.accept", async move { listener.accept().await }, data)
            }
            "address" => NativeOutcome::Ready(
                native::<Listener>(input)
                    .map(|listener| address(listener.0.address))
                    .unwrap_or_else(|| {
                        self.io_fault("OpenError", command, "Expected Listener".into())
                    }),
            ),
            "reader" | "writer" | "peer" | "local" =>
            {
                let Some(connection) = native::<Connection>(input)
                else
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "OpenError",
                        command,
                        "Expected Connection".into(),
                    ));
                };
                NativeOutcome::Ready(match command.as_str()
                {
                    "reader" => data(connection.reader.clone()),
                    "writer" => data(connection.writer.clone()),
                    "peer" => address(connection.peer),
                    _ => address(connection.local),
                })
            }
            "resolve" =>
            {
                let Value::String(host, Executable::No) = input.underlying()
                else
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "OpenError",
                        command,
                        "Expected host String".into(),
                    ));
                };
                let host = host.clone();
                self.native_wait(
                    "dns.resolve",
                    async move {
                        let mut addresses: Vec<_> =
                            tokio::time::timeout(Duration::from_secs(30), lookup_host((host, 0)))
                                .await
                                .map_err(|_| {
                                    NativeFault::new("TimeoutError", "DNS lookup timed out")
                                })?
                                .map_err(|error| NativeFault::io("OpenError", error))?
                                .map(|address| address.ip().to_string())
                                .collect();
                        addresses.sort();
                        addresses.dedup();
                        Ok(addresses)
                    },
                    |addresses| {
                        Value::from_array(addresses.into_iter().map(Value::from_string).collect())
                    },
                )
            }
            _ => NativeOutcome::Ready(self.io_fault(
                "OpenError",
                command,
                "Unknown TCP operation".into(),
            )),
        }
    }

    pub(in crate::language::interpreter) fn handle_tcp_close(
        &mut self,
        location: &Location,
        args: &[Value],
    ) -> InterpreterResult<()>
    {
        let mut value = match args
        {
            [value] if native::<Connection>(value).is_some() =>
            {
                native::<Connection>(value).unwrap().close();
                Value::None
            }
            [value] if native::<Listener>(value).is_some() =>
            {
                native::<Listener>(value).unwrap().close();
                Value::None
            }
            _ => self.io_fault(
                "ClosedError",
                "tcp.close",
                "Expected Connection or Listener".into(),
            ),
        };
        self.activate_failure(location, &mut value, false)?;
        self.last_result = Some(value);
        Ok(())
    }
}

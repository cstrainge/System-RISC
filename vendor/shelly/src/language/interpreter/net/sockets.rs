
use std::{net::SocketAddr, time::Duration};

use crate::{
    language::{
        data::{
            io::Bytes,
            native_value::{NativeData, NativeValue},
            net::{
                sockets::{Connection, DatagramSocket, tls_connect, tls_wrap},
                tcp::Connection as TcpConnection,
            },
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
fn text(value: &Value) -> Option<&str>
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
fn port(value: Option<&Value>) -> Option<u16>
{
    if let Some(Value::Integer(value)) = value
    {
        u16::try_from(*value).ok()
    }
    else
    {
        None
    }
}

impl Interpreter
{
    pub(in crate::language::interpreter) fn handle_socket(
        &mut self,
        _: &Location,
        args: &[Value],
    ) -> NativeOutcome
    {
        let [command, input, rest @ ..] = args
        else
        {
            return NativeOutcome::Ready(self.io_fault(
                "OpenError",
                "socket",
                "Missing operation".into(),
            ));
        };
        let command = text(command).unwrap_or("");
        match command
        {
            "udp_bind" =>
            {
                let (Some(host), Some(port)) = (text(input), port(rest.first()))
                else
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "OpenError",
                        command,
                        "Expected host and port".into(),
                    ));
                };
                let host = host.to_string();
                self.native_wait(
                    command,
                    async move { DatagramSocket::bind(host, port).await },
                    data,
                )
            }
            "udp_address" => NativeOutcome::Ready(
                native::<DatagramSocket>(input)
                    .ok_or_else(|| NativeFault::new("OpenError", "Expected UDP socket"))
                    .and_then(DatagramSocket::address)
                    .map(address)
                    .unwrap_or_else(|error| self.io_fault(&error.kind, command, error.message)),
            ),
            "udp_receive" =>
            {
                let Some(socket) = native::<DatagramSocket>(input).cloned()
                else
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "ReadError",
                        command,
                        "Expected UDP socket".into(),
                    ));
                };
                let maximum = if let Some(Value::Integer(maximum)) = rest.first()
                {
                    *maximum
                }
                else
                {
                    65535
                };
                if !(1..=65535).contains(&maximum)
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "LimitError",
                        command,
                        "Datagram limit must be 1..65535".into(),
                    ));
                }
                self.native_wait(
                    command,
                    async move {
                        tokio::time::timeout(
                            Duration::from_secs(30),
                            socket.receive(maximum as usize),
                        )
                        .await
                        .map_err(|_| {
                            NativeFault::new("TimeoutError", "Datagram receive timed out")
                        })?
                    },
                    |(bytes, peer, truncated)| {
                        Value::from_array(vec![
                            data(bytes),
                            address(peer),
                            Value::Boolean(truncated),
                        ])
                    },
                )
            }
            "udp_send" =>
            {
                let socket = native::<DatagramSocket>(input).cloned();
                let bytes = rest.first().and_then(native::<Bytes>).cloned();
                let host = rest.get(1).and_then(text).map(str::to_string);
                let port = port(rest.get(2));
                let (Some(socket), Some(bytes), Some(host), Some(port)) =
                    (socket, bytes, host, port)
                else
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "WriteError",
                        command,
                        "Expected socket, Bytes, host and port".into(),
                    ));
                };
                self.native_wait(
                    command,
                    async move {
                        tokio::time::timeout(
                            Duration::from_secs(30),
                            socket.send(bytes, host, port),
                        )
                        .await
                        .map_err(|_| NativeFault::new("TimeoutError", "Datagram send timed out"))?
                    },
                    |count| Value::Integer(count as i64),
                )
            }
            "reader" | "writer" => NativeOutcome::Ready(
                native::<Connection>(input)
                    .map(|connection| {
                        if command == "reader"
                        {
                            data(connection.reader.clone())
                        }
                        else
                        {
                            data(connection.writer.clone())
                        }
                    })
                    .unwrap_or_else(|| {
                        self.io_fault("OpenError", command, "Expected stream connection".into())
                    }),
            ),
            "tls_connect" | "tls_wrap" =>
            {
                let certificate = rest.get(1).and_then(native::<Bytes>).cloned();
                if command == "tls_connect"
                {
                    let (Some(host), Some(port)) = (text(input), port(rest.first()))
                    else
                    {
                        return NativeOutcome::Ready(self.io_fault(
                            "OpenError",
                            command,
                            "Expected TLS host and port".into(),
                        ));
                    };
                    let host = host.to_string();
                    self.native_wait(
                        command,
                        async move {
                            tokio::time::timeout(
                                Duration::from_secs(30),
                                tls_connect(host, port, certificate),
                            )
                            .await
                            .map_err(|_| {
                                NativeFault::new("TimeoutError", "TLS connect timed out")
                            })?
                        },
                        data,
                    )
                }
                else
                {
                    let connection = native::<TcpConnection>(input).cloned();
                    let host = rest.first().and_then(text).map(str::to_string);
                    let (Some(connection), Some(host)) = (connection, host)
                    else
                    {
                        return NativeOutcome::Ready(self.io_fault(
                            "OpenError",
                            command,
                            "Expected TCP connection and TLS server name".into(),
                        ));
                    };
                    self.native_wait(
                        command,
                        async move {
                            tokio::time::timeout(
                                Duration::from_secs(30),
                                tls_wrap(connection, host, certificate),
                            )
                            .await
                            .map_err(|_| {
                                NativeFault::new("TimeoutError", "TLS handshake timed out")
                            })?
                        },
                        data,
                    )
                }
            }
            #[cfg(unix)]
            "unix_connect" | "unix_listen" =>
            {
                let Some(path) = text(input).map(str::to_string)
                else
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "OpenError",
                        command,
                        "Expected Unix socket path".into(),
                    ));
                };
                if command == "unix_connect"
                {
                    self.native_wait(
                        command,
                        async move {
                            tokio::net::UnixStream::connect(path)
                                .await
                                .map(Connection::new)
                                .map_err(|error| NativeFault::io("OpenError", error))
                        },
                        data,
                    )
                }
                else
                {
                    self.native_wait(command, async move
                            { crate::language::data::net::sockets::unix::Listener::bind(path) }, data)
                }
            }
            #[cfg(unix)]
            "unix_accept" =>
            {
                let Some(listener) =
                    native::<crate::language::data::net::sockets::unix::Listener>(input).cloned()
                else
                {
                    return NativeOutcome::Ready(self.io_fault(
                        "OpenError",
                        command,
                        "Expected Unix listener".into(),
                    ));
                };
                self.native_wait(command, async move { listener.accept().await }, data)
            }
            _ => NativeOutcome::Ready(self.io_fault(
                "OpenError",
                command,
                "Unsupported socket operation".into(),
            )),
        }
    }
    pub(in crate::language::interpreter) fn handle_socket_close(
        &mut self,
        location: &Location,
        args: &[Value],
    ) -> InterpreterResult<()>
    {
        let mut value = Value::None;
        if let [input] = args
        {
            if let Some(socket) = native::<DatagramSocket>(input)
            {
                socket.close();
            }
            else if let Some(connection) = native::<Connection>(input)
            {
                connection.close();
            }
            else
            {
                #[cfg(unix)]
                if let Some(listener) =
                    native::<crate::language::data::net::sockets::unix::Listener>(input)
                {
                    listener.close();
                    self.last_result = Some(value);
                    return Ok(());
                }
                value = self.io_fault(
                    "ClosedError",
                    "socket.close",
                    "Expected socket handle".into(),
                );
            }
        }
        self.activate_failure(location, &mut value, false)?;
        self.last_result = Some(value);
        Ok(())
    }
}

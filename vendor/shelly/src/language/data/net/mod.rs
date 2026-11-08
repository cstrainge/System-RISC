pub mod http;
pub mod server;
pub mod sockets;
pub mod tcp;
pub mod upload;

use crate::language::{
    data::types::{TypeKind, TypeRegistry},
    native::{NativeType, NativeVisibility},
    text::location::Location,
};

impl TypeRegistry
{
    pub(crate) fn register_network_types(&mut self)
    {
        for name in [
            "__server_Server",
            "__server_Incoming",
            "__socket_Connection",
            "__socket_Listener",
            "__socket_DatagramSocket",
            "__tcp_Connection",
            "__tcp_Listener",
            "__http_Upload",
            "__http_Client",
            "__http_Response",
            "__http_DiagnosticResponse",
        ]
        {
            self.register_native(name.into(), TypeKind::Builtin, NativeVisibility::Hidden);
        }
        let string = self.builtin_id("String").unwrap();
        let integer = self.intern(TypeKind::Optional(self.builtin_id("Integer").unwrap()));
        let diagnostic = self.builtin_id("__http_DiagnosticResponse").unwrap();
        for (name, fields) in [
            (
                "StatusError",
                vec![("message", string), ("response", diagnostic)],
            ),
            ("ProtocolError", vec![("message", string)]),
            (
                "TransportError",
                vec![
                    ("message", string),
                    ("operation", string),
                    ("delivery", string),
                    ("code", integer),
                ],
            ),
        ]
        {
            let id = self.register(
                name.into(),
                TypeKind::Struct(self.native_fields(&fields)),
                Some(Location::new("<native>", 1, 1)),
            );
            self.mark_error(id);
            self.native_types.insert(
                format!("__http_{}", name),
                NativeType {
                    id,
                    visibility: NativeVisibility::Hidden,
                },
            );
        }
    }
}

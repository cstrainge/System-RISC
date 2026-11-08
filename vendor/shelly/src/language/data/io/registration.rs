
use crate::language::{
    data::types::{TypeKind, TypeRegistry},
    native::{NativeType, NativeVisibility},
    text::location::Location,
};

impl TypeRegistry
{
    pub(crate) fn register_io_types(&mut self)
    {
        for name in [
            "__io_Lines",
            "__io_Bytes",
            "__io_Reader",
            "__io_Writer",
            "__io_ByteSource",
            "__io_ByteSink",
        ]
        {
            self.register_native(name.into(), TypeKind::Builtin, NativeVisibility::Hidden);
        }
        let string = self.builtin_id("String").unwrap();
        let integer = self.intern(TypeKind::Optional(self.builtin_id("Integer").unwrap()));
        for name in [
            "ReadError",
            "WriteError",
            "ClosedError",
            "TimeoutError",
            "CancelledError",
            "LimitError",
            "DecodeError",
            "LeaseError",
            "OpenError",
        ]
        {
            let fields = self.native_fields(&[
                ("message", string),
                ("operation", string),
                ("code", integer),
            ]);
            let id = self.register(
                name.into(),
                TypeKind::Struct(fields),
                Some(Location::new("<native>", 1, 1)),
            );
            self.mark_error(id);
            self.native_types.insert(
                format!("__io_{}", name),
                NativeType {
                    id,
                    visibility: NativeVisibility::Hidden,
                },
            );
        }
    }
}

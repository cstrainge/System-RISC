
use crate::language::{
    data::types::{FieldDefinition, TypeKind, TypeRegistry},
    native::{NativeType, NativeVisibility},
    text::location::Location,
};

impl TypeRegistry
{
    pub(crate) fn register_json_types(&mut self)
    {
        for name in [
            "__JsonArrayReader",
            "__JsonDocument",
            "__JsonNumber",
            "__JsonCodec",
            "__JsonConversion",
        ]
        {
            self.register_native(name.into(), TypeKind::Builtin, NativeVisibility::Hidden);
        }
        let string = self.builtin_id("String").unwrap();
        let integer = self.builtin_id("Integer").unwrap();
        for name in [
            "SyntaxError",
            "EncodingError",
            "MappingError",
            "PathError",
            "LimitError",
            "ValidationError",
        ]
        {
            let fields = [
                ("message", string),
                ("path", string),
                ("line", integer),
                ("column", integer),
            ]
            .into_iter()
            .map(|(name, type_id)| FieldDefinition {
                name: name.into(),
                type_id,
                public: true,
                optional: false,
                location: Location::new("<native>", 1, 1),
            })
            .collect();
            let id = self.register(
                name.into(),
                TypeKind::Struct(fields),
                Some(Location::new("<native>", 1, 1)),
            );
            self.mark_error(id);
            self.native_types.insert(
                format!("__json_{}", name),
                NativeType {
                    id,
                    visibility: NativeVisibility::Hidden,
                },
            );
        }
    }
}

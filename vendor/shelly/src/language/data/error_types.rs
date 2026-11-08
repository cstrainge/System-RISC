
use crate::language::{
    data::types::{FieldDefinition, TypeId, TypeKind, TypeRegistry},
    native::NativeVisibility,
    text::location::Location,
};

impl TypeRegistry
{
    pub(super) fn register_error_types(&mut self)
    {
        let string = self.builtin_id("String").unwrap();
        let integer = self.builtin_id("Integer").unwrap();
        let boolean = self.builtin_id("Boolean").unwrap();
        let any = self.builtin_id("any").unwrap();
        let ty = self.builtin_id("Type").unwrap();
        let location = self.native_record(
            "SourceLocation",
            &[("origin", string), ("line", integer), ("column", integer)],
        );
        let optional_location = self.intern(TypeKind::Optional(location));
        let frame = self.native_record(
            "CallFrame",
            &[
                ("name", string),
                ("location", location),
                ("native", boolean),
                ("native_boundary", optional_location),
            ],
        );
        let frames = self.intern(TypeKind::Array(frame));
        let locations = self.intern(TypeKind::Array(location));
        let optional_string = self.intern(TypeKind::Optional(string));
        let snapshot = self.native_record(
            "ErrorSnapshot",
            &[
                ("original_type", ty),
                ("public_fields", any),
                ("formatted", optional_string),
                ("truncated", boolean),
            ],
        );
        let stack = self.register_native(
            "CallStack".into(),
            TypeKind::Pending,
            NativeVisibility::Visible,
        );
        let related = self.native_record(
            "RelatedFailure",
            &[("relation", string), ("error", snapshot), ("stack", stack)],
        );
        let related = self.intern(TypeKind::Array(related));
        self.finish(
            stack,
            TypeKind::Struct(self.native_fields(&[
                ("error_type", ty),
                ("origin", location),
                ("native_origin", optional_location),
                ("frames", frames),
                ("propagation", locations),
                ("related", related),
                ("truncated", boolean),
            ])),
        );
        let runtime = self.native_record("RuntimeError", &[("what", string), ("message", string)]);
        self.mark_error(runtime);
    }

    pub(crate) fn native_fields(&self, fields: &[(&str, TypeId)]) -> Vec<FieldDefinition>
    {
        fields
            .iter()
            .map(|(name, type_id)| FieldDefinition {
                public: true,
                name: (*name).into(),
                type_id: *type_id,
                optional: matches!(self.get(*type_id).kind, TypeKind::Optional(_)),
                location: Location::new("<native>", 1, 1),
            })
            .collect()
    }

    fn native_record(&mut self, name: &str, fields: &[(&str, TypeId)]) -> TypeId
    {
        self.register_native(
            name.into(),
            TypeKind::Struct(self.native_fields(fields)),
            NativeVisibility::Visible,
        )
    }
}

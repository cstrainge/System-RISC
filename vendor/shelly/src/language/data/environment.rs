
use crate::language::{ data::types::{ EnumVariant, TypeKind, TypeRegistry },
                       native::NativeVisibility,
                       text::location::Location };


impl TypeRegistry
{
    pub(super) fn register_environment_types(&mut self)
    {
        let variants = ["Local", "Exported"].into_iter().map(|name| EnumVariant
            {
                name: name.to_string(),
                location: Location::new("<native>", 1, 1),
            }).collect();
        let visibility = self.register_native("VariableVisibility".into(), TypeKind::Enum(variants),
                                              NativeVisibility::Hidden);
        let string = self.builtin_id("String").unwrap();
        let any = self.builtin_id("any").unwrap();
        let fields = self.native_fields(&[("name", string), ("value", any),
                                         ("visibility", visibility)]);
        self.register_native("VariableInfo".into(), TypeKind::Struct(fields),
                             NativeVisibility::Hidden);
    }
}

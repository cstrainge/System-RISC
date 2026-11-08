
use std::collections::HashMap;

use crate::language::data::{ map_key::MapKey, methods::NativeMethodError,
                             types::TypeKind, value::Value };


pub(super) fn properties(receiver: &mut Value, _: &[Value]) -> Result<Value, NativeMethodError>
{
    let mut members = HashMap::new();
    if let Value::Struct(item) = receiver.underlying()
    {
        if let TypeKind::Struct(fields) = &item.definition.kind
        {
            for (field, value) in fields.iter().zip(&item.fields)
            {
                if field.public
                { members.insert(MapKey::String(field.name.clone()), value.clone()); }
            }
        }
    }
    else
    {
        members.insert(MapKey::String("value".into()), receiver.clone());
        if let Value::Enum(item) = receiver.underlying()
        { members.insert(MapKey::String("index".into()), Value::Integer(item.variant as i64)); }
        if let Value::Type(item) = receiver.underlying()
        { members.insert(MapKey::String("name".into()), Value::from_string(item.name.clone())); }
    }
    Ok(Value::from_hash_map(members))
}

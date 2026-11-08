
use std::{ collections::HashMap, rc::Rc };

use crate::language::data::{ types::{ StructValue, TypeId, TypeKind, TypeRegistry },
                            map_key::MapKey, value::Value };


// Shared structural mapping for decoded objects and header-described tabular rows.
// Explicit scripted from_<source> factories take precedence in the interpreter.
pub fn map_value(
    types: &TypeRegistry,
    target: TypeId,
    value: Value,
    factory: &mut impl FnMut(TypeId, &Value) -> Result<Option<Value>, String>,
) -> Result<Value, String>
{
    if let Some(converted) = factory(target, &value)? { return Ok(converted); }
    let definition = types.get(target);
    if let TypeKind::Struct(fields) = &definition.kind
        && let Value::HashMap(values) = value.underlying()
    {
        for field in fields.iter().filter(|field| !field.public
            && !types.constructors.get(&target).is_some_and(|signature|
                signature.parameters.iter().any(|parameter| parameter.name == field.name)))
        {
            if values.contains_key(&MapKey::String(field.name.clone()))
            { return Err(format!("Field '{}.{}' is private", definition.name, field.name)); }
        }
    }
    let definition = types.get(target);
    match &definition.kind
    {
        TypeKind::Optional(_) if matches!(value, Value::None) => Ok(Value::None),
        TypeKind::Optional(inner) => map_value(types, *inner, value, factory),
        TypeKind::Builtin if definition.name == "Boolean" => match value.underlying()
            {
                Value::String(text, _) if text == "true" => Ok(Value::Boolean(true)),
                Value::String(text, _) if text == "false" => Ok(Value::Boolean(false)),
                Value::Boolean(value) => Ok(Value::Boolean(*value)),
                _ => Err("Decoded Boolean must be true or false".into()),
            },
        TypeKind::Named(inner) =>
            {
                let mapped = map_value(types, *inner, value, factory)?;
                types.coerce(target, mapped)
            },
        TypeKind::Struct(fields) if matches!(value.underlying(), Value::HashMap(_)) =>
            {
                let Value::HashMap(values) = value.underlying() else { unreachable!(); };
                let mut remaining: HashMap<_, _> = values.iter().collect();
                let mut mapped = Vec::new();
                if let Some(signature) = types.constructors.get(&target)
                {
                    if !types.constructor_permissions.contains(&target)
                    { return Err("Constructor is private to its module".into()); }
                    for (index, parameter) in signature.parameters.iter().enumerate()
                    {
                        let key = MapKey::String(parameter.name.clone());
                        let value = remaining.remove(&key).cloned();
                        let value = match value
                            {
                                Some(value) => map_value(types, parameter.type_id, value, factory)?,
                                None if parameter.optional => signature.defaults()[index].clone(),
                                None => return Err(format!(
                                    "Missing constructor parameter '{}'", parameter.name)),
                            };
                        mapped.push(value);
                    }
                    if !remaining.is_empty()
                    { return Err("Unknown constructor parameters in decoded object".into()); }
                    let inputs = Value::Struct(Rc::new(StructValue
                        { definition, fields: mapped, constructing: true, failure: None }));
                    return factory(target, &inputs)?
                        .ok_or_else(|| "Constructor factory is unavailable".into());
                }
                for field in fields
                {
                    let key = MapKey::String(field.name.clone());
                    let value = remaining.remove(&key).cloned();
                    if !field.public
                    {
                        if types.validate(field.type_id, &Value::None).is_err()
                        { return Err(format!("Private field '{}.{}' requires ::new",
                            definition.name, field.name)); }
                        mapped.push(Value::None);
                        continue;
                    }
                    let value = value.or_else(|| field.optional.then_some(Value::None))
                        .ok_or_else(|| format!(
                            "Missing field '{}.{}'", definition.name, field.name))?;
                    mapped.push(map_value(types, field.type_id, value, factory)
                        .map_err(|error| format!("Field '{}': {}", field.name, error))?);
                }
                if !remaining.is_empty() { return Err("Unknown fields in decoded object".into()); }
                let value = Value::Struct(Rc::new(StructValue
                    { definition, fields: mapped, constructing: false, failure: None }));
                types.coerce(target, value)
            },
        TypeKind::Array(item) =>
            {
                let Value::Array(values) = value.underlying() else
                    { return Err("Decoded value must be an Array".into()); };
                Ok(Value::from_array(values.iter()
                    .map(|value| map_value(types, *item, value.clone(), factory))
                    .collect::<Result<_, _>>()?))
            },
        TypeKind::FixedArray(items) =>
            {
                let Value::Array(values) = value.underlying() else
                    { return Err("Decoded value must be an Array".into()); };
                if items.len() != values.len()
                { return Err("Decoded row has wrong length".into()); }
                Ok(Value::from_array(items.iter().zip(values.iter())
                    .map(|(id, value)| map_value(types, *id, value.clone(), factory))
                    .collect::<Result<_, _>>()?))
            },
        TypeKind::Map(key_type, value_type) =>
            {
                let Value::HashMap(values) = value.underlying() else
                    { return Err("Decoded value must be a HashMap".into()); };
                let mut mapped = HashMap::new();
                for (key, value) in values.iter()
                {
                    let key = map_value(types, *key_type, key.to_value(), factory)?;
                    let key = MapKey::from_value(&key);
                    let value = map_value(types, *value_type, value.clone(), factory)?;
                    if mapped.insert(key, value).is_some()
                    { return Err("Decoded keys collide after conversion".into()); }
                }
                Ok(Value::from_hash_map(mapped))
            },
        _ => types.convert(target, &value),
    }
}


use std::{collections::HashMap, rc::Rc, sync::Arc, time::Instant};

use crate::language::{
    bytecode::FunctionRef,
    data::{
        formats::json::{
            document::{Document, JsonNode, document, exact_number},
            parser::JsonError,
            values::{float, integer},
        },
        map_key::MapKey,
        methods::method_key,
        types::{EnumValue, NamedValue, StructValue, TypeId, TypeKind},
        value::{Executable, Value},
    },
    interpreter::{Interpreter, exceptions::CleanupGuard},
    text::location::Location,
};

use super::{Failure, Operation, Result};

fn invalid(message: impl Into<String>) -> Failure
{
    JsonError::new("MappingError", message).into()
}

impl Interpreter
{
    pub(super) fn json_method(
        &self,
        operation: &mut Operation,
        id: TypeId,
        name: &str,
    ) -> Option<FunctionRef>
    {
        if operation.frozen
        {
            return operation.methods.get(&(id, name.into())).cloned().flatten();
        }
        operation
            .methods
            .entry((id, name.into()))
            .or_insert_with(|| {
                let key = method_key(id, name);
                let scope = &self.scopes[&operation.scope];
                scope
                    .lexical_function(&key)
                    .or_else(|| scope.imported_methods.get(&key).cloned())
            })
            .clone()
    }

    pub(super) fn json_invoke(
        &mut self,
        location: &Location,
        function: &FunctionRef,
        args: &[Value],
        operation: &mut Operation,
    ) -> Result<Value>
    {
        if operation.hook_instructions == 0 || operation.hook_time.is_zero()
        {
            return Err(
                JsonError::new("LimitError", "JSON hook operation budget exhausted").into(),
            );
        }
        if self.exceptions.json_calls >= 128
        {
            return Err(
                JsonError::new("LimitError", "JSON protocol recursion limit exceeded").into(),
            );
        }
        self.exceptions.json_calls += 1;
        let previous = self.last_result.take();
        let parent = self.exceptions.cleanup.take();
        let budget = parent
            .as_ref()
            .map_or(operation.hook_instructions, |guard| {
                guard.instructions.min(operation.hook_instructions)
            });
        let started = Instant::now();
        let deadline = operation
            .deadline
            .map_or(started + operation.hook_time, |deadline| {
                deadline.min(started + operation.hook_time)
            });
        self.exceptions.cleanup = Some(CleanupGuard {
            hook: true,
            deadline: parent
                .as_ref()
                .map_or(deadline, |guard| guard.deadline.min(deadline)),
            instructions: budget,
            interrupted: parent.as_ref().is_some_and(|guard| guard.interrupted),
            function_depth: self.exceptions.frames.len(),
        });
        let outcome = self.execute_function(location, "<JSON conversion>", function, args);
        let result = self.last_result.take().unwrap_or(Value::None);
        self.last_result = previous;
        let completed = self.exceptions.cleanup.take();
        let interrupted = completed.as_ref().is_some_and(|guard| guard.interrupted);
        let consumed = budget.saturating_sub(completed.map_or(0, |guard| guard.instructions));
        self.exceptions.cleanup = parent.map(|mut guard| {
            guard.instructions = guard.instructions.saturating_sub(consumed);
            guard.interrupted |= interrupted;
            guard
        });
        operation.hook_instructions = operation.hook_instructions.saturating_sub(consumed);
        operation.hook_time = operation.hook_time.saturating_sub(started.elapsed());
        self.exceptions.json_calls -= 1;
        if let Err(error) = outcome
        {
            if matches!(
                error.what,
                crate::language::interpreter::ErrorWhat::HookTimeout
            )
            {
                return Err(
                    JsonError::new("LimitError", "JSON conversion hook budget exhausted").into(),
                );
            }

            return Err(
                if matches!(error.what, crate::language::interpreter::ErrorWhat::ReturnContract(_, id)
                if id == Rc::as_ptr(function) as usize)
                {
                    Failure::Contract(error)
                }
                else
                {
                    Failure::Raised(error)
                },
            );
        }
        Ok(result)
    }

    pub(super) fn json_wrap(
        &mut self,
        location: &Location,
        value: &Value,
        operation: &mut Operation,
        depth: usize,
    ) -> Result<Arc<JsonNode>>
    {
        operation.visit(depth)?;
        self.consume_success(location, value)?;
        if let Some(document) = document(value)
        {
            return Ok(document.value.clone());
        }
        if let Some(number) = exact_number(value)
        {
            return Ok(Arc::new(JsonNode::Number(number.0.clone())));
        }
        let id = operation.types.value_type(value);
        if let Some(function) = self.json_method(operation, id, "to_json")
        {
            let result = self.json_invoke(location, &function, &[value.clone()], operation)?;
            self.consume_success(location, &result)?;
            return document(&result)
                .map(|document| document.value.clone())
                .ok_or_else(|| invalid("to_json must return json::Document"));
        }
        let codec = self.json_codec(location, id, operation)?;
        let value = value.underlying();
        Ok(Arc::new(match value
        {
            Value::None => JsonNode::Null,
            Value::Boolean(value) => JsonNode::Boolean(*value),
            Value::Integer(value) => JsonNode::Number(value.to_string()),
            Value::Float(value, _) if value.is_finite() => JsonNode::Number(value.to_string()),
            Value::String(value, Executable::No) => JsonNode::String(value.clone()),
            Value::Enum(item) =>
            {
                let TypeKind::Enum(variants) = &item.definition.kind
                else
                {
                    unreachable!();
                };
                JsonNode::String(
                    codec
                        .as_ref()
                        .and_then(|codec| {
                            codec
                                .enum_names
                                .iter()
                                .find(|(variant, _)| *variant == item.variant)
                                .map(|(_, name)| name.clone())
                        })
                        .unwrap_or_else(|| variants[item.variant].name.clone()),
                )
            }
            Value::Array(items) => JsonNode::Array(
                items
                    .iter()
                    .map(|item| self.json_wrap(location, item, operation, depth + 1))
                    .collect::<Result<_>>()?,
            ),
            Value::HashMap(items) =>
            {
                let mut fields = Vec::new();
                for (key, value) in items.iter()
                {
                    let key = match key.to_value().underlying()
                    {
                        Value::String(key, Executable::No) => key.clone(),
                        Value::Enum(item) =>
                        {
                            let TypeKind::Enum(variants) = &item.definition.kind
                            else
                            {
                                unreachable!();
                            };
                            self.json_codec(location, item.definition.id, operation)?
                                .as_ref()
                                .and_then(|codec| {
                                    codec
                                        .enum_names
                                        .iter()
                                        .find(|(id, _)| *id == item.variant)
                                        .map(|(_, name)| name.clone())
                                })
                                .unwrap_or_else(|| variants[item.variant].name.clone())
                        }
                        _ =>
                        {
                            return Err(JsonError::new(
                                "EncodingError",
                                "JSON object keys require String or unit enum",
                            )
                            .into());
                        }
                    };
                    if fields.iter().any(|(existing, _)| existing == &key)
                    {
                        return Err(JsonError::new("EncodingError", "JSON key collision").into());
                    }
                    fields.push((key, self.json_wrap(location, value, operation, depth + 1)?));
                }
                fields.sort_by(|(left, _), (right, _)| left.cmp(right));
                JsonNode::Object(fields)
            }
            Value::Struct(item) =>
            {
                let TypeKind::Struct(fields) = &item.definition.kind
                else
                {
                    unreachable!();
                };
                if !fields.is_empty() && !fields.iter().any(|field| field.public)
                {
                    return Err(JsonError::new("EncodingError", "NoPublicRepresentation").into());
                }
                let mut object = Vec::new();
                for (field, value) in fields.iter().zip(&item.fields)
                {
                    if field.public
                    {
                        if matches!(value, Value::None)
                            && codec
                                .as_ref()
                                .is_some_and(|codec| codec.omit_null.contains(&field.name))
                        {
                            continue;
                        }
                        let name = codec
                            .as_ref()
                            .and_then(|codec| {
                                codec
                                    .field_names
                                    .iter()
                                    .find(|(original, _)| *original == field.name)
                                    .map(|(_, name)| name.clone())
                            })
                            .unwrap_or_else(|| field.name.clone());
                        object.push((name, self.json_wrap(location, value, operation, depth + 1)?));
                    }
                }
                JsonNode::Object(object)
            }
            value =>
            {
                return Err(JsonError::new(
                    "EncodingError",
                    format!("JSON cannot encode {}", value.type_name()),
                )
                .into());
            }
        }))
    }

    pub(super) fn json_convert(
        &mut self,
        location: &Location,
        value: &Value,
        target: TypeId,
        operation: &mut Operation,
        depth: usize,
    ) -> Result<Value>
    {
        if let Some(doc) = document(value)
        {
            if operation
                .types
                .get(operation.types.underlying_type(target))
                .name
                == "__JsonDocument"
            {
                return operation
                    .types
                    .coerce(target, value.clone())
                    .map_err(invalid);
            }
            if let Some(function) = self.json_method(operation, target, "from_json")
            {
                let result = self.json_invoke(location, &function, &[value.clone()], operation)?;
                return self.json_checked(location, target, result, operation);
            }
            return self.json_map(location, doc, target, operation, depth);
        }
        let source = operation.types.inferred_value_type(value);
        let name = operation.types.conversion_method_name(source);
        let typed_name = format!("{}#{}", name, source.0);
        if let Some(function) = self
            .json_method(operation, target, &typed_name)
            .or_else(|| self.json_method(operation, target, &name))
        {
            let result = self.json_invoke(location, &function, &[value.clone()], operation)?;
            return self.json_checked(location, target, result, operation);
        }
        if operation.types.validate(target, value).is_ok()
        {
            return Ok(value.clone());
        }
        let doc = Document::new(self.json_wrap(location, value, operation, depth)?);
        if let Some(function) = self.json_method(operation, target, "from_json")
        {
            let result = self.json_invoke(location, &function, &[doc.into_value()], operation)?;
            return self.json_checked(location, target, result, operation);
        }
        self.json_map(location, &doc, target, operation, depth)
    }

    fn json_checked(
        &mut self,
        location: &Location,
        target: TypeId,
        mut value: Value,
        operation: &Operation,
    ) -> Result<Value>
    {
        if crate::language::data::failure::pending(&value).is_some()
        {
            return Ok(value);
        }
        operation.types.validate(target, &value).map_err(invalid)?;
        self.activate_failure(location, &mut value, true)?;
        Ok(value)
    }

    fn json_map(
        &mut self,
        location: &Location,
        document: &Document,
        target: TypeId,
        operation: &mut Operation,
        depth: usize,
    ) -> Result<Value>
    {
        operation.visit(depth)?;
        let definition = operation.types.get(target);
        let codec = self.json_codec(location, target, operation)?;
        let node = document.value.as_ref();
        let mapped = match &definition.kind
        {
            TypeKind::Named(inner) => Value::Named(Rc::new(NamedValue {
                definition: definition.clone(),
                value: self.json_convert(
                    location,
                    &document.clone().into_value(),
                    *inner,
                    operation,
                    depth + 1,
                )?,
            })),
            TypeKind::Optional(_) if matches!(node, JsonNode::Null) => Value::None,
            TypeKind::Optional(inner) =>
            {
                return self.json_convert(
                    location,
                    &document.clone().into_value(),
                    *inner,
                    operation,
                    depth + 1,
                );
            }
            TypeKind::Union(items) =>
            {
                let candidates: Vec<_> = items
                    .iter()
                    .filter(|id| json_shape(&operation.types, **id, node, 0))
                    .copied()
                    .collect();
                if candidates.len() != 1
                {
                    return Err(invalid(
                        if candidates.is_empty()
                        {
                            "No union member matches JSON value"
                        }
                        else
                        {
                            "AmbiguousUnion"
                        },
                    ));
                }
                return self.json_convert(
                    location,
                    &document.clone().into_value(),
                    candidates[0],
                    operation,
                    depth + 1,
                );
            }
            TypeKind::Builtin => match (definition.name.as_str(), node)
            {
                ("any", _) => crate::language::data::formats::json::values::materialize(
                    node,
                    operation.exact,
                )?,
                ("String", JsonNode::String(value)) => Value::from_string(value.clone()),
                ("Boolean", JsonNode::Boolean(value)) => Value::Boolean(*value),
                ("Integer", JsonNode::Number(value)) => Value::Integer(integer(value)?),
                ("Float", JsonNode::Number(value)) => Value::Float(float(value)?, None),
                ("Number", JsonNode::Number(value)) => integer(value)
                    .map(Value::Integer)
                    .unwrap_or(Value::Float(float(value)?, None)),
                ("None", JsonNode::Null) => Value::None,
                ("Array", JsonNode::Array(_)) | ("HashMap", JsonNode::Object(_)) =>
                {
                    crate::language::data::formats::json::values::materialize(
                        node,
                        operation.exact,
                    )?
                }
                ("__JsonDocument", _) => document.clone().into_value(),
                ("__JsonNumber", JsonNode::Number(value)) =>
                {
                    Value::Native(crate::language::data::native_value::NativeValue::new(
                        crate::language::data::formats::json::document::ExactNumber(value.clone()),
                    ))
                }
                _ =>
                {
                    return Err(invalid(format!(
                        "Expected JSON representation of {}",
                        definition.name
                    )));
                }
            },
            TypeKind::Enum(variants) =>
            {
                let JsonNode::String(value) = node
                else
                {
                    return Err(invalid("Enum requires JSON String"));
                };
                let variant = codec
                    .as_ref()
                    .and_then(|codec| {
                        codec
                            .enum_names
                            .iter()
                            .find(|(_, name)| name == value)
                            .map(|(id, _)| *id)
                    })
                    .or_else(|| {
                        variants
                            .iter()
                            .enumerate()
                            .find(|(id, variant)| {
                                variant.name == *value
                                    && !codec.as_ref().is_some_and(|codec| {
                                        codec.enum_names.iter().any(|(renamed, _)| id == renamed)
                                    })
                            })
                            .map(|(id, _)| id)
                    })
                    .ok_or_else(|| invalid("Unknown enum variant"))?;
                Value::Enum(Rc::new(EnumValue {
                    definition: definition.clone(),
                    variant,
                }))
            }
            TypeKind::Array(_) | TypeKind::FixedArray(_) =>
            {
                let JsonNode::Array(items) = node
                else
                {
                    return Err(invalid("Expected JSON Array"));
                };
                let targets = if let TypeKind::FixedArray(types) = &definition.kind
                {
                    if items.len() != types.len()
                    {
                        return Err(invalid("Fixed array length mismatch"));
                    }
                    types.clone()
                }
                else if let TypeKind::Array(element) = &definition.kind
                {
                    vec![*element; items.len()]
                }
                else
                {
                    unreachable!();
                };
                let mut values = Vec::new();
                for (item, target) in items.iter().zip(targets)
                {
                    let value = self.json_convert(
                        location,
                        &document.view(item.clone()).into_value(),
                        target,
                        operation,
                        depth + 1,
                    )?;
                    self.consume_success(location, &value)?;
                    values.push(value);
                }
                Value::from_array(values).with_collection_type(target)
            }
            TypeKind::Map(key_type, value_type) =>
            {
                let JsonNode::Object(items) = node
                else
                {
                    return Err(invalid("Expected JSON Object"));
                };
                let mut values = HashMap::new();
                for (key, value) in items
                {
                    let key = self.json_convert(
                        location,
                        &Document::new(Arc::new(JsonNode::String(key.clone()))).into_value(),
                        *key_type,
                        operation,
                        depth + 1,
                    )?;
                    self.consume_success(location, &key)?;
                    let key = MapKey::from_value(&key);
                    if values.contains_key(&key)
                    {
                        return Err(invalid("JSON key collision"));
                    }
                    let value = self.json_convert(
                        location,
                        &document.view(value.clone()).into_value(),
                        *value_type,
                        operation,
                        depth + 1,
                    )?;
                    self.consume_success(location, &value)?;
                    values.insert(key, value);
                }
                Value::from_hash_map(values).with_collection_type(target)
            }
            TypeKind::Struct(fields) =>
            {
                let JsonNode::Object(items) = node
                else
                {
                    return Err(invalid("Struct requires JSON Object"));
                };
                let signature = operation.types.constructors.get(&target).cloned();
                if signature.is_some() && !operation.types.constructor_permissions.contains(&target)
                {
                    return Err(invalid("Constructor is private to the conversion caller"));
                }
                let parameters: Vec<_> = signature
                    .as_ref()
                    .map(|signature| {
                        signature
                            .parameters
                            .iter()
                            .map(|parameter| {
                                (
                                    parameter.name.clone(),
                                    parameter.type_id,
                                    parameter.optional,
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_else(|| {
                        fields
                            .iter()
                            .map(|field| (field.name.clone(), field.type_id, field.optional))
                            .collect()
                    });
                let mut values = Vec::new();
                for (name, ty, optional) in &parameters
                {
                    let wire = codec
                        .as_ref()
                        .and_then(|codec| {
                            codec
                                .field_names
                                .iter()
                                .find(|(original, _)| original == name)
                                .map(|(_, wire)| wire)
                        })
                        .unwrap_or(name);
                    let node = items
                        .iter()
                        .find(|(key, _)| key == wire)
                        .map(|(_, value)| value.clone());
                    if signature.is_none()
                        && fields
                            .iter()
                            .any(|field| field.name == *name && !field.public)
                    {
                        return Err(invalid(format!(
                            "Private field {} requires a constructor",
                            name
                        )));
                    }
                    let value = match node
                    {
                        Some(node) => self.json_convert(
                            location,
                            &document.view(node).into_value(),
                            *ty,
                            operation,
                            depth + 1,
                        )?,
                        None if *optional => Value::None,
                        None => return Err(invalid(format!("Missing field '{}'", name))),
                    };
                    self.consume_success(location, &value)?;
                    values.push(value);
                }
                let ignore = if operation.unknown_explicit
                {
                    operation.ignore_unknown
                }
                else
                {
                    codec
                        .as_ref()
                        .and_then(|codec| codec.ignore_unknown)
                        .unwrap_or(false)
                };
                if !ignore
                    && items.iter().any(|(key, _)| {
                        !parameters.iter().any(|(name, _, _)| {
                            codec
                                .as_ref()
                                .and_then(|codec| {
                                    codec
                                        .field_names
                                        .iter()
                                        .find(|(original, _)| original == name)
                                        .map(|(_, wire)| wire)
                                })
                                .unwrap_or(name)
                                == key
                        })
                    })
                {
                    return Err(invalid("Unknown fields in JSON object"));
                }
                if let Some(signature) = signature
                {
                    let function = self
                        .json_method(operation, target, "new")
                        .ok_or_else(|| invalid("Constructor unavailable"))?;
                    let args = signature.arguments(values).map_err(invalid)?;
                    let value = self.json_invoke(location, &function, &args, operation)?;
                    return self.json_checked(location, target, value, operation);
                }
                Value::Struct(Rc::new(StructValue {
                    definition: definition.clone(),
                    fields: values,
                    constructing: false,
                    failure: None,
                }))
            }
            _ => return Err(invalid("Type has no JSON representation")),
        };
        self.json_checked(location, target, mapped, operation)
    }
}

fn json_shape(
    types: &crate::language::data::types::TypeRegistry,
    target: TypeId,
    node: &JsonNode,
    depth: usize,
) -> bool
{
    if depth > 128
    {
        return false;
    }
    match &types.get(target).kind
    {
        TypeKind::Named(inner) => json_shape(types, *inner, node, depth + 1),
        TypeKind::Optional(inner) =>
        {
            matches!(node, JsonNode::Null) || json_shape(types, *inner, node, depth + 1)
        }
        TypeKind::Union(items) => items
            .iter()
            .any(|id| json_shape(types, *id, node, depth + 1)),
        TypeKind::Builtin => matches!(
            (types.get(target).name.as_str(), node),
            ("any", _)
                | ("None", JsonNode::Null)
                | ("String", JsonNode::String(_))
                | ("Boolean", JsonNode::Boolean(_))
                | ("Integer" | "Float" | "Number", JsonNode::Number(_))
                | ("Array", JsonNode::Array(_))
                | ("HashMap", JsonNode::Object(_))
        ),
        TypeKind::Enum(variants) => matches!(node, JsonNode::String(value)
            if variants.iter().any(|variant| variant.name == *value)),
        TypeKind::Array(_) | TypeKind::FixedArray(_) => matches!(node, JsonNode::Array(_)),
        TypeKind::Map(_, _) => matches!(node, JsonNode::Object(_)),
        TypeKind::Struct(fields) => matches!(node, JsonNode::Object(items)
            if fields.iter().all(|field| field.optional || items.iter().any(|(name, _)| name == &field.name))),
        _ => false,
    }
}

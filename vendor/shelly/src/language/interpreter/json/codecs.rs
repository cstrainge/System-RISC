
use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use crate::language::{
    data::{
        formats::json::{document::Codec, parser::JsonError},
        native_value::NativeValue,
        types::{TypeId, TypeKind},
        value::Value,
    },
    interpreter::{Interpreter, exceptions::CleanupGuard},
    text::location::Location,
};

use super::{Operation, Result, enum_name, field, text};

impl Interpreter
{
    pub(super) fn json_make_codec(
        &self,
        command: &str,
        args: &[Value],
        operation: &Operation,
    ) -> Result<Value>
    {
        let Some(Value::Type(target)) = args.first()
        else
        {
            return Err(JsonError::new("ValidationError", "Codec requires a target Type").into());
        };
        let mut codec = Codec {
            target: target.id,
            enum_names: Vec::new(),
            field_names: Vec::new(),
            omit_null: Vec::new(),
            ignore_unknown: None,
        };
        let empty = Value::None;
        let options = args.get(1).unwrap_or(&empty);
        if command == "enum_codec"
        {
            let TypeKind::Enum(variants) = &target.kind
            else
            {
                return Err(
                    JsonError::new("ValidationError", "enum_codec requires unit enum").into(),
                );
            };
            let Value::HashMap(names) = options.underlying()
            else
            {
                return Err(JsonError::new("ValidationError", "enum_codec requires a hash").into());
            };
            for (key, value) in names.iter()
            {
                let Value::Enum(item) = key.to_value()
                else
                {
                    return Err(JsonError::new(
                        "ValidationError",
                        "Codec keys must be enum variants",
                    )
                    .into());
                };
                if item.definition.id != target.id
                {
                    return Err(JsonError::new(
                        "ValidationError",
                        "Codec variant belongs to another type",
                    )
                    .into());
                }
                codec.enum_names.push((item.variant, text(value)?.into()));
            }
            let mut names = HashSet::new();
            for (index, variant) in variants.iter().enumerate()
            {
                let name = codec
                    .enum_names
                    .iter()
                    .find(|(id, _)| *id == index)
                    .map(|(_, name)| name)
                    .unwrap_or(&variant.name);
                if !names.insert(name)
                {
                    return Err(
                        JsonError::new("ValidationError", "Duplicate enum wire name").into(),
                    );
                }
            }
        }
        else
        {
            let TypeKind::Struct(fields) = &target.kind
            else
            {
                return Err(
                    JsonError::new("ValidationError", "struct_codec requires a struct").into(),
                );
            };
            if let Some(rename) = field(options, "rename_fields")
            {
                let Value::HashMap(rename) = rename.underlying()
                else
                {
                    return Err(
                        JsonError::new("ValidationError", "rename_fields must be a hash").into(),
                    );
                };
                for (key, value) in rename.iter()
                {
                    let key = key.to_value();
                    let key = text(&key)?;
                    if !fields.iter().any(|field| field.name == key && field.public)
                    {
                        return Err(JsonError::new(
                            "ValidationError",
                            "Codec references inaccessible field",
                        )
                        .into());
                    }
                    codec.field_names.push((key.into(), text(value)?.into()));
                }
            }
            if let Some(omit) = field(options, "omit_null_fields")
            {
                let Value::Array(omit) = omit.underlying()
                else
                {
                    return Err(JsonError::new(
                        "ValidationError",
                        "omit_null_fields must be an array",
                    )
                    .into());
                };
                for value in omit.iter()
                {
                    let name = text(value)?;
                    let field = fields
                        .iter()
                        .find(|field| field.name == name && field.public)
                        .ok_or_else(|| {
                            JsonError::new("ValidationError", "Codec references inaccessible field")
                        })?;
                    if operation
                        .types
                        .validate(field.type_id, &Value::None)
                        .is_err()
                    {
                        return Err(JsonError::new(
                            "ValidationError",
                            "Omitted-null field must allow null",
                        )
                        .into());
                    }
                    codec.omit_null.push(name.into());
                }
            }
            let mut names = HashSet::new();
            for field in fields.iter().filter(|field| field.public)
            {
                let name = codec
                    .field_names
                    .iter()
                    .find(|(original, _)| *original == field.name)
                    .map(|(_, wire)| wire)
                    .unwrap_or(&field.name);
                if !names.insert(name)
                {
                    return Err(
                        JsonError::new("ValidationError", "Duplicate struct wire name").into(),
                    );
                }
            }
            codec.ignore_unknown = field(options, "unknown_fields")
                .and_then(enum_name)
                .map(|name| name == "Ignore");
        }
        Ok(Value::Native(NativeValue::new(codec)))
    }

    pub(super) fn json_codec(
        &mut self,
        location: &Location,
        target: TypeId,
        operation: &mut Operation,
    ) -> Result<Option<Codec>>
    {
        if let Some(codec) = operation.codecs.get(&target)
        {
            return Ok(codec.clone());
        }
        let Some(provider) = self.json_method(operation, target, "json_codec")
        else
        {
            operation.codecs.insert(target, None);
            return Ok(None);
        };
        if !self.exceptions.json_providers.insert(target)
        {
            return Err(JsonError::new("ValidationError", "Recursive json_codec provider").into());
        }
        let parent = self.exceptions.cleanup.take();
        let budget = parent
            .as_ref()
            .map_or(100_000, |guard| guard.instructions.min(100_000));
        let deadline = Instant::now() + Duration::from_millis(100);
        self.exceptions.cleanup = Some(CleanupGuard {
            hook: true,
            deadline: parent
                .as_ref()
                .map_or(deadline, |guard| guard.deadline.min(deadline)),
            instructions: budget,
            interrupted: parent.as_ref().is_some_and(|guard| guard.interrupted),
            function_depth: self.exceptions.frames.len(),
        });
        let reporting = self.exceptions.reporting;
        let floor = self.exceptions.report_floor;
        self.exceptions.reporting = true;
        self.exceptions.report_floor = self.exceptions.regions.len();
        let outcome = self.json_invoke(location, &provider, &[], operation);
        let completed = self.exceptions.cleanup.take();
        let interrupted = completed.as_ref().is_some_and(|guard| guard.interrupted);
        let consumed = budget.saturating_sub(completed.map_or(0, |guard| guard.instructions));
        self.exceptions.cleanup = parent.map(|mut guard| {
            guard.instructions = guard.instructions.saturating_sub(consumed);
            guard.interrupted |= interrupted;
            guard
        });
        self.exceptions.reporting = reporting;
        self.exceptions.report_floor = floor;
        self.exceptions.json_providers.remove(&target);
        let result = outcome?;
        self.consume_success(location, &result)?;
        let codec = if let Value::Native(native) = result.underlying()
        {
            native.downcast::<Codec>().cloned()
        }
        else
        {
            None
        };
        let codec = codec
            .ok_or_else(|| JsonError::new("ValidationError", "Provider must return json::Codec"))?;
        if codec.target != target
        {
            return Err(JsonError::new(
                "ValidationError",
                "Codec target differs from its provider type",
            )
            .into());
        }
        operation.codecs.insert(target, Some(codec.clone()));
        Ok(Some(codec))
    }
}

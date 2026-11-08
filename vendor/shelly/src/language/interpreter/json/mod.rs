mod reader;

mod codecs;
mod mapping;
mod streaming;

use std::{any::Any, collections::HashMap, rc::Rc, time::Duration};

use crate::language::{
    bytecode::FunctionRef,
    data::{
        failure::occurrence,
        formats::json::{
            document::{Codec, Document, JsonNode, document, exact_number},
            parser::{DuplicateNames, JsonError, Limits, parse_document},
            values::{edit, encode_document, find, float, integer, materialize, pointer_parts},
        },
        map_key::MapKey,
        types::{StructValue, TypeId, TypeKind, TypeRegistry},
        value::{Executable, Value},
    },
    interpreter::{Interpreter, InterpreterError, InterpreterResult},
    text::location::Location,
};

pub(super) enum Failure
{
    Json(JsonError),
    Contract(InterpreterError),
    Raised(InterpreterError),
}
type Result<T> = std::result::Result<T, Failure>;
impl From<JsonError> for Failure
{
    fn from(value: JsonError) -> Self
    {
        Self::Json(value)
    }
}
impl From<InterpreterError> for Failure
{
    fn from(value: InterpreterError) -> Self
    {
        Self::Raised(value)
    }
}

pub(super) struct Operation
{
    pub types: TypeRegistry,
    pub scope: String,
    pub methods: HashMap<(TypeId, String), Option<FunctionRef>>,
    pub codecs: HashMap<TypeId, Option<Codec>>,
    pub limits: Limits,
    pub members: usize,
    pub ignore_unknown: bool,
    pub unknown_explicit: bool,
    pub exact: bool,
    pub frozen: bool,
    pub hook_instructions: usize,
    pub hook_time: Duration,
    pub deadline: Option<std::time::Instant>,
}

struct Conversion
{
    target: TypeId,
    types: TypeRegistry,
    scope: String,
    methods: HashMap<(TypeId, String), Option<FunctionRef>>,
    codecs: HashMap<TypeId, Option<Codec>>,
}

impl crate::language::data::native_value::NativeData for Conversion
{
    fn type_name(&self) -> &'static str
    {
        "__JsonConversion"
    }
    fn display(&self) -> String
    {
        "<Conversion>".into()
    }
    fn as_any(&self) -> &dyn Any
    {
        self
    }
}

fn field<'a>(value: &'a Value, name: &str) -> Option<&'a Value>
{
    match value.underlying()
    {
        Value::HashMap(items) => items.get(&MapKey::String(name.into())),
        Value::Struct(item) =>
        {
            if let TypeKind::Struct(fields) = &item.definition.kind
            {
                fields
                    .iter()
                    .position(|field| field.name == name)
                    .map(|index| &item.fields[index])
            }
            else
            {
                None
            }
        }
        _ => None,
    }
    .filter(|value| !matches!(value.underlying(), Value::None))
}

fn text(value: &Value) -> Result<&str>
{
    match value.underlying()
    {
        Value::String(value, Executable::No) => Ok(value),
        _ => Err(JsonError::new("MappingError", "Expected String").into()),
    }
}

fn enum_name(value: &Value) -> Option<&str>
{
    if let Value::Enum(item) = value.underlying()
        && let TypeKind::Enum(variants) = &item.definition.kind
    {
        Some(&variants[item.variant].name)
    }
    else
    {
        None
    }
}

impl Operation
{
    fn new(types: TypeRegistry, scope: String, options: &Value) -> Result<Self>
    {
        let mut limits = Limits::default();
        for (name, slot, ceiling) in [
            ("max_bytes", &mut limits.bytes, 64 * 1024 * 1024),
            ("max_depth", &mut limits.depth, 256),
            ("max_scalar", &mut limits.scalar, 16 * 1024 * 1024),
            ("max_members", &mut limits.members, 1_000_000),
        ]
        {
            if let Some(value) = field(options, name)
            {
                let Value::Integer(value) = value.underlying()
                else
                {
                    return Err(JsonError::new("ValidationError", "Limit must be Integer").into());
                };
                if *value < 1 || *value as u64 > ceiling as u64
                {
                    return Err(JsonError::new(
                        "ValidationError",
                        format!("{} must be between 1 and {}", name, ceiling),
                    )
                    .into());
                }
                *slot = *value as usize;
            }
        }
        limits.duplicates = match field(options, "duplicate_names").and_then(enum_name)
        {
            Some("FirstWins") => DuplicateNames::FirstWins,
            Some("LastWins") => DuplicateNames::LastWins,
            _ => DuplicateNames::Reject,
        };
        Ok(Self {
            types,
            scope,
            methods: HashMap::new(),
            codecs: HashMap::new(),
            limits,
            members: 0,
            ignore_unknown: field(options, "unknown_fields").and_then(enum_name) == Some("Ignore"),
            unknown_explicit: field(options, "unknown_fields").is_some(),
            exact: field(options, "number_mode").and_then(enum_name) == Some("Exact"),
            frozen: false,
            hook_instructions: 5_000_000,
            hook_time: Duration::from_secs(5),
            deadline: None,
        })
    }

    pub fn visit(&mut self, depth: usize) -> Result<()>
    {
        if self
            .deadline
            .is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            return Err(JsonError::new("LimitError", "JSON operation deadline expired").into());
        }
        self.members += 1;
        if depth > self.limits.depth || self.members > self.limits.members
        {
            return Err(JsonError::new("LimitError", "JSON depth/member limit exceeded").into());
        }
        Ok(())
    }
}

impl Interpreter
{
    pub(super) fn handle_json_bind(
        &mut self,
        location: &Location,
        args: &[Value],
    ) -> InterpreterResult<()>
    {
        let [Value::Type(target)] = args
        else
        {
            return Err(InterpreterError::new(
                location.clone(),
                crate::language::interpreter::ErrorWhat::ArgumentMismatch(
                    "Conversion binding requires a Type".into(),
                ),
            ));
        };
        let scope = self
            .exceptions
            .callers
            .iter()
            .rev()
            .find(|scope| **scope != self.current_scope)
            .cloned()
            .unwrap_or_else(|| self.current_scope.clone());
        let caller = &self.scopes[&scope];
        let types = caller.types.clone();
        let methods: HashMap<(TypeId, String), Option<FunctionRef>> = caller
            .conversion_snapshot()
            .into_iter()
            .filter_map(|(key, function)| {
                let mut parts = key.split(':');
                if parts.next()? != "\0method"
                {
                    return None;
                }
                let id = parts.next()?.parse::<usize>().ok()?;
                Some(((TypeId(id), parts.next()?.into()), Some(function)))
            })
            .collect();
        let mut operation = Operation::new(types.clone(), scope.clone(), &Value::None)
            .map_err(|_| unreachable!())
            .unwrap();
        operation.methods = methods.clone();
        operation.frozen = true;
        let preflight = self
            .json_codec(location, target.id, &mut operation)
            .map(|_| Value::None);
        let checked = self.json_result(location, preflight)?;
        self.consume_success(location, &checked)?;
        self.last_result = Some(Value::Native(
            crate::language::data::native_value::NativeValue::new(Conversion {
                target: target.id,
                types,
                scope,
                methods,
                codecs: operation.codecs,
            }),
        ));
        Ok(())
    }

    pub(super) fn handle_json_apply(
        &mut self,
        location: &Location,
        args: &[Value],
    ) -> InterpreterResult<()>
    {
        let [input, conversion, rest @ ..] = args
        else
        {
            return Err(InterpreterError::new(
                location.clone(),
                crate::language::interpreter::ErrorWhat::ArgumentMismatch(
                    "Conversion requires value and bound target".into(),
                ),
            ));
        };
        let Some(conversion) = (match conversion.underlying()
        {
            Value::Native(native) => native.downcast::<Conversion>(),
            _ => None,
        })
        else
        {
            return Err(InterpreterError::new(
                location.clone(),
                crate::language::interpreter::ErrorWhat::InvalidOperand(
                    "Expected bound conversion capability".into(),
                ),
            ));
        };
        let mut operation = match Operation::new(
            conversion.types.clone(),
            conversion.scope.clone(),
            rest.first().unwrap_or(&Value::None),
        )
        {
            Ok(operation) => operation,
            Err(error) =>
            {
                self.last_result = Some(self.json_result(location, Err(error))?);
                return Ok(());
            }
        };
        operation.methods = conversion.methods.clone();
        operation.codecs = conversion.codecs.clone();
        operation.deadline = self
            .exceptions
            .operation
            .as_ref()
            .map(|(_, deadline)| *deadline);
        operation.frozen = true;
        let result = self.json_convert(location, input, conversion.target, &mut operation, 0);
        self.last_result = Some(self.json_result(location, result)?);
        Ok(())
    }

    pub(super) fn handle_json(
        &mut self,
        location: &Location,
        args: &[Value],
    ) -> InterpreterResult<()>
    {
        let outcome = self.json_command(location, args);
        self.last_result = Some(self.json_result(location, outcome)?);
        Ok(())
    }

    fn json_result(
        &mut self,
        location: &Location,
        outcome: Result<Value>,
    ) -> InterpreterResult<Value>
    {
        Ok(match outcome
        {
            Ok(value) => value,
            Err(Failure::Raised(error)) => return Err(error),
            Err(Failure::Contract(error)) =>
            {
                let mut value = self.json_result(
                    location,
                    Err(Failure::Json(JsonError::new(
                        "MappingError",
                        error.what.to_string(),
                    ))),
                )?;
                let related = self.related_failure(&error, "Cause");
                if let Some(failure) = occurrence(&value)
                {
                    failure.borrow_mut().related.push(related);
                }
                self.activate_failure(location, &mut value, false)?;
                value
            }
            Err(Failure::Json(error)) =>
            {
                let id = self.scope().types.native_types[&format!("__json_{}", error.kind)].id;
                let mut value = Value::Struct(Rc::new(StructValue {
                    definition: self.scope().types.get(id),
                    constructing: false,
                    fields: vec![
                        Value::from_string(error.message),
                        Value::from_string(error.pointer),
                        Value::Integer(error.line as i64),
                        Value::Integer(error.column as i64),
                    ],
                    failure: None,
                }));
                self.activate_failure(location, &mut value, false)?;
                if let Some(failure) = occurrence(&value)
                {
                    let mut failure = failure.borrow_mut();
                    failure.native_origin = Some(error.native);
                    failure.frames.insert(
                        0,
                        crate::language::interpreter::CallFrame {
                            name: "__json".into(),
                            location: location.clone(),
                            native: true,
                            native_location: Some(Location::new(
                                file!(),
                                line!() as usize,
                                column!() as usize,
                            )),
                        },
                    );
                    failure.frames.truncate(128);
                }
                value
            }
        })
    }

    fn json_pin_snapshot(&self, operation: &mut Operation, snapshot: &Value)
    {
        let Value::HashMap(methods) = snapshot
        else
        {
            return;
        };
        for (key, value) in methods.iter()
        {
            let MapKey::String(key) = key
            else
            {
                continue;
            };
            let mut parts = key.split(':');
            if parts.next() != Some("\0method")
            {
                continue;
            }
            let Some(id) = parts.next().and_then(|id| id.parse::<usize>().ok())
            else
            {
                continue;
            };
            let Some(name) = parts.next()
            else
            {
                continue;
            };
            let function = if let Value::String(_, Executable::Function(function)) = value
            {
                Some(function.clone())
            }
            else
            {
                self.scope().lexical_function(key)
            };
            operation
                .methods
                .insert((TypeId(id), name.into()), function);
        }
        operation.frozen = true;
    }

    pub(super) fn pipeline_json_codec(
        &mut self,
        location: &Location,
        encode: bool,
        exact: bool,
        value: Value,
        snapshot: &Value,
    ) -> InterpreterResult<Value>
    {
        let mut operation = Operation::new(
            self.scope().types.clone(),
            self.current_scope.clone(),
            &Value::None,
        )
        .map_err(|_| unreachable!())
        .unwrap();
        self.json_pin_snapshot(&mut operation, snapshot);
        let outcome = if encode
        {
            self.json_wrap(location, &value, &mut operation, 0)
                .and_then(|node| {
                    encode_document(&Document::new(node), &operation.limits, false)
                        .map(Value::from_string)
                        .map_err(Failure::from)
                })
        }
        else
        {
            text(&value)
                .and_then(|text| parse_document(text, &operation.limits).map_err(Failure::from))
                .and_then(|doc| {
                    if exact
                    {
                        Ok(doc.into_value())
                    }
                    else
                    {
                        materialize(&doc.value, false).map_err(Failure::from)
                    }
                })
        };
        let result = self.json_result(location, outcome)?;
        self.consume_success(location, &result)?;
        Ok(result)
    }

    pub(super) fn pipeline_json_target(
        &mut self,
        location: &Location,
        target: TypeId,
        value: &Value,
        snapshot: &Value,
    ) -> InterpreterResult<Value>
    {
        let mut operation = Operation::new(
            self.scope().types.clone(),
            self.current_scope.clone(),
            &Value::None,
        )
        .map_err(|_| unreachable!())
        .unwrap();
        self.json_pin_snapshot(&mut operation, snapshot);
        let outcome = self.json_convert(location, value, target, &mut operation, 0);
        let result = self.json_result(location, outcome)?;
        self.consume_success(location, &result)?;
        Ok(result)
    }

    fn json_command(&mut self, location: &Location, args: &[Value]) -> Result<Value>
    {
        let Some(command) = args.first()
        else
        {
            return Err(JsonError::new("ValidationError", "Missing JSON operation").into());
        };
        let command = text(command)?;
        let args = &args[1..];
        let missing = Value::None;
        let input = args.first().unwrap_or(&missing);
        let options = match command
        {
            "parse" | "decode" | "wrap" | "encode" => args.get(1),
            "convert" => args.get(2),
            _ => None,
        }
        .unwrap_or(&missing);
        let scope = self
            .exceptions
            .callers
            .iter()
            .rev()
            .find(|scope| **scope != self.current_scope)
            .cloned()
            .unwrap_or_else(|| self.current_scope.clone());
        let types = self.scopes[&scope].types.clone();
        let mut operation = Operation::new(types, scope, options)?;
        operation.methods = self.scopes[&operation.scope]
            .conversion_snapshot()
            .into_iter()
            .filter_map(|(key, function)| {
                let mut parts = key.split(':');
                if parts.next()? != "\0method"
                {
                    return None;
                }
                let id = parts.next()?.parse::<usize>().ok()?;
                Some(((TypeId(id), parts.next()?.into()), Some(function)))
            })
            .collect();
        operation.frozen = true;
        match command
        {
            "accepts_null" =>
            {
                if let Value::Type(target) = input
                {
                    Ok(Value::Boolean(
                        operation.types.validate(target.id, &Value::None).is_ok(),
                    ))
                }
                else
                {
                    Err(JsonError::new("MappingError", "Expected target Type").into())
                }
            }
            "enum_codec" | "struct_codec" => self.json_make_codec(command, args, &operation),
            "parse" => Ok(parse_document(text(input)?, &operation.limits)?.into_value()),
            "decode" => self.json_materialized(
                &parse_document(text(input)?, &operation.limits)?.value,
                operation.exact,
            ),
            "wrap" | "encode" =>
            {
                let node = self.json_wrap(location, input, &mut operation, 0)?;
                let doc = Document::new(node);
                // Validate the complete output, including custom documents, against this budget.
                let encoded = encode_document(
                    &doc,
                    &operation.limits,
                    field(options, "pretty").is_some_and(Value::as_bool),
                )?;
                Ok(
                    if command == "wrap"
                    {
                        doc.into_value()
                    }
                    else
                    {
                        Value::from_string(encoded)
                    },
                )
            }
            "convert" =>
            {
                let Some(Value::Type(target)) = args.get(1)
                else
                {
                    return Err(JsonError::new("MappingError", "Expected target Type").into());
                };
                self.json_convert(location, input, target.id, &mut operation, 0)
            }
            "number" =>
            {
                let parsed = parse_document(text(input)?, &operation.limits)?;
                let JsonNode::Number(number) = parsed.value.as_ref()
                else
                {
                    return Err(JsonError::new("MappingError", "Expected JSON number token").into());
                };
                Ok(Value::Native(
                    crate::language::data::native_value::NativeValue::new(
                        crate::language::data::formats::json::document::ExactNumber(number.clone()),
                    ),
                ))
            }
            "number_integer" | "number_float" | "number_text" =>
            {
                let number = exact_number(input)
                    .ok_or_else(|| JsonError::new("MappingError", "Expected json::Number"))?;
                Ok(match command
                {
                    "number_integer" => Value::Integer(integer(&number.0)?),
                    "number_float" => Value::Float(float(&number.0)?, None),
                    _ => Value::from_string(number.0.clone()),
                })
            }
            "count" | "member_at" =>
            {
                let doc = document(input)
                    .ok_or_else(|| JsonError::new("MappingError", "Expected Document"))?;
                match doc.value.as_ref()
                {
                    JsonNode::Array(items) if command == "count" =>
                    {
                        Ok(Value::Integer(items.len() as i64))
                    }
                    JsonNode::Object(items) if command == "count" =>
                    {
                        Ok(Value::Integer(items.len() as i64))
                    }
                    JsonNode::Object(items) =>
                    {
                        let Some(Value::Integer(index)) = args.get(1).map(Value::underlying)
                        else
                        {
                            return Err(JsonError::new(
                                "PathError",
                                "Member index must be an Integer",
                            )
                            .into());
                        };
                        let (name, value) = usize::try_from(*index)
                            .ok()
                            .and_then(|index| items.get(index))
                            .ok_or_else(|| {
                                JsonError::new("PathError", "Member index is out of range")
                            })?;
                        Ok(Value::from_array(vec![
                            Value::from_string(name.clone()),
                            doc.view(value.clone()).into_value(),
                        ]))
                    }
                    _ => Err(
                        JsonError::new("MappingError", "Operation requires a JSON container")
                            .into(),
                    ),
                }
            }
            _ =>
            {
                let doc = document(input)
                    .ok_or_else(|| JsonError::new("MappingError", "Expected json::Document"))?;
                let pointer = args
                    .get(1)
                    .filter(|value| !matches!(value, Value::None))
                    .map(text)
                    .transpose()?
                    .unwrap_or("");
                if matches!(command, "set" | "remove" | "append")
                {
                    let replacement = if command == "remove"
                    {
                        None
                    }
                    else
                    {
                        Some(self.json_wrap(
                            location,
                            args.get(2).unwrap_or(&missing),
                            &mut operation,
                            0,
                        )?)
                    };
                    let node = edit(
                        &doc.value,
                        &pointer_parts(pointer)?,
                        replacement,
                        command == "append",
                    )?;
                    let changed = Document::new(node);
                    encode_document(&changed, &operation.limits, false)?;
                    return Ok(changed.into_value());
                }
                if command == "detach"
                {
                    return Ok(doc.detach().into_value());
                }
                let found = find(doc, pointer)?;
                if command == "find"
                {
                    return Ok(found.map_or(Value::None, Document::into_value));
                }
                if command == "has"
                {
                    return Ok(Value::Boolean(found.is_some()));
                }
                let found = found.ok_or_else(|| {
                    JsonError::new("PathError", "JSON location is missing").at(pointer.into())
                })?;
                let node = found.value.as_ref();
                Ok(match command
                {
                    "at" => found.into_value(),
                    "kind" => Value::from_string(found.kind().into()),
                    "value" => self.json_materialized(node, true)?,
                    "require_string" =>
                    {
                        if let JsonNode::String(value) = node
                        {
                            Value::from_string(value.clone())
                        }
                        else
                        {
                            return Err(JsonError::new("MappingError", "Expected JSON String")
                                .at(pointer.into())
                                .into());
                        }
                    }
                    "require_boolean" =>
                    {
                        if let JsonNode::Boolean(value) = node
                        {
                            Value::Boolean(*value)
                        }
                        else
                        {
                            return Err(JsonError::new("MappingError", "Expected JSON Boolean")
                                .at(pointer.into())
                                .into());
                        }
                    }
                    "require_integer" =>
                    {
                        if let JsonNode::Number(value) = node
                        {
                            Value::Integer(integer(value)?)
                        }
                        else
                        {
                            return Err(JsonError::new("MappingError", "Expected JSON Number")
                                .at(pointer.into())
                                .into());
                        }
                    }
                    "require_float" =>
                    {
                        if let JsonNode::Number(value) = node
                        {
                            Value::Float(float(value)?, None)
                        }
                        else
                        {
                            return Err(JsonError::new("MappingError", "Expected JSON Number")
                                .at(pointer.into())
                                .into());
                        }
                    }
                    "document_number_text" =>
                    {
                        if let JsonNode::Number(value) = node
                        {
                            Value::from_string(value.clone())
                        }
                        else
                        {
                            return Err(
                                JsonError::new("MappingError", "Expected JSON Number").into()
                            );
                        }
                    }
                    "array_items" =>
                    {
                        if let JsonNode::Array(items) = node
                        {
                            Value::from_array(
                                items
                                    .iter()
                                    .map(|node| found.view(node.clone()).into_value())
                                    .collect(),
                            )
                        }
                        else
                        {
                            return Err(JsonError::new("MappingError", "Expected JSON Array")
                                .at(pointer.into())
                                .into());
                        }
                    }
                    "members" =>
                    {
                        if let JsonNode::Object(items) = node
                        {
                            Value::from_hash_map(
                                items
                                    .iter()
                                    .map(|(key, node)| {
                                        (
                                            MapKey::String(key.clone()),
                                            found.view(node.clone()).into_value(),
                                        )
                                    })
                                    .collect(),
                            )
                        }
                        else
                        {
                            return Err(
                                JsonError::new("MappingError", "Expected JSON Object").into()
                            );
                        }
                    }
                    _ =>
                    {
                        return Err(
                            JsonError::new("ValidationError", "Unknown JSON operation").into()
                        );
                    }
                })
            }
        }
    }

    fn json_materialized(&self, node: &JsonNode, exact: bool) -> Result<Value>
    {
        let mut value = materialize(node, exact)?;
        let number = self.scope().types.names.get("Number").copied();
        fn attach(value: &mut Value, types: &TypeRegistry, number: Option<TypeId>)
        {
            match value
            {
                Value::Native(_) if exact_number(value).is_some() =>
                {
                    if let Some(id) = number
                    {
                        *value = types.coerce(id, value.clone()).unwrap();
                    }
                }
                Value::Array(items) =>
                {
                    for value in Rc::make_mut(items)
                    {
                        attach(value, types, number);
                    }
                }
                Value::HashMap(items) =>
                {
                    for value in Rc::make_mut(items).values_mut()
                    {
                        attach(value, types, number);
                    }
                }
                _ =>
                {}
            }
        }
        attach(&mut value, &self.scope().types, number);
        Ok(value)
    }
}

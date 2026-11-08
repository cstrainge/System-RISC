
use std::{ rc::Rc, collections::hash_map::IntoIter };

use crate::language::{ data::{ map_key::MapKey, range::RangeIterator, value::Value,
                               collection::Collection,
                               scoped_variables::ValueReference,
                               methods::{ BoundMethod, MethodDefinition }, types::{ TypeId, TypeKind, TypeRegistry } },
                       interpreter::{ Interpreter, InterpreterResult, InterpreterError, ErrorWhat },
                       text::location::Location };


#[track_caller]
fn iteration_error(location: &Location, message: impl Into<String>) -> InterpreterError
{
    InterpreterError::new(location.clone(), ErrorWhat::IterationError(message.into()))
}


// Native cursors implement the same next_item contract without consuming a Vec
// from the front on every step. User overrides always use normal method dispatch.
enum NativeIteration
{
    Array { values: Rc<Collection<Vec<Value>>>, index: usize },
    Map { values: IntoIter<MapKey, Value>, key_type: TypeId },
    Range(RangeIterator),
}

impl NativeIteration
{
    fn new(value: Value, registry: &TypeRegistry, location: &Location) -> InterpreterResult<Self>
    {
        Ok(match value
        {
            Value::Array(values) | Value::ArgumentExpansion(values) =>
                Self::Array { values, index: 0 },
            Value::HashMap(values) =>
                {
                    let id = registry.inferred_value_type(&Value::HashMap(values.clone()));
                    let TypeKind::Map(key_type, _) = registry.get(id).kind else { unreachable!(); };
                    Self::Map { values: Rc::unwrap_or_clone(values).into_iter(), key_type }
                },
            Value::Range(range) =>
                {
                    if range.start.is_none() || range.end.is_none()
                    { return Err(iteration_error(location,
                        "For loops require a range with both bounds")); }
                    Self::Range(range.iter().unwrap())
                },
            _ => return Err(iteration_error(location, "Invalid native iterator receiver")),
        })
    }

    fn next(&mut self, registry: &TypeRegistry) -> Result<Option<Value>, String>
    {
        Ok(match self
        {
            Self::Array { values, index } =>
                {
                    let value = values.get(*index).cloned();
                    *index += 1;
                    value
                },
            Self::Map { values, key_type } => match values.next()
                {
                    Some((key, value)) => Some(Value::from_array(vec![
                            registry.key_value(*key_type, &key)?, value,
                        ])),
                    None => None,
                },
            Self::Range(values) => values.next().map(Value::Integer),
        })
    }
}


pub(super) struct Iteration
{
    method: BoundMethod,
    native: Option<NativeIteration>,
    ended: bool,
    lease: Option<super::streams::IterationLease>,
}

impl Iteration
{
    pub fn new(interpreter: &Interpreter, location: &Location, value: Value, snapshot: &Value)
        -> InterpreterResult<Self>
    {
        let name = value.type_name();
        Self::try_new(interpreter, location, value, snapshot)?.ok_or_else(||
            iteration_error(location, format!("Type '{}' has no next_item method", name)))
    }

    pub fn try_new(
        interpreter: &Interpreter,
        location: &Location,
        value: Value,
        snapshot: &Value,
    ) -> InterpreterResult<Option<Self>>
    {
        let reference = ValueReference::temporary(value.clone());
        let Some(method) = interpreter.resolve_method(&value, reference, "next_item", snapshot)
            else { return Ok(None); };
        let (return_type, native) = match &method.definition
            {
                MethodDefinition::Builtin(definition) =>
                    (Some(definition.return_type), definition.native_iterator),
                MethodDefinition::User(function) =>
                    {
                        if function.variadic || function.arguments.len() != 1
                        { return Err(iteration_error(location,
                            "next_item must accept no arguments beyond its receiver")); }
                        (function.return_type, false)
                    },
            };
        // Native collections can contain acknowledged errors as ordinary items.
        // Their precise optional item type must not be reduced to the success path.
        let returns_native_items = native && return_type.is_some_and(|id|
            matches!(interpreter.scope().types.get(id).kind, TypeKind::Optional(_)));
        if    !returns_native_items
           && !return_type
            .and_then(|id| interpreter.scope().types.success_type(id))
            .is_some_and(
                |id| matches!(interpreter.scope().types.get(id).kind, TypeKind::Optional(_)),
            )
        { return Err(iteration_error(location, "next_item must declare a return type T | ()")); }
        let lease = super::streams::iteration_lease(&value, location)?;
        let native = if native
            { Some(NativeIteration::new(value.underlying().clone(),
                &interpreter.scope().types, location)?) } else { None };
        Ok(Some(Self {
            method,
            native,
            ended: false,
            lease,
        }))
    }

    pub fn next(
        &mut self,
        interpreter: &mut Interpreter,
        location: &Location,
    ) -> InterpreterResult<Option<Value>>
    {
        if self.ended { return Ok(None); }
        if let Some(lease) = &self.lease
        {
            lease.enter();
        }
        let value = if let Some(native) = &mut self.native
            {
                let value = native.next(&interpreter.scope().types)
                    .map_err(|message| iteration_error(location, message))?;
                if value.is_none() { self.ended = true; }
                if let Some(value) = &value { interpreter.consume_success(location, value)?; }
                return Ok(value);
            }
            else
            {
                interpreter.execute_method(location, &self.method, &[])?;
                if interpreter.halted { self.ended = true; return Ok(None); }
                interpreter.last_result.take().unwrap_or(Value::None)
            };
        interpreter.consume_success(location, &value)?;
        if matches!(value, Value::None)
        {
            self.ended = true;
            self.lease.take();
            return Ok(None);
        }
        Ok(Some(value))
    }
}

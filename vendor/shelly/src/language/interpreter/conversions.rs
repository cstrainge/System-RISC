
use crate::language::{ bytecode::FunctionRef,
                       data::{ map_key::MapKey, methods::{ method_key, conversion_key },
                               scoped_variables::ValueReference, types::{ TypeId, TypeKind },
                               value::{ Executable, Value } },
                       interpreter::{ ErrorWhat, Interpreter, InterpreterError, InterpreterResult },
                       text::location::Location };


impl Interpreter
{
    // A captured method set must not acquire definitions added by a later REPL submission.
    pub(super) fn conversion_function(&self, location: &Location, owner: TypeId,
                                     name: &str, source: Option<TypeId>, snapshot: &Value)
        -> InterpreterResult<Option<FunctionRef>>
    {
        let Value::HashMap(methods) = snapshot else { return Ok(None); };
        let plain = method_key(owner, name);
        let typed = source.map(|id| conversion_key(owner, name, id));
        let key = typed.as_ref().filter(|key| methods.contains_key(&MapKey::String((*key).clone())))
            .unwrap_or(&plain);
        match methods.get(&MapKey::String(key.clone()))
        {
            Some(Value::String(_, Executable::Function(function))) => Ok(Some(function.clone())),
            Some(Value::None) => self.scope().lexical_function(key)
                .or_else(|| self.scope().imported_methods.get(key).cloned())
                .or_else(|| self.module_method(owner, key))
                .map(Some).ok_or_else(|| InterpreterError::new(location.clone(),
                    ErrorWhat::InvalidOperand(format!(
                        "Conversion method '{}::{}' is unavailable during module initialization",
                        self.scope().types.get(owner).name, name)))),
            _ => Ok(None),
        }
    }

    pub(super) fn conversion_owners(&self, target: TypeId) -> Vec<TypeId>
    {
        match self.scope().types.get(target).kind
        {
            TypeKind::Array(_) | TypeKind::FixedArray(_) | TypeKind::Map(_, _) =>
                vec![target, self.scope().types.method_types(target)[0]],
            _ => vec![target],
        }
    }

    pub(super) fn has_captured_conversion(&self, location: &Location, target: TypeId,
                                         source: TypeId, snapshot: &Value)
        -> InterpreterResult<bool>
    {
        let name = self.scope().types.conversion_method_name(source);
        for owner in self.conversion_owners(target)
        {
            if self.conversion_function(location, owner, &name, Some(source), snapshot)?.is_some()
            { return Ok(true); }
        }
        let name = self.scope().types.output_conversion_method_name(target);
        for owner in self.scope().types.method_types(source)
        {
            if self.conversion_function(location, owner, &name, None, snapshot)?.is_some()
            { return Ok(true); }
        }
        Ok(false)
    }

    pub(super) fn convert_value(
        &mut self,
        location: &Location,
        target: TypeId,
        value: Value,
        snapshot: &Value,
        hint: &Value,
    ) -> InterpreterResult<()>
    {
        let declared = match hint
        {
            Value::Integer(id) => Some(TypeId(*id as usize)),
            Value::String(_, Executable::Function(function)) => function.return_type,
            Value::String(name, Executable::Yes) =>
                self.module_function(name).and_then(|function| function.return_type),
            Value::String(name, Executable::No) => self.variable_binding(name)
                .and_then(|binding|
                    { let binding = binding.borrow(); binding.type_id.or(binding.dispatch_type) }),
            _ => None,
        };
        let declared = declared.map(|id| match self.scope().types
            .get(self.scope().types.underlying_type(id)).kind
        {
            TypeKind::Function(_, result) => result,
            _ => id,
        });
        let source = declared.and_then(|id| self.scope().types.collection_source_type(id, &value))
            .unwrap_or_else(|| self.scope().types.inferred_value_type(&value));
        let output_source = declared.and_then(|id| self.scope().types.union_interface(id))
            .unwrap_or(source);
        let mut factory = None;
        let count = if output_source == source { 1 } else { 2 };
        for source in [output_source, source].into_iter().take(count)
        {
            let name = self.scope().types.conversion_method_name(source);
            for owner in self.conversion_owners(target)
            {
                if let Some(function) =
                    self.conversion_function(location, owner, &name, Some(source), snapshot)?
                {
                    factory = Some((owner, name.clone(), function));
                    break;
                }
            }
            if factory.is_some() { break; }
        }
        let converted = if let Some((owner, name, function)) = factory
        {
            let display = format!("{}::{}", self.scope().types.get(owner).name, name);
            self.execute_function(location, &display, &function, &[value])?;
            if self.halted { return Ok(()); }
            let result = self.last_result.take().unwrap_or(Value::None);
            if crate::language::data::failure::pending(&result).is_some()
            { self.last_result = Some(result); return Ok(()); }
            self.scope().types.coerce(target, result)
        }
        else
        {
            let name = self.scope().types.output_conversion_method_name(target);
            let mut method = None;
            let mut owners = self.scope().types.method_types(output_source);
            if output_source != source
            {
                let any = self.scope().types.builtin_id("any").unwrap();
                owners.retain(|id| *id != any);
                owners.extend(self.scope().types.method_types(source));
            }
            for owner in owners
            {
                if let Some(function) =
                    self.conversion_function(location, owner, &name, None, snapshot)?
                {
                    method = Some((owner, function));
                    break;
                }
            }
            if let Some((owner, function)) = method
            {
                let mut receiver = value;
                while let Value::Named(item) = &receiver
                {
                    if item.definition.id == owner { break; }
                    receiver = item.value.clone();
                }
                let reference = ValueReference::temporary(receiver.clone());
                let display = format!("{}::{}", self.scope().types.get(owner).name, name);
                let arguments = [receiver];
                let mut result = self.execute_function_with_receiver(location, &display, &function,
                    &arguments, Some(&reference));
                if    function.return_type == self.scope().types.builtin_id("any")
                   && self.scope().types.has_error_type(target)
                   && self.scope().types.success_type(target).is_none()
                { self.conversion_causes(location, &arguments, &mut result); }
                result?;
                if self.halted { return Ok(()); }
                let result = self.last_result.take().unwrap_or(Value::None);
                if crate::language::data::failure::pending(&result).is_some()
                { self.last_result = Some(result); return Ok(()); }
                self.scope().types.coerce(target, result)
            }
            else if matches!(self.scope().types.get(target).kind,
                TypeKind::Array(_) | TypeKind::FixedArray(_) | TypeKind::Map(_, _))
            {
                let base = self.scope().types.method_types(target)[0];
                self.scope().types.convert(base, &value)
                    .and_then(|value| self.scope().types.coerce(target, value))
            }
            else { self.scope().types.convert(target, &value) }
        };
        self.last_result = Some(converted.map_err(|message| InterpreterError::new(
            location.clone(),
            ErrorWhat::InvalidOperand(format!("Type conversion error: {}", message))
        ))?);
        Ok(())
    }
}

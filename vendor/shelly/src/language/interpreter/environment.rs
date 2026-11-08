
use std::{ cell::Cell, collections::HashMap, mem::replace, rc::Rc };

use crate::language::{ data::{ map_key::MapKey,
                               scoped_variables::{ EnvironmentConversion, ValueVisibility },
                               types::{ EnumValue, StructValue, TypeId, TypeKind },
                               value::{ Executable, Value } },
                       interpreter::{ ErrorWhat, Interpreter, InterpreterError, InterpreterResult },
                       text::location::Location };


impl Interpreter
{
    fn variable_information(&self, location: &Location) -> InterpreterResult<Value>
    {
        let registry = &self.scope().types;
        let info = registry.get(registry.native_types["VariableInfo"].id);
        let visibility = registry.get(registry.native_types["VariableVisibility"].id);
        let bindings = self.scope().variables.get_all_flattened();
        let mut variables = HashMap::new();
        for name in self.variable_names()
        {
            let exported = bindings.get(&name)
                .is_some_and(|binding| binding.exported == ValueVisibility::Exported);
            let value = self.read_raw_variable(&name, location)?;
            let name = name.strip_prefix('$').unwrap_or(&name).to_string();
            let entry = Value::Struct(Rc::new(StructValue
                {
                    definition: info.clone(),
                    fields: vec![
                            Value::from_string(name.clone()), value,
                            Value::Enum(Rc::new(EnumValue
                                {
                                    definition: visibility.clone(),
                                    variant: usize::from(exported),
                                })),
                        ],
                    constructing: false,
                    failure: None,
                }));
            variables.insert(name, entry);
        }
        Ok(Value::from_hash_map(variables.into_iter()
            .map(|(name, entry)| (MapKey::String(name), entry)).collect()))
    }

    pub(super) fn handle_get_env(&mut self, location: &Location, args: &[Value])
        -> InterpreterResult<()>
    {
        if !args.is_empty()
        {
            return Err(InterpreterError::new(location.clone(), ErrorWhat::ArgumentMismatch(
                "__get_env expects no arguments".into())));
        }
        // The prelude is a separate module. Its bridge must inspect the calling scope.
        let caller = self.exceptions.callers.last().cloned()
            .unwrap_or_else(|| self.current_scope.clone());
        let home = replace(&mut self.current_scope, caller);
        let result = self.variable_information(location);
        self.current_scope = home;
        self.last_result = Some(result?);
        Ok(())
    }

    fn environment_converter_required(&self, source: TypeId) -> bool
    {
        let definition = self.scope().types.get(source);
        !matches!(definition.kind, TypeKind::Builtin)
            || !matches!(definition.name.as_str(),
                "any" | "String" | "Integer" | "Float" | "Number" | "Boolean"
                | "ExecResult" | "None")
    }

    pub(super) fn retype_variable(&mut self, location: &Location, name: &str,
                                 target: TypeId, methods: &Value) -> InterpreterResult<()>
    {
        let invalid = |message| InterpreterError::new(location.clone(),
            ErrorWhat::InvalidOperand(message));
        let binding = self.variable_binding(name).ok_or_else(||
            invalid(format!("Cannot retype undefined variable '{}'", name)))?;
        if binding.borrow().reference.is_some()
        { return Err(invalid("Cannot retype a projected receiver binding".into())); }
        let current = self.read_raw_variable(name, location)?;
        let source = binding.borrow().type_id
            .and_then(|id| self.scope().types.collection_source_type(id, &current))
            .unwrap_or_else(|| self.scope().types.inferred_value_type(&current));
        let original = binding.borrow().environment_conversion.as_ref()
            .map(|conversion| conversion.original_type).unwrap_or(source);
        // Resolve forward declarations now, while their defining lexical scope is available.
        let mut methods = methods.clone();
        if let Value::HashMap(values) = &mut methods
        {
            let functions = self.scope().conversion_snapshot();
            for (key, value) in Rc::make_mut(values).iter_mut()
            {
                if matches!(value, Value::None)
                    && let MapKey::String(key) = key
                    && let Some(function) = functions.get(key)
                { *value = Value::String(key.clone(), Executable::Function(function.clone())); }
            }
        }
        // A collection's display text is not a reversible environment encoding.
        if    !self.has_captured_conversion(location, original, target, &methods)?
           && (self.environment_converter_required(target)
                || !matches!(self.scope().types.get(original).kind, TypeKind::Builtin))
        {
            return Err(invalid(format!("No reverse conversion from {} to {} for '{}'",
                self.scope().types.get(target).name, self.scope().types.get(original).name, name)));
        }
        let previous_result = self.last_result.take();
        let converted = self.convert_value(location, target, current, &methods,
            &Value::Integer(source.0 as i64));
        let value = self.last_result.take().unwrap_or(Value::None);
        self.last_result = previous_result;
        converted?;
        self.consume_success(location, &value)?;
        if self.halted { return Ok(()); }
        let mut binding = binding.borrow_mut();
        binding.value = value;
        binding.type_id = Some(target);
        binding.environment_conversion = Some(Rc::new(EnvironmentConversion
            {
                original_type: original,
                value_type: target,
                methods,
                active: Cell::new(false),
            }));
        Ok(())
    }

    pub(super) fn environment_value(&mut self, location: &Location, name: &str)
        -> InterpreterResult<Value>
    {
        let conversion = self.variable_binding(name)
            .and_then(|binding| binding.borrow().environment_conversion.clone());
        let value = self.read_raw_variable(name, location)?;
        let Some(conversion) = conversion else { return Ok(value); };
        let source = self.scope().types.collection_source_type(conversion.value_type, &value)
            .unwrap_or_else(|| self.scope().types.inferred_value_type(&value));
        if    self.environment_converter_required(source)
           && !self.has_captured_conversion(location, conversion.original_type, source,
                &conversion.methods)?
        {
            return Err(InterpreterError::new(location.clone(), ErrorWhat::InvalidOperand(
                format!("No reverse conversion from {} to {} for '{}'",
                    self.scope().types.get(source).name,
                    self.scope().types.get(conversion.original_type).name, name))));
        }
        if conversion.active.replace(true)
        {
            return Err(InterpreterError::new(location.clone(), ErrorWhat::InvalidOperand(
                format!("Recursive environment conversion for '{}'", name))));
        }
        let previous_result = self.last_result.take();
        let result = self.convert_value(location, conversion.original_type, value,
            &conversion.methods, &Value::Integer(conversion.value_type.0 as i64));
        let value = self.last_result.take().unwrap_or(Value::None);
        self.last_result = previous_result;
        conversion.active.set(false);
        result.map_err(|error|
            {
                let context = InterpreterError::new(location.clone(), ErrorWhat::InvalidOperand(
                    format!("Environment conversion for '{}' from {} to {} failed: {}", name,
                        self.scope().types.get(conversion.value_type).name,
                        self.scope().types.get(conversion.original_type).name, error.what)));
                error.with_context(context)
            })?;
        self.consume_success(location, &value)?;
        Ok(value)
    }

    // Optional lookup and child inheritance share the binding's retained conversion.
    fn exported_variable_text(&mut self, key: &str, location: &Location)
        -> InterpreterResult<String>
    {
        let text = self.environment_value(location, key)?.as_text();
        Ok(if key == "$PATH"
            {
                self.eval_path_list_from(&text)
            }
            else
            {
                self.eval_path_from(&text)
            })
    }

    pub(super) fn exported_environment(&mut self, location: &Location)
        -> InterpreterResult<Vec<(String, String)>>
    {
        if let Some(environment) = &self.widget_environment { return Ok(environment.clone()); }
        let bindings = self.scope().variables.get_all_flattened();
        let mut environment = Vec::new();
        for (key, binding) in bindings
        {
            if binding.exported != ValueVisibility::Exported { continue; }
            let text = self.exported_variable_text(&key, location)?;
            if self.halted { break; }
            environment.push((key.strip_prefix('$').unwrap_or(&key).to_string(), text));
        }
        Ok(environment)
    }

    fn environment_variable(&mut self, name: &str, location: &Location) -> InterpreterResult<Value>
    {
        if let Some(environment) = &self.widget_environment
        {
            return Ok(environment.iter().find(|(key, _)| key == name)
                .map_or(Value::None, |(_, text)| Value::from_string(text.clone())));
        }
        let key = format!("${name}");
        let exported = self.scope().variables.get(&key)
            .is_some_and(|value| value.exported == ValueVisibility::Exported);
        if !exported { return Ok(Value::None); }
        self.exported_variable_text(&key, location).map(Value::from_string)
    }

    pub(super) fn handle_env(&mut self, location: &Location, args: &[Value])
        -> InterpreterResult<()>
    {
        let (name, fallback) = match args
        {
            [Value::String(name, _)] => (name, Value::None),
            [Value::String(name, _), fallback @ (Value::String(_, _) | Value::None)] =>
                (name, fallback.clone()),
            _ =>
            {
                return Err(InterpreterError::new(location.clone(), ErrorWhat::ArgumentMismatch(
                    "__env expects a String name and an optional String fallback".into())));
            }
        };
        if name.is_empty() || name.contains(['=', '\0'])
        {
            return Err(InterpreterError::new(location.clone(), ErrorWhat::InvalidOperand(
                "Environment names must be nonempty and contain neither '=' nor NUL".into())));
        }

        // This private bridge reads the caller of its Shelly wrapper, rather than
        // the standard library's separate module environment. Direct top-level
        // native calls use the current scope instead.
        let caller = self.exceptions.callers.last().cloned()
            .unwrap_or_else(|| self.current_scope.clone());
        let home = replace(&mut self.current_scope, caller);
        let result = self.environment_variable(name, location);
        self.current_scope = home;
        self.last_result = Some(match result?
            {
                Value::None => fallback,
                value => value,
            });
        Ok(())
    }
}

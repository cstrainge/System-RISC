
use std::rc::Rc;

use crate::language::{ ast::{ AstExpression, AstExpressionKind, AstStatement, AstStringFlag,
                              AstTopLevel },
                       bytecode::FunctionRef,
                       data::{ types::TypeId, value::{ Executable, Value } },
                       interpreter::{ ErrorWhat, Interpreter, InterpreterError, InterpreterResult },
                       native::NativeFunction,
                       text::location::Location };


#[track_caller]
fn visibility_error(location: &Location, message: impl Into<String>) -> InterpreterError
{
    InterpreterError::new(location.clone(), ErrorWhat::ModuleError(message.into()))
}


fn visibility_argument<'a>(location: &Location, args: &'a [Value])
    -> InterpreterResult<&'a Value>
{
    match args
    {
        [value] => Ok(value),
        _ => Err(visibility_error(location,
            "visible expects one function reference or type name; use pub visible to export")),
    }
}


// Only literal top-level declarations participate in compilation. Expressions,
// interpolation, and nested calls retain ordinary runtime evaluation and ordering.
fn literal_argument(expression: &AstExpression) -> Option<Value>
{
    if matches!(&expression.string_flag, Some(AstStringFlag::Interpolated(parts))
        if !parts.is_empty()) { return None; }
    match &expression.kind
    {
        AstExpressionKind::Symbol(symbol) => Some(Value::from_string(symbol.name.clone())),
        AstExpressionKind::Literal(literal) => Some(literal.value.clone()),
        AstExpressionKind::ExecutableReference(inner) =>
            match literal_argument(inner)?
            {
                Value::String(name, _) => Some(Value::from_executable_string(name)),
                _ => None,
            },
        _ => None,
    }
}


impl Interpreter
{
    pub(super) fn publish_native(&mut self, location: &Location, function: Rc<NativeFunction>,
                                 export: bool) -> InterpreterResult<()>
    {
        let name = function.name;
        if    self.scope().base_function(name).is_some()
           || self.scope().native_functions.get(name)
                .is_some_and(|existing| !Rc::ptr_eq(existing, &function))
        { return Err(visibility_error(location, format!("Function '{}' is already bound", name))); }
        self.scope_mut().import_native(function);
        if export { self.scope_mut().exports.insert(name.to_string()); }
        Ok(())
    }

    fn publish_function(&mut self, location: &Location, name: &str, function: FunctionRef,
                        export: bool) -> InterpreterResult<()>
    {
        let name = name.rsplit("::").next().unwrap();
        if    self.scope().native_functions.contains_key(name)
           || self.scope().base_function(name)
                .is_some_and(|existing| !Rc::ptr_eq(&existing, &function))
        { return Err(visibility_error(location, format!("Function '{}' is already bound", name))); }
        self.scope_mut().import_function(name.to_string(), function);
        if export { self.scope_mut().exports.insert(name.to_string()); }
        Ok(())
    }

    fn publish_type(&mut self, location: &Location, id: TypeId, export: bool)
        -> InterpreterResult<()>
    {
        let definition = self.scope().types.get(id);
        let name = definition.name.clone();
        if self.scope().types.names.get(&name).is_some_and(|existing| *existing != id)
        { return Err(visibility_error(location, format!("Type '{}' is already bound", name))); }
        let scope = self.scope_mut();
        scope.types.names.insert(name.clone(), id);
        scope.imported_types.insert(name.clone(), definition);
        if export { scope.exports.insert(name); }
        Ok(())
    }

    fn visible_symbol(&mut self, location: &Location, value: &Value, export: bool)
        -> InterpreterResult<()>
    {
        match value
        {
            Value::String(_, Executable::Native(function)) =>
                self.publish_native(location, function.clone(), export),
            Value::String(name, Executable::Function(function)) =>
                self.publish_function(location, name, function.clone(), export),
            Value::String(name, Executable::Yes) =>
                {
                    if let Some(function) = self.module_native_function(name)
                        .or_else(|| self.native_functions.get(name.as_str()).cloned())
                    { return self.publish_native(location, function, export); }
                    if let Some(function) = self.module_function(name)
                    { return self.publish_function(location, name, function, export); }
                    Err(visibility_error(location,
                        format!("Function '{}' is not registered or available", name)))
                },
            Value::String(name, Executable::No) =>
                {
                    let id = self.scope().types.names.get(name).copied()
                        .or_else(|| self.scope().types.native_types.get(name).map(|item| item.id))
                        .ok_or_else(|| visibility_error(location,
                            format!("Type '{}' is not registered or available", name)))?;
                    self.publish_type(location, id, export)
                },
            _ => Err(visibility_error(location,
                "visible expects a function reference or type name")),
        }
    }

    pub(super) fn handle_visible(&mut self, location: &Location, args: &[Value])
        -> InterpreterResult<()>
    {
        let value = visibility_argument(location, args)?;
        self.visible_symbol(location, value, false)?;
        self.last_result = Some(Value::None);
        Ok(())
    }

    pub(super) fn publish_symbol(&mut self, location: &Location, value: &Value)
        -> InterpreterResult<()>
    {
        self.visible_symbol(location, value, true)
    }

    pub(super) fn prepare_visibility(&mut self, statements: &AstTopLevel) -> InterpreterResult<()>
    {
        for statement in statements
        {
            let call = match statement
                {
                    AstStatement::ExecuteStatement(call) => call,
                    AstStatement::ExpressionStatement(expression) =>
                        {
                            let AstExpressionKind::Execute(call) = &expression.kind
                                else { continue; };
                            call
                        },
                    _ => continue,
                };
            if !matches!(&call.executable.kind, AstExpressionKind::Symbol(symbol)
                if symbol.name == "visible") { continue; }
            let Some(args) = call.arguments.iter().map(literal_argument)
                .collect::<Option<Vec<_>>>() else { continue; };
            let value = visibility_argument(&call.location, &args)?;
            // Scripted declarations can be forward-declared in this submission;
            // leave their publication to the runtime call after compilation.
            let known = match value
                {
                    Value::String(name, Executable::Yes) =>
                        self.native_functions.contains_key(name.as_str())
                            || self.module_native_function(name).is_some(),
                    Value::String(name, Executable::No) =>
                        self.scope().types.native_types.contains_key(name)
                            || self.scope().types.names.contains_key(name),
                    _ => false,
                };
            if known { self.visible_symbol(&call.location, value, call.public)?; }
        }
        Ok(())
    }
}

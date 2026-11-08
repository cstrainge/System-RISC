
use std::{ borrow::Cow, collections::{ HashMap, HashSet }, rc::Rc, mem::{ replace, take },
           ops::{ Deref, DerefMut } };

use crate::language::{ ast::*,
                       parser::member_word_parts,
                       compiler::{ CompileError, CompileResult, ErrorWhat },
                       bytecode::FunctionBlockRef,
                       data::{ types::{ EnumValue,
                                        EnumVariant,
                                        FieldDefinition,
                                        StructValue,
                                        ConstructorParameter,
                                        ConstructorSignature,
                                        TypeId,
                                        TypeKind,
                                        TypeRegistry },
                               value::{ Value, Executable }, scoped_variables::ScopedVariables,
                               conversion::{ conversion_type_name, is_conversion_method },
                               methods::method_key,
                               map_key::MapKey },
                       text::{ location::Location, interpolation::interpolation_references } };

type Names = HashMap<String, TypeId>;
// Flow analysis snapshots share unchanged method signatures. Declaration or
// locality changes detach the map, preserving each branch's own signature state.
type MethodResults = Rc<HashMap<String, MethodResult>>;

fn error(location: &Location, message: String) -> CompileError
{
    CompileError { location: Some(location.clone()), what: ErrorWhat::TypeError(message) }
}


// The compiler owns the staged registry and commits it only after checking,
// code generation and linking succeed. Mutate that private stage directly;
// cloning it here duplicates the caller's transaction without protecting any
// additional published state.
pub(super) fn check_staged_ast(registry: &mut TypeRegistry, ast: &mut AstTopLevel,
                              variables: &ScopedVariables, functions: &FunctionBlockRef)
    -> CompileResult<()>
{
    let names = check_scope(registry, ast, &registry.names.clone(), true)?.into_owned();
    let mut bindings: Bindings = variables.get_all_flattened().into_iter().map(|(name, value)|
        {
            let inferred = if value.reference.is_none()
                { value.dispatch_type.or_else(|| Some(registry.inferred_value_type(&value.value))) }
                else { None };
            (name, Binding { constraint: value.type_id, inferred, refined: None,
                type_value: if let Value::Type(item) = &value.value
                    { Some(item.id) } else { None } })
        }).collect();
    let mut block = Some(functions.clone());
    let mut local = true;
    while let Some(current) = block
    {
        let current = current.borrow();
        for (name, function) in &current.functions
        {
            if function.receiver_type.is_some()
            { Rc::make_mut(&mut bindings.methods).entry(name.clone())
                .or_insert(MethodResult { type_id: function.return_type, local,
                    parameters: function.argument_types.clone(), variadic: function.variadic }); }
        }
        for name in &current.declared_functions
        { Rc::make_mut(&mut bindings.methods).entry(name.clone()).or_insert(MethodResult { type_id: None, local,
            parameters: Vec::new(), variadic: false }); }
        block = current.parent.clone();
        local = false;
    }
    check_binding_scope(registry, ast, &bindings, None, None)?;
    registry.names = names;
    Ok(())
}


fn check_scope<'names>(
    registry: &mut TypeRegistry,
    ast: &mut AstTopLevel,
    parent: &'names Names,
    module_level: bool,
) -> CompileResult<Cow<'names, Names>>
{
    // A block without a local type declaration only reads its inherited names.
    // Copy the namespace once on its first local declaration, preserving shadowing.
    let mut names = Cow::Borrowed(parent);
    let mut declarations = HashMap::new();

    // Reserve every local name before resolving any field annotations.
    let mut local_ids = Vec::new();
    for statement in ast.iter()
    {
        if    !module_level
           && let Some((_, location, true)) = statement.module_declarations().into_iter()
                .find(|(_, _, public)| *public)
        { return Err(error(location, "pub is only allowed at module top level".into())); }
        let (name, location) = match statement
            {
                AstStatement::EnumDeclaration(item) => (&item.name, &item.location),
                AstStatement::StructDeclaration(item) => (&item.name, &item.location),
                AstStatement::TypeDeclaration(item) => (&item.name, &item.location),
                _ => continue
            };
        if name == "any"
        { return Err(error(location, format!("'{}' is reserved in type annotations", name))); }
        if let Some(previous) = declarations.insert(name, location)
        {
            return Err(error(
                location,
                format!("Duplicate type '{}'; first declared at {}", name, previous),
            ));
        }
        let id = registry.register(name.clone(), TypeKind::Pending, Some(location.clone()));
        names.to_mut().insert(name.clone(), id);
        local_ids.push(id);
    }
    for statement in ast.iter()
    {
        match statement
        {
            AstStatement::TypeDeclaration(declaration) =>
                {
                    let inner = resolve_type(registry, &declaration.annotation, &names,
                                             &declaration.location)?;
                    registry.finish(names[&declaration.name], TypeKind::Named(inner));
                },
            AstStatement::EnumDeclaration(declaration) =>
                {
                    let mut variants = Vec::<EnumVariant>::new();
                    for (name, location) in &declaration.variants
                    {
                        if let Some(previous) = variants.iter()
                            .find(|variant| variant.name == *name)
                        {
                            return Err(error(
                                location,
                                format!(
                                    "Duplicate variant '{}'; first declared at {}",
                                    name, previous.location
                                ),
                            ));
                        }
                        variants.push(EnumVariant
                            {
                                name: name.clone(),
                                location: location.clone(),
                            });
                    }
                    registry.finish(names[&declaration.name], TypeKind::Enum(variants));
                },
            AstStatement::StructDeclaration(declaration) =>
                {
                    let mut fields = Vec::<FieldDefinition>::new();
                    for field in &declaration.fields
                    {
                        if field.name == "type"
                        {
                            return Err(error(&field.location,
                                "The .type property is reserved".into()));
                        }
                        if let Some(previous) = fields.iter().find(|item| item.name == field.name)
                        {
                            return Err(error(
                                &field.location,
                                format!(
                                    "Duplicate field '{}'; first declared at {}",
                                    field.name, previous.location
                                ),
                            ));
                        }
                        let type_id =
                            resolve_type(registry, &field.annotation, &names, &field.location)?;
                        fields.push(FieldDefinition
                            {
                                public: field.public, name: field.name.clone(), type_id,
                                optional: matches!(registry.get(type_id).kind,
                                                   TypeKind::Optional(_)),
                                location: field.location.clone(),
                            });
                    }
                    registry.finish(names[&declaration.name], TypeKind::Struct(fields));
                    if declaration.error { registry.mark_error(names[&declaration.name]); }
                },
            _ => {}
        }
    }
    registry.check_named_cycles(&local_ids).map_err(|(id, message)|
        error(&registry.get(id).location.clone().unwrap(), message))?;
    if let Err(message) = registry.check_required_cycles(&local_ids)
    {
        let location = registry.get(local_ids[0]).location.clone().unwrap();
        return Err(error(&location, message));
    }

    // Defaults use ordinary parameter/return coercion, never another scripted conversion.
    for statement in ast.iter_mut()
    {
        let AstStatement::TypeDeclaration(declaration) = statement else { continue; };
        let target = names[&declaration.name];
        let TypeKind::Named(source) = registry.get(target).kind else { unreachable!(); };
        let name = registry.declare_implied_conversion(target, source);
        let location = declaration.location.clone();
        declaration.conversion = Some(Box::new(AstFunctionStatement
            {
                public: declaration.public,
                condition: None,
                location: location.clone(),
                name,
                receiver: Some(declaration.name.clone()),
                receiver_type: Some(target),
                parameters: vec![
                        AstParameter
                            {
                                location: location.clone(), name: "value".into(),
                                annotation: None, optional: false, variadic: false,
                                type_id: Some(source),
                            },
                    ],
                return_annotation: None,
                return_type: Some(target),
                body: vec![
                        AstStatement::ExpressionStatement(new_ast_variable(
                            location, "value".into())),
                    ],
            }));
    }

    // Resolve extension targets before checking uses, including forward declarations.
    for statement in ast.iter_mut()
    {
        if    let AstStatement::FunctionDefinition(function) = statement
           && let Some(receiver) = &function.receiver
        {
            let id = *names.get(receiver).ok_or_else(|| error(&function.location,
                format!("Unknown type '{}'", receiver)))?;
            let definition = registry.get(registry.underlying_type(id));
            let collision = function.name == "type"
                || registry.collection_member_type(id, &function.name).is_some()
                || match &definition.kind
                {
                    TypeKind::Struct(fields) => fields.iter()
                        .any(|field| field.name == function.name),
                    TypeKind::Enum(_) => function.name == "index",
                    TypeKind::Builtin if definition.name == "Type" => function.name == "name",
                    _ => false,
                };
            if collision
            {
                return Err(error(&function.location,
                    format!("Method '{}::{}' conflicts with a data member", receiver,
                        function.name)));
            }
            function.receiver_type = Some(id);
            registry.declare_extension(id, &function.name);
        }
    }

    for statement in ast.iter_mut()
    {
        if let AstStatement::FunctionDefinition(function) = statement
        { check_protocol(registry, function, &names)?; }
    }

    for statement in ast.iter()
    {
        if let AstStatement::FunctionDefinition(function) = statement
        {
            let parameters = function
                .parameters
                .iter()
                .map(|parameter| {
                    parameter
                        .annotation
                        .as_ref()
                        .map(|annotation| {
                            resolve_method_type(
                                registry,
                                annotation,
                                &names,
                                &parameter.location,
                                function
                                    .receiver_type
                                    .filter(|_| !is_conversion_method(&function.name)),
                            )
                        })
                        .transpose()
                })
                .collect::<CompileResult<Vec<_>>>()?;
            let name = function.receiver_type.map_or_else(
                || function.name.clone(),
                |id| method_key(id, &function.name),
            );
            registry.argument_signatures.insert(name, parameters);
        }
    }

    for statement in ast.iter_mut()
    {
        if let AstStatement::ExecuteStatement(call) = statement
            && call.arguments.is_empty()
            && let AstExpressionKind::Symbol(item) = &call.executable.kind
            && let Some(id) = names.get(&item.name)
        {
            *statement = AstStatement::ExpressionStatement(new_ast_literal(
                call.location.clone(), Value::Type(registry.get(*id)), None));
        }
        match statement
        {
            AstStatement::ImportStatement(import) => return Err(error(&import.location,
                "Imports are only allowed at module top level".into())),
            AstStatement::EnumDeclaration(_)
            | AstStatement::StructDeclaration(_)
            | AstStatement::TypeDeclaration(_)
            | AstStatement::NullStatement
            | AstStatement::AliasStatement(_)
            | AstStatement::BreakStatement(_)
            | AstStatement::ContinueStatement(_) => {},
            AstStatement::LetStatement(statement) =>
                {
                    if let Some(annotation) = &statement.annotation
                    {
                        let id = resolve_method_type(registry, annotation, &names,
                            &statement.location, registry.checking_receiver)?;
                        statement.type_id = Some(id);
                        if statement.default_initialize && registry.has_receiver_type(id)
                        { statement.deferred_initialize = true; }
                        else if statement.default_initialize
                        {
                            let value = match registry.default_value(id)
                            {
                                Ok(value) => value,
                                Err(_) if matches!(registry.get(registry.underlying_type(id)).kind,
                                    TypeKind::Struct(_) | TypeKind::Enum(_)) &&
                                        !registry.has_error_type(id)
                                    ||
                                        registry.get(registry.underlying_type(id)).name.starts_with("__") =>
                                    { statement.deferred_initialize = true; Value::None },
                                Err(message) => return Err(error(&statement.location, message)),
                            };
                            statement.expression =
                                new_ast_literal(statement.location.clone(), value, None);
                        }
                    }
                    check_expression(registry, &mut statement.expression, &names)?;
                    if statement.type_id.is_none()
                    {
                        statement.type_id = constant_value(registry, &statement.expression)
                            .and_then(|value| value.collection_type());
                    }
                    if    let Some(id) = statement.type_id
                       && !statement.deferred_initialize && !statement.retype
                    {
                        check_known_value(registry, id, &statement.expression)
                            .map_err(|message| error(&statement.location,
                                                     format!("Variable '{}': {}",
                                                             statement.identifier, message)))?;
                    }
                },
            AstStatement::DestructureStatement(statement) =>
                {
                    if !module_level && statement.public
                    { return Err(error(&statement.location,
                        "pub is only allowed at module top level".into())); }
                    check_expression(registry, &mut statement.expression, &names)?;
                },
            AstStatement::SetStatement(statement) =>
                {
                    for access in &mut statement.indexes
                    {
                        if matches!(access, AstAccess::Field(name) if name == "type")
                        {
                            return Err(error(&statement.location,
                                "The .type property is read-only".into()));
                        }
                        if let AstAccess::Index(index) = access
                        {
                            check_expression(registry, index, &names)?;
                        }
                    }
                    check_expression(registry, &mut statement.expression, &names)?;
                },
            AstStatement::ExpressionStatement(expression)
            | AstStatement::ThrowStatement(expression)
            | AstStatement::DiscardStatement(expression) =>
                check_expression(registry, expression, &names)?,
            AstStatement::ExecuteStatement(statement) => check_call(registry, statement, &names)?,
            AstStatement::ReturnStatement(statement) =>
                {
                    if let Some(expression) = &mut statement.expression
                    {
                        check_expression(registry, expression, &names)?;
                    }
                },
            AstStatement::TryStatement(statement) =>
                {
                    check_scope(registry, &mut statement.body.body, &names, false)?;
                    for catch in &mut statement.catches
                    {
                        catch.type_id = catch.annotation.as_ref().map(|annotation|
                            resolve_type(registry, annotation, &names,
                                &catch.location)).transpose()?;
                        if catch.type_id.is_some_and(|id| !registry.has_error_type(id))
                        { return Err(error(&catch.location,
                            "Catch type must include marked errors".into())); }
                        check_scope(registry, &mut catch.body.body, &names, false)?;
                    }
                    if let Some(finally) = &mut statement.finally
                    {
                        check_finally(&finally.body, 0)?;
                        check_scope(registry, &mut finally.body, &names, false)?;
                    }
                },
            AstStatement::FunctionDefinition(statement) =>
                {
                    check_function(registry, statement, &names)?;
                },
            AstStatement::BlockStatement(block) | AstStatement::LoopStatement(block) =>
                {
                    check_scope(registry, &mut block.body, &names, false)?;
                },
            AstStatement::ForStatement(statement) =>
                {
                    check_expression(registry, &mut statement.iterable, &names)?;
                    check_scope(registry, &mut statement.body.body, &names, false)?;
                },
            AstStatement::ConditionalLoopStatement(statement) =>
                {
                    check_expression(registry, &mut statement.condition, &names)?;
                    check_scope(registry, &mut statement.body.body, &names, false)?;
                }
        }
    }
    Ok(names)
}


fn check_function(
    registry: &mut TypeRegistry,
    function: &mut AstFunctionStatement,
    names: &Names,
) -> CompileResult<()>
{
    let mut optional_seen = false;
    for parameter in &mut function.parameters
    {
        if let Some(annotation) = &parameter.annotation
        {
            let id = resolve_method_type(registry, annotation, names, &parameter.location,
                function.receiver_type.filter(|_| !is_conversion_method(&function.name)))?;
            parameter.type_id = Some(id);
            let definition = registry.get(id);
            parameter.optional = matches!(definition.kind, TypeKind::Optional(_));
            if    parameter.variadic
               && !matches!(definition.kind, TypeKind::Array(_))
               && !(matches!(definition.kind, TypeKind::Builtin)
                    && matches!(definition.name.as_str(), "Array" | "any"))
            {
                return Err(error(
                    &parameter.location,
                    "A variadic parameter requires an array type or any"
                        .to_string(),
                ));
            }
        }
        if optional_seen && !parameter.optional && !parameter.variadic && function.name != "new"
        {
            return Err(error(
                &parameter.location,
                "Required parameters cannot follow optional parameters".to_string(),
            ));
        }
        optional_seen |= parameter.optional;
    }
    if let Some(annotation) = &function.return_annotation
    {
        function.return_type = Some(resolve_method_type(registry, annotation, names,
            &function.location,
            function.receiver_type.filter(|_| !is_conversion_method(&function.name)))?);
    }
    else if function.return_type.is_none()
    { function.return_type = registry.builtin_id("None"); }
    if function.receiver.is_some() && is_conversion_method(&function.name)
    {
        check_conversion_spelling(registry, function, names, "from_",
            function.parameters.first().and_then(|parameter| parameter.type_id))?;
        if    function.parameters.len() != 1
           || function.parameters[0].optional
           || function.parameters[0].variadic
        { return Err(error(&function.location,
            "Conversion methods must accept exactly one required source argument".into())); }
        let target = function.receiver_type.unwrap();
        if    !function.return_type.is_some_and(|id| registry.valid_conversion_result(target, id))
           && function.return_type != registry.builtin_id("any")
        { return Err(error(&function.location, format!(
            "Conversion method '{}::{}' must declare return type {} or any",
            registry.get(target).name, function.name, registry.get(target).name))); }
    }
    if    function.receiver.is_some() && function.name.starts_with("to_")
       && function.name != "to_json"
    {
        check_conversion_spelling(registry, function, names, "to_", function.return_type)?;
        if function.parameters.len() != 1
        { return Err(error(&function.location,
            "Output conversion methods must accept no arguments beyond their receiver".into())); }
        let hinted = function.return_type.into_iter()
            .chain(function.return_type.and_then(|id| registry.success_type(id)))
            .find(|id| registry.output_conversion_method_name(*id) == function.name);
        let mut targets: HashSet<_> = names.values().copied()
            .filter(|id| registry.output_conversion_method_name(*id) == function.name).collect();
        targets.extend(registry.output_conversion_target(&function.name));
        let target = if let Some(id) = hinted { id }
            else if targets.len() == 1 { *targets.iter().next().unwrap() }
            else
            {
                let message = if targets.is_empty()
                    { format!("Unknown conversion target in '{}'", function.name) }
                    else { format!("Ambiguous conversion target in '{}'; declare its return type",
                        function.name) };
                return Err(error(&function.location, message));
            };
        if    !function.return_type.is_some_and(|id| registry.valid_conversion_result(target, id))
           && function.return_type != registry.builtin_id("any")
        { return Err(error(&function.location, format!(
            "Conversion method '{}::{}' must declare return type {} or any",
            registry.get(function.receiver_type.unwrap()).name, function.name,
            registry.get(target).name))); }
    }
    if function.receiver.is_some() && function.name == "next_item"
    {
        if function.parameters.len() != 1
        { return Err(error(&function.location,
            "next_item must accept no arguments beyond its receiver".into())); }
        if !function.return_type.and_then(|id| registry.success_type(id)).is_some_and(|id|
            matches!(registry.get(id).kind, TypeKind::Optional(_)))
        { return Err(error(&function.location,
            "next_item must declare a return type T | ()".into())); }
    }
    if function.receiver.is_some() && matches!(function.name.as_str(), "to_json" | "json_codec")
    {
        let (arguments, success) = if function.name == "to_json"
        {
            (1, "__JsonDocument")
        }
        else
        {
            (0, "__JsonCodec")
        };
        let valid_result = function.return_type.is_some_and(|result| {
            registry
                .success_type(result)
                .is_none_or(|id| registry.get(registry.underlying_type(id)).name == success)
        });
        if function.parameters.len() != arguments || !valid_result
        {
            return Err(error(
                &function.location,
                format!(
                    "JSON protocol ABI 1: {} must take no arguments and return {} | marked errors",
                    function.name,
                    if arguments == 1
                    {
                        "json::Document"
                    }
                    else
                    {
                        "json::Codec"
                    }
                ),
            ));
        }
    }
    if function.receiver.is_some() && function.name == "from_json"
    {
        let source = function.parameters[0].type_id;
        if !source
            .is_some_and(|id| registry.get(registry.underlying_type(id)).name == "__JsonDocument")
            || function.return_type == registry.builtin_id("any")
        {
            return Err(error(&function.location,
            concat!("JSON protocol ABI 1: from_json requires json::Document and returns ",
                    "its target | marked errors; use json::encode/to_json for encoding").into()));
        }
    }
    let previous = registry.checking_factory;
    let previous_receiver = registry.checking_receiver;
    registry.checking_receiver = if function.receiver_type.is_some()
        { function.receiver_type.filter(|_| !is_conversion_method(&function.name)
            && function.name != "new") }
        else if function.parameters.iter().any(|parameter| parameter.name == "$self")
        { None } else { previous_receiver };
    registry.checking_factory = if function.name == "new" { function.receiver_type }
        else if function.receiver_type.is_none() { previous } else { None };
    let result = check_scope(registry, &mut function.body, names, false);
    registry.checking_factory = previous;
    registry.checking_receiver = previous_receiver;
    result?;
    Ok(())
}


// Preserve broad source parameters and format codecs while rejecting misspelled type words.
fn check_conversion_spelling(
    registry: &TypeRegistry, function: &AstFunctionStatement, names: &Names,
    prefix: &str, hint: Option<TypeId>,
) -> CompileResult<()>
{
    let compact = |name: &str| name.chars().filter(|character| *character != '_')
        .flat_map(char::to_lowercase).collect::<String>();
    let suggested = hint.map(|id| if prefix == "from_"
        { registry.conversion_method_name(id) } else { registry.output_conversion_method_name(id) })
        .filter(|name| compact(name) == compact(&function.name));
    let expected = if let Some(name) = suggested { name }
        else
        {
            let spelling = conversion_type_name(&function.name);
            let Some(suffix) = spelling.strip_prefix(prefix).filter(|name| !name.is_empty())
            else { return Err(error(&function.location, format!(
                "Conversion method names must use snake case: {}<{}_type>", prefix,
                if prefix == "from_" { "source" } else { "target" }))); };
            let type_names: HashSet<_> = names.values().map(|id|
                conversion_type_name(&registry.get(*id).name)).collect();
            let mut words = Vec::new();
            for word in suffix.split('_')
            {
                if type_names.contains(word) { words.push(word.to_string()); continue; }
                let alternatives: HashSet<_> = type_names.iter()
                    .filter(|name| compact(name) == word).cloned().collect();
                if alternatives.len() > 1
                { return Err(error(&function.location, format!(
                    "Ambiguous type word '{}' in conversion '{}'; use its snake-case name",
                    word, function.name))); }
                words.push(alternatives.into_iter().next().unwrap_or_else(|| word.to_string()));
            }
            format!("{}{}", prefix, words.join("_"))
        };
    if function.name != expected
    {
        return Err(error(&function.location, format!(
            "Conversion method '{}::{}' must use snake case; use '{}'",
            function.receiver.as_deref().unwrap(), function.name, expected)));
    }
    Ok(())
}


// Preserve ordinary shell argument splitting when a spaced empty call names
// no type. Declared types take precedence, including forward declarations.
fn split_spaced_call(expression: &mut AstExpression, names: &Names) -> Option<AstExpression>
{
    let receiver = spaced_call_receiver(expression)?;
    if    let AstExpressionKind::SpacedEmptyCall(name) = &receiver.kind
       && !names.contains_key(name)
    {
        let name = name.clone();
        let location = receiver.location.clone();
        *receiver = new_ast_literal(location.clone(), Value::None, None);
        return Some(replace(expression, new_ast_symbol(location, name, None)));
    }
    None
}


fn spaced_call_receiver(expression: &mut AstExpression) -> Option<&mut AstExpression>
{
    // NLL-safe: the SpacedEmptyCall case returns `expression` itself, so it is
    // checked before the match borrows `expression.kind` (stable Rust rejects
    // the borrow held live across the return; Polonius accepted it).
    if matches!(expression.kind, AstExpressionKind::SpacedEmptyCall(_))
    {
        return Some(expression);
    }
    match &mut expression.kind
    {
        AstExpressionKind::Field(value, _, _) | AstExpressionKind::Index(value, _)
        | AstExpressionKind::Splat(value) | AstExpressionKind::MathExpression(_, value, _)
        | AstExpressionKind::BooleanExpression(_, value, _)
        | AstExpressionKind::Range(Some(value), _, _) => spaced_call_receiver(value),
        _ => None
    }
}


fn check_call(
    registry: &mut TypeRegistry,
    call: &mut AstExecuteStatement,
    names: &Names,
) -> CompileResult<()>
{
    if let Some(none) = split_spaced_call(&mut call.executable, names)
    { call.arguments.insert(0, none); }
    for mut argument in take(&mut call.arguments)
    {
        let none = split_spaced_call(&mut argument, names);
        call.arguments.push(argument);
        if let Some(none) = none { call.arguments.push(none); }
    }
    // Command words retain their shell meaning, including names of types.
    if    !matches!(call.executable.kind, AstExpressionKind::Symbol(_))
       || type_word_receiver(&call.executable, names).is_some()
    { check_expression(registry, &mut call.executable, names)?; }
    if matches!(
        &call.executable.kind,
        AstExpressionKind::Literal(AstLiteral
        {
            value: Value::Enum(_)
        })
    )
    {
        return Err(error(
            &call.executable.location,
            "Cannot execute an enum as a command".to_string(),
        ));
    }
    if matches!(
        &call.executable.kind,
        AstExpressionKind::StructConstructor(_)
    )
    {
        return Err(error(
            &call.executable.location,
            "Cannot execute a struct as a command".to_string(),
        ));
    }
    let callable = match &call.executable.kind
    {
        AstExpressionKind::Symbol(item) => Some(item.name.clone()),
        AstExpressionKind::EnumVariant(owner, member) => Some(format!("{}::{}", owner, member)),
        _ => None,
    };
    let parameters = callable.and_then(|name| registry.argument_signatures.get(&name).cloned());
    let method_parameters: Vec<_> = if let AstExpressionKind::Field(_, name, _) =
        &call.executable.kind
    {
        registry
            .argument_signatures
            .iter()
            .filter(|(key, _)| key.starts_with("\0method:") && key.ends_with(&format!(":{}", name)))
            .map(|(_, parameters)| parameters.clone())
            .collect()
    }
    else
    {
        Vec::new()
    };
    for (index, argument) in call.arguments.iter_mut().enumerate()
    {
        let type_parameter = parameters
            .as_ref()
            .and_then(|parameters| parameters.get(index))
            .copied()
            .flatten()
            == registry.builtin_id("Type")
            || !method_parameters.is_empty()
                && method_parameters.iter().all(|parameters| {
                    parameters.get(index + 1).copied().flatten() == registry.builtin_id("Type")
                });
        if    !matches!(argument.kind, AstExpressionKind::Symbol(_))
           || type_parameter
           || type_word_receiver(argument, names).is_some()
        { check_expression(registry, argument, names)?; }
    }
    Ok(())
}


// Dots remain part of shell words until a known type establishes a value receiver.
fn type_word_receiver(expression: &AstExpression, names: &Names) -> Option<TypeId>
{
    let AstExpressionKind::Symbol(symbol) = &expression.kind else { return None; };
    let (owner, _) = member_word_parts(&symbol.name)?;
    names.get(owner).copied()
}


fn check_expression(
    registry: &mut TypeRegistry,
    expression: &mut AstExpression,
    names: &Names,
) -> CompileResult<()>
{
    if    let AstExpressionKind::TryExecute(inner) = &mut expression.kind
       && type_word_receiver(inner, names).is_some()
    {
        check_expression(registry, inner, names)?;
        let value = replace(inner,
            Box::new(new_ast_literal(inner.location.clone(), Value::None, None)));
        *expression = *value;
        return Ok(());
    }
    if let Some(id) = type_word_receiver(expression, names)
    {
        let AstExpressionKind::Symbol(symbol) = &expression.kind else { unreachable!(); };
        let mut receiver = new_ast_literal(expression.location.clone(),
            Value::Type(registry.get(id)), None);
        for name in symbol.name.split('.').skip(1)
        {
            receiver = AstExpression
                {
                    location: expression.location.clone(),
                    kind: AstExpressionKind::Field(Box::new(receiver), name.to_string(), None),
                    interpolation_members: Default::default(),
                    dispatch_type: Default::default(),
                    string_flag: None,
                };
        }
        *expression = receiver;
    }
    // Only a single bare type entry is a constructor. Grouping requests a Type value.
    let collection = match &expression.kind
        {
            AstExpressionKind::Array(values) if values.len() == 1 =>
                bare_type(&values[0], names).map(TypeKind::Array),
            AstExpressionKind::HashMap(entries) if entries.len() == 1 =>
                bare_type(&entries[0].0, names).zip(bare_type(&entries[0].1, names))
                    .map(|(key, value)| TypeKind::Map(key, value)),
            _ => None,
        };
    if let Some(kind) = collection
    {
        let id = registry.intern(kind);
        let value = registry.default_value(id)
            .map_err(|message| error(&expression.location, message))?
            .with_collection_type(id);
        expression.kind = AstExpressionKind::Literal(AstLiteral { value });
        expression.string_flag = None;
        return Ok(());
    }
    // A type name is a value in expression positions. Literal command words are
    // handled by check_call; quoted names remain ordinary strings.
    let type_name = match &expression.kind
        {
            AstExpressionKind::Symbol(item) => Some(item.name.clone()),
            AstExpressionKind::EnumVariant(owner, member) => Some(format!("{}::{}", owner, member)),
            AstExpressionKind::TryExecute(inner) =>
                if let AstExpressionKind::Symbol(item) = &inner.kind
                    { Some(item.name.clone()) } else { None },
            _ => None,
        };
    if let Some(id) = type_name.as_ref().and_then(|name| names.get(name))
    {
        expression.kind = AstExpressionKind::Literal(AstLiteral
            { value: Value::Type(registry.get(*id)) });
        expression.string_flag = None;
        return Ok(());
    }
    if let Some(argument) = split_spaced_call(expression, names)
    {
        let location = expression.location.clone();
        let executable = replace(expression, new_ast_literal(location.clone(), Value::None, None));
        expression.kind = AstExpressionKind::Execute(Box::new(AstExecuteStatement
            { public: false, location, executable, expand_path: false,
              arguments: vec![argument] }));
    }
    match &mut expression.kind
    {
        AstExpressionKind::Pipeline(pipeline) =>
            {
                if let Some((location, message)) = &pipeline.layout_error
                { return Err(error(location, message.clone())); }
                for boundary in &mut pipeline.boundaries
                {
                    boundary.type_id = boundary.annotation.as_ref()
                        .and_then(|name| names.get(name)).copied();
                }
                for stage in &mut pipeline.stages { check_expression(registry, stage, names)?; }
            },
        AstExpressionKind::AnonymousFunction(function) =>
            check_function(registry, function, names)?,
        AstExpressionKind::TypeConversion(name, value, type_id, _) =>
            {
                let id = *names.get(name).ok_or_else(|| error(&expression.location,
                    format!("Unknown type '{}'", name)))?;
                if !matches!(registry.get(id).kind,
                    TypeKind::Builtin | TypeKind::Named(_)
                    | TypeKind::Struct(_) | TypeKind::Enum(_))
                {
                    return Err(error(&expression.location,
                        format!("The shadowed type '{}' does not support positional conversion",
                                name)));
                }
                *type_id = Some(id);
                check_expression(registry, value, names)?;
                if    matches!(registry.get(id).kind, TypeKind::Struct(_) | TypeKind::Enum(_))
                   && constant_value(registry, value)
                        .is_some_and(|value| !matches!(value, Value::Named(_))
                            && !registry.has_conversion(id, registry.inferred_value_type(&value)))
                {
                    let message = if TypeRegistry::is_builtin_name(name)
                        { format!("The shadowed type '{}' does not support positional conversion",
                            name) }
                        else { format!("Type '{}' has no positional constructor", name) };
                    return Err(error(&expression.location, message));
                }
            },
        AstExpressionKind::SpacedEmptyCall(name) =>
            {
                expression.kind =
                    AstExpressionKind::StructConstructor(Box::new(AstStructConstructor
                        {
                            raw: false, name: name.clone(),
                            fields: Vec::new(),
                            type_id: None,
                            field_indexes: Vec::new(),
                        }));
                check_expression(registry, expression, names)?;
            },
        AstExpressionKind::StructConstructor(constructor) =>
            {
                let id = *names.get(&constructor.name).ok_or_else(|| error(&expression.location,
                    format!("Unknown type '{}'", constructor.name)))?;
                let definition = registry.get(id);
                if matches!(definition.kind, TypeKind::Builtin)
                {
                    return Err(error(&expression.location,
                        format!("{} conversion requires exactly one positional value",
                                definition.name)));
                }
                constructor.raw = registry.checking_factory == Some(id);
                if matches!(definition.kind, TypeKind::Enum(_))
                {
                    if !constructor.fields.is_empty() || !registry.constructors.contains_key(&id)
                    { return Err(error(&expression.location,
                        "Enum construction requires a zero-argument ::new factory".into())); }
                    if constructor.raw
                    { return Err(error(&expression.location,
                        "Use an explicit enum variant inside its ::new factory".into())); }
                    if !registry.constructor_permissions.contains(&id)
                    { return Err(error(&expression.location,
                        "Constructor is private to its module".into())); }
                    constructor.type_id = Some(id);
                    return Ok(());
                }
                if !constructor.raw && registry.constructors.contains_key(&id)
                {
                    if !registry.constructor_permissions.contains(&id)
                    { return Err(error(&expression.location,
                        "Constructor is private to its module".into())); }
                    let signature = registry.constructors[&id].clone();
                    let mut supplied = HashSet::new();
                    constructor.field_indexes.clear();
                    for (name, location, value) in &mut constructor.fields
                    {
                        let index = signature.parameters.iter().position(|parameter|
                            parameter.name == *name).ok_or_else(|| error(location,
                                format!("Unknown constructor parameter '{}.{}'",
                                    constructor.name, name)))?;
                        if !supplied.insert(index)
                        { return Err(error(location,
                            format!("Duplicate constructor parameter '{}'", name))); }
                        check_expression(registry, value, names)?;
                        check_known_value(registry, signature.parameters[index].type_id, value)
                            .map_err(|message| error(location, message))?;
                        constructor.field_indexes.push(index);
                    }
                    for (index, parameter) in signature.parameters.iter().enumerate()
                    {
                        if !parameter.optional && !supplied.contains(&index)
                        { return Err(error(&expression.location,
                            format!("Missing constructor parameter '{}'", parameter.name))); }
                    }
                    constructor.type_id = Some(id);
                    return Ok(());
                }
                let TypeKind::Struct(fields) = &definition.kind
                else { return Err(error(&expression.location,
                    format!("Type '{}' is not a struct", constructor.name))); };
                let mut supplied = HashSet::new();
                for (name, location, value) in &mut constructor.fields
                {
                    let index = fields.iter().position(|field| field.name == *name)
                        .ok_or_else(|| error(location,
                        format!("Unknown field '{}.{}'", constructor.name, name)))?;
                    if !fields[index].public && !constructor.raw
                    { return Err(error(location, format!("Field '{}.{}' is private",
                        constructor.name, name))); }
                    if !supplied.insert(index)
                    {
                        return Err(error(
                            location,
                            format!("Duplicate constructor field '{}'", name),
                        ));
                    }
                    check_expression(registry, value, names)?;
                    check_known_value(registry, fields[index].type_id, value).map_err(|message|
                        error(location,
                              format!("Field '{}.{}' (declared at {}): {}", constructor.name, name,
                                      fields[index].location, message)))?;
                    constructor.field_indexes.push(index);
                }
                for (index, field) in fields.iter().enumerate()
                {
                    if !supplied.contains(&index) && !constructor.raw
                        && !field.optional
                        && (field.public || registry.validate(field.type_id, &Value::None).is_err())
                    {
                        return Err(error(
                            &expression.location,
                            format!(
                                "Missing required field '{}.{}'",
                                constructor.name, field.name
                            ),
                        ));
                    }
                }
                constructor.type_id = Some(id);
            },
        AstExpressionKind::Field(object, name, index) =>
            {
                check_expression(registry, object, names)?;
                if name == "type" || name.starts_with('$') || name.starts_with("::")
                { return Ok(()); }
                if let Some(id) = known_type(registry, object)
                {
                    if registry.properties.contains_key(&(id, name.clone()))
                        || registry.method(id, name).is_some() { return Ok(()); }
                    let definition = registry.get(registry.underlying_type(id));
                    if registry.collection_member_type(id, name).is_some() { return Ok(()); }
                    if matches!(definition.kind, TypeKind::Enum(_)) && name == "index"
                    { return Ok(()); }
                    if definition.name == "Type" && name == "name" { return Ok(()); }
                    if    registry.has_extension(id, name)
                       && !matches!(&definition.kind, TypeKind::Struct(fields)
                            if fields.iter().any(|field| field.name == *name))
                    { return Ok(()); }
                    if    matches!(definition.kind, TypeKind::Optional(_) | TypeKind::Union(_))
                       || matches!(definition.kind, TypeKind::Builtin)
                       && definition.name == "any"
                    { return Ok(()); }
                    let TypeKind::Struct(fields) = &definition.kind
                    else
                    {
                        return Err(error(
                            &expression.location,
                            format!("Cannot access field '{}' on {}", name, definition.name),
                        ));
                    };
                    *index =
                        Some(fields.iter().position(|field| field.name == *name)
                            .ok_or_else(|| error(&expression.location,
                        format!("Unknown field '{}.{}'", definition.name, name)))?);
                }
            },
        AstExpressionKind::EnumVariant(name, variant) =>
            {
                let qualified = format!("{}::{}", name, variant);
                if variant == "new" && let Some(id) = names.get(name)
                    && registry.constructors.contains_key(id)
                    && !registry.constructor_permissions.contains(id)
                { return Err(error(&expression.location,
                    "Constructor is private to its module".into())); }
                if    let Some(id) = names.get(name)
                   && (registry.has_callable_conversion(*id, variant)
                        || variant == "new" && registry.constructors.contains_key(id))
                {
                    expression.kind = AstExpressionKind::Symbol(AstSymbol
                        { name: method_key(*id, variant) });
                    return Ok(());
                }
                if    registry.qualified_functions.contains(&qualified)
                   || (!names.contains_key(name)
                   && registry.module_names.contains(name.split("::").next().unwrap()))
                {
                    expression.kind = AstExpressionKind::Symbol(AstSymbol { name: qualified });
                    return Ok(());
                }
                let id = names.get(name).ok_or_else(|| error(&expression.location,
                    format!("Unknown type or module member '{}'", qualified)))?;
                let definition = registry.get(registry.underlying_type(*id));
                let TypeKind::Enum(variants) = &definition.kind else
                {
                    return Err(error(
                        &expression.location,
                        format!("Type '{}' is not an enum", name),
                    ));
                };
                let index = variants.iter().position(|item| item.name == *variant)
                    .ok_or_else(|| error(&expression.location,
                        format!("Unknown variant '{}::{}' (enum declared at {})", name, variant,
                            definition.location.as_ref().unwrap())))?;
                let value = Value::Enum(Rc::new(EnumValue { definition, variant: index }));
                let value = registry.coerce(*id, value)
                    .map_err(|message| error(&expression.location, message))?;
                expression.kind = AstExpressionKind::Literal(AstLiteral { value });
            },
        AstExpressionKind::Array(values) =>
            {
                for value in values { check_expression(registry, value, names)?; }
            },
        AstExpressionKind::HashMap(entries) =>
            {
                for (key, value) in entries
                {
                    check_expression(registry, key, names)?;
                    check_expression(registry, value, names)?;
                }
            },
        AstExpressionKind::Range(start, end, _) =>
            {
                for bound in [start, end].into_iter().flatten()
                {
                    check_expression(registry, bound, names)?;
                }
            },
        AstExpressionKind::Index(left, right) | AstExpressionKind::MathExpression(_, left, right)
        | AstExpressionKind::BooleanExpression(_, left, right) =>
            {
                check_expression(registry, left, names)?;
                check_expression(registry, right, names)?;
            },
        AstExpressionKind::ExecutableReference(value) =>
            {
                check_expression(registry, value, names)?;
                if matches!(&value.kind, AstExpressionKind::Literal(literal)
                    if matches!(literal.value, Value::Enum(_)))
                {
                    return Err(error(&expression.location,
                        "Cannot execute an enum as a command".into()));
                }
            },
        AstExpressionKind::Splat(value) | AstExpressionKind::Grouped(value)
        | AstExpressionKind::TryExecute(value)
        | AstExpressionKind::MathNegate(value) | AstExpressionKind::BooleanNot(value) =>
            check_expression(registry, value, names)?,
        AstExpressionKind::Execute(call) => check_call(registry, call, names)?,
        AstExpressionKind::Redirect(source, redirects) =>
            {
                check_expression(registry, source, names)?;
                for redirect in redirects
                { check_expression(registry, &mut redirect.target, names)?; }
            },
        AstExpressionKind::IfExpression(conditional) =>
            {
                for branch in &mut conditional.branches
                {
                    check_expression(registry, &mut branch.condition, names)?;
                    check_scope(registry, &mut branch.body.body, names, false)?;
                }
                if let Some(block) = &mut conditional.else_body
                {
                    check_scope(registry, &mut block.body, names, false)?;
                }
            },
        AstExpressionKind::MatchExpression(matching) =>
            {
                check_expression(registry, &mut matching.value, names)?;
                for arm in &mut matching.arms
                {
                    if let Some(pattern) = &mut arm.pattern
                    { check_expression(registry, pattern, names)?; }
                    check_scope(registry, &mut arm.body.body, names, false)?;
                }
            },
        AstExpressionKind::Variable(_) | AstExpressionKind::VariableSplat(_)
        | AstExpressionKind::Symbol(_) | AstExpressionKind::Literal(_) => {}
    }
    Ok(())
}

fn bare_type(expression: &AstExpression, names: &Names) -> Option<TypeId>
{
    match &expression.kind
    {
        AstExpressionKind::Symbol(item) => names.get(&item.name).copied(),
        AstExpressionKind::EnumVariant(owner, member) =>
            names.get(&format!("{}::{}", owner, member)).copied(),
        AstExpressionKind::TryExecute(inner) => bare_type(inner, names),
        _ => None,
    }
}


fn resolve_type(
    registry: &mut TypeRegistry, annotation: &AstType, names: &Names, location: &Location,
) -> CompileResult<TypeId>
{
    resolve_method_type(registry, annotation, names, location, None)
}


fn resolve_method_type(
    registry: &mut TypeRegistry, annotation: &AstType, names: &Names, location: &Location,
    receiver: Option<TypeId>,
) -> CompileResult<TypeId>
{
    let kind = match annotation
        {
            AstType::Function(parameters, result) => TypeKind::Function(
                parameters.iter().map(|item|
                    resolve_method_type(registry, item, names, location, receiver))
                    .collect::<CompileResult<Vec<_>>>()?,
                resolve_method_type(registry, result, names, location, receiver)?),
            AstType::ReceiverMember(name) =>
                {
                    receiver.and_then(|id|
                        registry.collection_member_type(id, name)).ok_or_else(||
                            error(location, format!(
                                "$self.{} requires a compatible collection method receiver", name)))?;
                    TypeKind::ReceiverMember(if name == "key_type"
                        { "key_type" } else { "value_type" })
                },
            AstType::Named(name) =>
                return names.get(name).copied()
                    .ok_or_else(|| error(location, format!("Unknown type '{}'", name))),
            AstType::Array(inner) =>
                TypeKind::Array(resolve_method_type(registry, inner, names, location, receiver)?),
            AstType::FixedArray(items) => TypeKind::FixedArray(items.iter()
                .map(|item| resolve_method_type(registry, item, names, location, receiver))
                .collect::<CompileResult<Vec<_>>>()?),
            AstType::Union(members) =>
                {
                    let members = members.iter()
                        .map(|item| resolve_method_type(registry, item, names, location, receiver))
                        .collect::<CompileResult<Vec<_>>>()?;
                    return Ok(registry.union_type(members));
                },
            AstType::Map(key, value) => TypeKind::Map(
                resolve_method_type(registry, key, names, location, receiver)?,
                resolve_method_type(registry, value, names, location, receiver)?,
            )
        };
    Ok(registry.intern(kind))
}

fn anonymous_type(registry: &TypeRegistry, function: &AstFunctionStatement) -> Option<TypeId>
{
    if function.parameters.iter().any(|item| item.optional || item.variadic) { return None; }
    let any = registry.builtin_id("any").unwrap();
    Some(registry.intern(TypeKind::Function(function.parameters.iter()
        .map(|item| item.type_id.unwrap_or(any)).collect(), function.return_type.unwrap_or(any))))
}


fn known_type(registry: &TypeRegistry, expression: &AstExpression) -> Option<TypeId>
{
    match &expression.kind
    {
        AstExpressionKind::AnonymousFunction(function) => anonymous_type(registry, function),
        AstExpressionKind::Array(_) => registry.builtin_id("Array"),
        AstExpressionKind::Range(_, _, _) => registry.builtin_id("Range"),
        AstExpressionKind::HashMap(_) => registry.builtin_id("HashMap"),
        AstExpressionKind::Symbol(symbol) if symbol.is_glob() =>
            registry.builtin_id("ArgumentExpansion"),
        AstExpressionKind::StructConstructor(item) => item.type_id.map(|id|
            if item.raw { id }
            else { registry.constructors.get(&id).map_or(id, |signature| signature.result) }),
        AstExpressionKind::TypeConversion(_, _, type_id, _) => *type_id,
        AstExpressionKind::Literal(AstLiteral
        {
            value: Value::Struct(item),
        }) => Some(item.definition.id),
        AstExpressionKind::Literal(AstLiteral
        {
            value: Value::Enum(item),
        }) => Some(item.definition.id),
        AstExpressionKind::Literal(AstLiteral
        {
            value: Value::Named(item),
        }) => Some(item.definition.id),
        AstExpressionKind::Grouped(inner) => known_type(registry, inner),
        AstExpressionKind::Field(object, name, _) =>
            {
                if name == "type" { return registry.builtin_id("Type"); }
                let id = known_type(registry, object)?;
                if let Some(name) = name.strip_prefix("::")
                {
                    let definition = registry.get(registry.underlying_type(id));
                    return if let TypeKind::Struct(fields) = &definition.kind
                        { fields.iter().find(|field| field.name == name)
                            .map(|field| field.type_id) }
                        else { None };
                }
                if let Some(ty) = registry.properties.get(&(id, name.clone()))
                { return Some(*ty); }
                if registry.collection_member_type(id, name).is_some()
                { return registry.builtin_id("Type"); }
                let definition = registry.get(registry.underlying_type(id));
                if matches!(definition.kind, TypeKind::Enum(_)) && name == "index"
                { return registry.builtin_id("Integer"); }
                if definition.name == "Type" && name == "name"
                { return registry.builtin_id("String"); }
                if let TypeKind::Struct(fields) = &definition.kind
                {
                    if let Some(field) = fields.iter().find(|field| field.name == *name)
                    { return Some(field.type_id); }
                }
                // User definitions are versioned and scoped. Their runtime signature
                // validates the result; a builtin signature cannot describe an override.
                if registry.has_extension(id, name) { return None; }
                registry.method(id, name).map(|method| method.return_type)
            },
        AstExpressionKind::Index(object, index) =>
            {
                let index_type = known_type(registry, index)
                    .or_else(|| constant_value(registry, index)
                        .map(|value| registry.inferred_value_type(&value)));
                indexed_type(registry, known_type(registry, object)?, None,
                             slice_index(registry, index_type))
            },
        _ => None
    }
}

fn constant_value(registry: &TypeRegistry, expression: &AstExpression) -> Option<Value>
{
    match &expression.kind
    {
        AstExpressionKind::Field(object, name, _) if name == "index" =>
            {
                let Value::Enum(item) = constant_value(registry, object)? else { return None; };
                Some(Value::Integer(item.variant as i64))
            },
        AstExpressionKind::TypeConversion(name, value, Some(id), _)
            if !matches!(name.as_str(), "Array" | "ArgumentExpansion") =>
            {
                let value = constant_value(registry, value)?;
                if registry.has_conversion(*id, registry.inferred_value_type(&value))
                { return None; }
                registry.convert(*id, &value).ok()
            },
        AstExpressionKind::Literal(literal) =>
            {
                // Names bind to a particular callable only after function compilation.
                if matches!(literal.value, Value::String(_, Executable::Yes)) { return None; }
                if    let Value::String(text, _) = &literal.value
                   && matches!(expression.string_flag, Some(AstStringFlag::Interpolated(_)))
                   && text.contains('$')
                { return None; }
                Some(literal.value.clone())
            },
        AstExpressionKind::Grouped(inner) => constant_value(registry, inner),
        AstExpressionKind::Array(values) => Some(Value::from_array(values.iter()
            .map(|value| constant_value(registry, value)).collect::<Option<Vec<_>>>()?)),
        AstExpressionKind::HashMap(entries) =>
            {
                let mut values = HashMap::new();
                for (key, value) in entries
                {
                    values.insert(
                        MapKey::from_value(&constant_value(registry, key)?),
                        constant_value(registry, value)?,
                    );
                }
                Some(Value::from_hash_map(values))
            },
        AstExpressionKind::StructConstructor(item) =>
            {
                let id = item.type_id?;
                if registry.constructors.contains_key(&id) || item.raw { return None; }
                let definition = registry.get(id);
                let TypeKind::Struct(fields) = &definition.kind else { return None; };
                let mut values = vec![Value::None; fields.len()];
                for ((_, _, expression), index) in item.fields.iter().zip(&item.field_indexes)
                { values[*index] = constant_value(registry, expression)?; }
                Some(Value::Struct(Rc::new(StructValue
                    { definition, fields: values, constructing: false, failure: None })))
            },
        _ => None
    }
}

fn check_known_value(
    registry: &TypeRegistry,
    expected: TypeId,
    expression: &AstExpression,
) -> Result<(), String>
{
    if registry.has_receiver_type(expected) { return Ok(()); }
    if let Some(value) = constant_value(registry, expression)
    {
        if crate::language::data::failure::marked(&value) { return Ok(()); }
        return registry.coerce(expected, value).map(|_| ());
    }
    if    let Some(actual) = known_type(registry, expression)
       && matches!(registry.get(actual).kind, TypeKind::Struct(_) | TypeKind::Enum(_))
    {
        let definition = registry.get(expected);
        match definition.kind
        {
            TypeKind::Builtin if definition.name == "any" => {},
            TypeKind::Optional(inner) => return check_known_value(registry, inner, expression),
            _ if !registry.may_assign(expected, actual) =>
                return Err(format!("Expected {}, got {}", definition.name,
                                   registry.get(actual).name)),
            _ => {}
        }
    }
    Ok(())
}


// Binding constraints follow lexical declarations within a body. Named functions get
// fresh binding information because their free variables use dynamic scope at runtime.
// Closures retain capture constraints but discard mutable inferred types.
// Runtime guards remain authoritative for calls, writes, and all return paths.
#[derive(Clone, Copy)]
struct Binding
{
    constraint: Option<TypeId>,
    inferred: Option<TypeId>,
    refined: Option<TypeId>,
    type_value: Option<TypeId>,
}

impl Binding
{
    fn declared(constraint: Option<TypeId>) -> Self
    { Self { constraint, inferred: None, refined: None, type_value: None } }

    fn type_id(self) -> Option<TypeId>
    { self.refined.or(self.constraint).or(self.inferred) }
}

#[derive(Clone, Default)]
struct Bindings
{
    values: Rc<HashMap<String, Binding>>,
    // A missing return annotation is unknown, and still shadows older signatures.
    methods: MethodResults,
}

#[derive(Clone)]
struct MethodResult
{
    type_id: Option<TypeId>,
    parameters: Vec<Option<TypeId>>,
    variadic: bool,
    // Already compiled definitions in this function block precede pending replacements.
    local: bool,
}

impl MethodResult
{
    fn declared(function: &AstFunctionStatement) -> Self
    {
        Self { type_id: function.return_type, local: true,
            parameters: function.parameters.iter().map(|item| item.type_id).collect(),
            variadic: function.parameters.last().is_some_and(|item| item.variadic) }
    }
}


impl Deref for Bindings
{
    type Target = HashMap<String, Binding>;
    fn deref(&self) -> &Self::Target { &self.values }
}

impl DerefMut for Bindings
{
    fn deref_mut(&mut self) -> &mut Self::Target { Rc::make_mut(&mut self.values) }
}

impl FromIterator<(String, Binding)> for Bindings
{
    fn from_iter<T: IntoIterator<Item = (String, Binding)>>(items: T) -> Self
    { Self { values: Rc::new(items.into_iter().collect()), methods: Rc::default() } }
}

// All possible declared results must fit; overlapping unions alone are insufficient.
// `any` remains dynamic, and existing annotation coercions still apply.
fn accepts_results(registry: &TypeRegistry, expected: TypeId, actual: TypeId) -> bool
{
    let mut alternatives = Vec::new();
    runtime_alternatives(registry, actual, &mut alternatives);
    alternatives
        .into_iter()
        .all(|id| registry.get(id).error || registry.may_assign(expected, id))
}

fn method_result(registry: &TypeRegistry, receiver: TypeId, name: &str,
                 bindings: &Bindings) -> Option<TypeId>
{
    // Only the selected named interface can override concrete member signatures.
    for id in registry.method_types(receiver).into_iter()
        .filter(|id| registry.union_interface(*id).is_some())
    {
        if let Some(result) = bindings.methods.get(&method_key(id, name))
        { return result.type_id.map(|result|
            registry.specialize_receiver_type(result,
                registry.collection_member_type(receiver, "value_type"),
                registry.collection_member_type(receiver, "key_type"))); }
    }
    let mut alternatives = Vec::new();
    runtime_alternatives(registry, receiver, &mut alternatives);
    if alternatives.len() > 1
    {
        let results: Option<Vec<_>> = alternatives.into_iter()
            .map(|id| method_result(registry, id, name, bindings)).collect();
        return results.map(|items| registry.union_type(items));
    }
    for (index, id) in registry.method_types(receiver).into_iter().enumerate()
    {
        if let Some(result) = bindings.methods.get(&method_key(id, name))
        { return result.type_id.map(|result|
            registry.specialize_receiver_type(result,
                registry.collection_member_type(receiver, "value_type"),
                registry.collection_member_type(receiver, "key_type"))); }
        if let Some(method) = registry.direct_method(id, name)
        {
            // Preserve the element-dependent signature of native iterators.
            return Some(if index == 0
                { registry.direct_method(receiver, name).unwrap_or(method).return_type }
                else { method.return_type });
        }
    }
    None
}

// Calling a method returns its data once; a returned String is not another command.
fn returns_call_data(registry: &TypeRegistry, expression: &AstExpression,
                     bindings: &Bindings) -> bool
{
    match &ungroup(expression).kind
    {
        AstExpressionKind::Execute(_) => true,
        AstExpressionKind::Field(object, name, _) =>
            {
                if name == "type" { return false; }
                let Some(id) = binding_type(registry, object, bindings) else { return false; };
                if registry.collection_member_type(id, name).is_some() { return false; }
                let definition = registry.get(registry.underlying_type(id));
                if matches!(definition.kind, TypeKind::Enum(_)) && name == "index"
                { return false; }
                if definition.name == "Type" && name == "name" { return true; }
                if matches!(&definition.kind, TypeKind::Struct(fields)
                    if fields.iter().any(|field| field.name == *name)) { return false; }
                method_receiver_type(registry, object, bindings)
                    .and_then(|id| method_result(registry, id, name, bindings)).is_some()
            },
        _ => false,
    }
}

fn forget_inferred(bindings: &mut Bindings)
{
    for binding in bindings.values_mut()
    { binding.inferred = None; binding.refined = None; binding.type_value = None; }
}

fn ungroup(expression: &AstExpression) -> &AstExpression
{
    if let AstExpressionKind::Grouped(inner) = &expression.kind { ungroup(inner) }
    else { expression }
}

fn type_value(expression: &AstExpression, bindings: &Bindings) -> Option<TypeId>
{
    match &ungroup(expression).kind
    {
        AstExpressionKind::Literal(AstLiteral { value: Value::Type(item) }) => Some(item.id),
        AstExpressionKind::Variable(item) => bindings.get(&item.name)?.type_value,
        _ => None,
    }
}

// Removing alternatives keeps the named interface until one concrete type remains.
fn method_receiver_type(registry: &TypeRegistry, expression: &AstExpression,
                        bindings: &Bindings) -> Option<TypeId>
{
    let actual = binding_type(registry, expression, bindings)?;
    if let Some(interface) = registry.union_interface(actual) { return Some(interface); }
    if matches!(registry.get(actual).kind, TypeKind::Union(_) | TypeKind::Optional(_))
        && let AstExpressionKind::Variable(variable) = &ungroup(expression).kind
        && let Some(binding) = bindings.get(&variable.name)
        && let Some(interface) = binding.constraint.or(binding.inferred)
            .and_then(|id| registry.union_interface(id))
    { return Some(interface); }
    Some(actual)
}

fn refined_receiver(expression: &AstExpression, bindings: &Bindings) -> bool
{
    match &ungroup(expression).kind
    {
        AstExpressionKind::Variable(item) =>
            bindings.get(&item.name).is_some_and(|binding| binding.refined.is_some()),
        AstExpressionKind::Field(value, _, _) | AstExpressionKind::Index(value, _) =>
            refined_receiver(value, bindings),
        _ => false,
    }
}

fn type_subject(expression: &AstExpression) -> Option<&str>
{
    if let AstExpressionKind::Field(value, name, _) = &ungroup(expression).kind
        && name == "type"
        && let AstExpressionKind::Variable(item) = &value.kind
    { Some(&item.name) }
    else { None }
}

fn runtime_alternatives(registry: &TypeRegistry, id: TypeId, items: &mut Vec<TypeId>)
{
    let definition = registry.get(id);
    match &definition.kind
    {
        TypeKind::Named(inner) if registry.has_error_type(id) =>
            runtime_alternatives(registry, *inner, items),
        TypeKind::Union(members) =>
            { for member in members { runtime_alternatives(registry, *member, items); } },
        TypeKind::Optional(inner) =>
            {
                runtime_alternatives(registry, *inner, items);
                items.push(registry.builtin_id("None").unwrap());
            },
        TypeKind::Builtin if definition.name == "Number" =>
            items.extend([registry.builtin_id("Integer").unwrap(),
                          registry.builtin_id("Float").unwrap()]),
        _ => items.push(id),
    }
}

fn exact_runtime_type(registry: &TypeRegistry, id: TypeId) -> TypeId
{
    match registry.get(id).kind
    {
        TypeKind::Array(_) | TypeKind::FixedArray(_) => registry.builtin_id("Array").unwrap(),
        TypeKind::Map(_, _) => registry.builtin_id("HashMap").unwrap(),
        _ => id,
    }
}

// Type equality preserves nominal identity. Refining Array/HashMap retains any
// stronger collection constraints already known about the same runtime value.
fn refine_type(registry: &TypeRegistry, bindings: &mut Bindings,
                name: &str, target: TypeId, matched: bool)
{
    let binding = bindings.entry(name.to_string()).or_insert(Binding::declared(None));
    let mut alternatives = Vec::new();
    if let Some(current) = binding.type_id()
    { runtime_alternatives(registry, current, &mut alternatives); }
    let retained: Vec<_> = alternatives.into_iter()
        .filter(|id| (exact_runtime_type(registry, *id) == target) == matched).collect();
    if matched
    {
        binding.refined = Some(if retained.is_empty() { target }
            else { registry.union_type(retained) });
    }
    else if !retained.is_empty() { binding.refined = Some(registry.union_type(retained)); }
}

fn common_bindings(mut left: Bindings, right: &Bindings) -> Bindings
{
    for (name, binding) in Rc::make_mut(&mut left.values)
    {
        let other = right.get(name).copied().unwrap_or(Binding::declared(None));
        if binding.refined != other.refined { binding.refined = None; }
        if binding.inferred != other.inferred { binding.inferred = None; }
        if binding.type_value != other.type_value { binding.type_value = None; }
    }
    left
}

fn condition_bindings(registry: &TypeRegistry, expression: &AstExpression,
                       bindings: &Bindings, truth: bool) -> Bindings
{
    match &ungroup(expression).kind
    {
        AstExpressionKind::BooleanNot(inner) =>
            condition_bindings(registry, inner, bindings, !truth),
        AstExpressionKind::BooleanExpression(operator, left, right)
            if matches!(operator, AstBooleanOperator::And | AstBooleanOperator::Or) =>
            {
                let first = matches!(operator, AstBooleanOperator::And);
                let continuing = condition_bindings(registry, left, bindings, first);
                let continued = condition_bindings(registry, right, &continuing, truth);
                if truth == first { return continued; }
                let stopped = condition_bindings(registry, left, bindings, truth);
                common_bindings(stopped, &continued)
            },
        AstExpressionKind::BooleanExpression(operator, left, right)
            if matches!(operator, AstBooleanOperator::Equal | AstBooleanOperator::NotEqual) =>
            {
                let mut selected = bindings.clone();
                if expression_may_mutate(registry, expression, bindings)
                { forget_inferred(&mut selected); }
                let subject = type_subject(left).zip(type_value(right, &selected))
                    .or_else(|| type_subject(right).zip(type_value(left, &selected)));
                let subject = subject.or_else(||
                    {
                        let pair = [
                            (left.as_ref(), right.as_ref()), (right.as_ref(), left.as_ref()),
                        ];
                        pair.into_iter().find_map(|(value, other)|
                            if let AstExpressionKind::Variable(variable) = &ungroup(value).kind
                                && matches!(constant_value(registry, other), Some(Value::None))
                            { Some((variable.name.as_str(), registry.builtin_id("None").unwrap())) }
                            else { None })
                    });
                if let Some((name, id)) = subject
                {
                    refine_type(registry, &mut selected, name, id,
                        truth == matches!(operator, AstBooleanOperator::Equal));
                }
                selected
            },
        _ =>
            {
                let mut selected = bindings.clone();
                if expression_may_mutate(registry, expression, bindings)
                { forget_inferred(&mut selected); }
                selected
            },
    }
}


fn check_binding_scope(
    registry: &TypeRegistry,
    ast: &AstTopLevel,
    parent: &Bindings,
    return_type: Option<TypeId>,
    tail_type: Option<TypeId>,
) -> CompileResult<MethodResults>
{
    let mut bindings = parent.clone();
    // Pending local declarations shadow outer versions. Preserve already installed
    // local versions; otherwise forward references resolve to the final definition.
    let existing: HashSet<_> = bindings.methods.iter()
        .filter(|(_, result)| result.local).map(|(key, _)| key.clone()).collect();
    for statement in ast
    {
        if let AstStatement::FunctionDefinition(item) = statement
            && let Some(id) = item.receiver_type
        {
            let key = method_key(id, &item.name);
            if !existing.contains(&key)
            {
                Rc::make_mut(&mut bindings.methods).insert(key,
                    MethodResult::declared(item));
            }
        }
    }
    let last = ast.iter().rposition(|statement| !matches!(statement, AstStatement::NullStatement));
    let mut reachable = true;
    for (index, statement) in ast.iter().enumerate()
    {
        let expected = if reachable && last == Some(index) { tail_type } else { None };
        match statement
        {
            AstStatement::LetStatement(item) =>
                {
                    let receiver = bindings.get("$self").and_then(|binding| binding.type_id());
                    let value = receiver.and_then(|id|
                        registry.collection_member_type(id, "value_type"));
                    let key = receiver.and_then(|id|
                        registry.collection_member_type(id, "key_type"));
                    let type_id = item.type_id.map(|id|
                        registry.specialize_receiver_type(id, value, key));
                    if item.retype
                    {
                        // Conversion executes at runtime; subsequent uses see the new constraint.
                        forget_inferred(&mut bindings);
                        bindings.insert(item.identifier.clone(), Binding::declared(type_id));
                        continue;
                    }
                    if item.deferred_initialize
                    {
                        bindings.insert(item.identifier.clone(), Binding::declared(type_id));
                        continue;
                    }
                    bindings.methods = check_binding_expression(
                        registry,
                        &item.expression,
                        &bindings,
                        return_type,
                        type_id,
                    )?;
                    check_stored_string(registry, &item.expression, &bindings, type_id)?;
                    let inferred = binding_type(registry, &item.expression, &bindings)
                        .and_then(|id| if type_id.is_none_or(|expected|
                            !registry.has_error_type(expected))
                            && returns_call_data(registry, &item.expression, &bindings)
                            { registry.success_type(id) } else { Some(id) });
                    if expression_may_mutate(registry, &item.expression, &bindings)
                    { forget_inferred(&mut bindings); }
                    let type_value = type_value(&item.expression, &bindings);
                    bindings.insert(item.identifier.clone(), Binding
                        { constraint: type_id, inferred, refined: None,
                          type_value });
                },
            AstStatement::DestructureStatement(item) =>
                {
                    bindings.methods = check_binding_expression(
                        registry, &item.expression, &bindings, return_type, None)?;
                    let ty = binding_type(registry, &item.expression, &bindings);
                    let inferred: Vec<_> = item.bindings.iter().enumerate()
                        .map(|(index, _)| destructured_type(
                            registry, &item.expression, &bindings, ty, index)).collect();
                    if expression_may_mutate(registry, &item.expression, &bindings)
                    { forget_inferred(&mut bindings); }
                    for ((_, name), inferred) in item.bindings.iter().zip(inferred)
                    {
                        let Some(name) = name else { continue; };
                        bindings.insert(name.clone(), Binding
                            { constraint: None, inferred, refined: None, type_value: None });
                    }
                },
            AstStatement::SetStatement(item) =>
                {
                    for access in &item.indexes
                    {
                        if let AstAccess::Field(name) = access
                        { check_field_name(registry, name, &bindings, &item.location)?; }
                        if let AstAccess::Index(index) = access
                        {
                            if expression_may_mutate(registry, index, &bindings)
                            { forget_inferred(&mut bindings); }
                            bindings.methods = check_binding_expression(
                                registry, index, &bindings, return_type, None)?;
                        }
                    }
                    let constraint = if item.indexes.is_empty()
                        { bindings.get(&item.identifier).and_then(|item| item.constraint) }
                        else { None };
                    bindings.methods = check_binding_expression(
                        registry,
                        &item.expression,
                        &bindings,
                        return_type,
                        constraint,
                    )?;
                    check_stored_string(registry, &item.expression, &bindings, constraint)?;
                    let inferred = if item.indexes.is_empty()
                        { binding_type(registry, &item.expression, &bindings) } else { None };
                    let type_value = if item.indexes.is_empty()
                        { type_value(&item.expression, &bindings) } else { None };
                    // Imports can give one mutable binding multiple visible names.
                    forget_inferred(&mut bindings);
                    if let Some(binding) = bindings.get_mut(&item.identifier)
                    { binding.inferred = inferred; binding.type_value = type_value; }
                },
            AstStatement::ExpressionStatement(expression) =>
                {
                    let result = implicit_callable_result(registry, expression, &bindings);
                    bindings.methods = check_binding_expression(
                        registry, expression, &bindings, return_type,
                        if result.is_some() { None } else { expected })?;
                    if let (Some(expected), Some(actual)) = (expected, result)
                        && !accepts_results(registry, expected, actual)
                    {
                        return Err(error(&expression.location, format!("Expected {}, got {}",
                            registry.get(expected).name, registry.get(actual).name)));
                    }
                    if    expression_may_mutate(registry, expression, &bindings)
                       || matches!(expression.kind,
                            AstExpressionKind::Variable(_) | AstExpressionKind::Index(_, _))
                    { forget_inferred(&mut bindings); }
                },
            AstStatement::TryStatement(item) =>
                {
                    check_binding_scope(registry, &item.body.body, &bindings, return_type, None)?;
                    for catch in &item.catches
                    {
                        let mut handler = bindings.clone();
                        let caught = catch.type_id.map(|id| registry.catch_type(id))
                            .unwrap_or_else(|| registry.builtin_id("ErrorValue").unwrap());
                        handler.insert(catch.error.clone(), Binding::declared(Some(caught)));
                        if let Some(stack) = &catch.stack
                        { handler.insert(stack.clone(),
                            Binding::declared(Some(registry.native_types["CallStack"].id))); }
                        check_binding_scope(registry, &catch.body.body, &handler, return_type,
                            None)?;
                    }
                    if let Some(finally) = &item.finally
                    { check_binding_scope(registry, &finally.body, &bindings, return_type, None)?; }
                    forget_inferred(&mut bindings);
                },
            AstStatement::ThrowStatement(expression) =>
                {
                    bindings.methods = check_binding_expression(
                        registry, expression, &bindings, return_type, None)?;
                    reachable = false;
                },
            AstStatement::DiscardStatement(expression) =>
                {
                    bindings.methods = check_binding_expression(
                        registry, expression, &bindings, return_type, None)?;
                    if expression_may_mutate(registry, expression, &bindings)
                    { forget_inferred(&mut bindings); }
                },
            AstStatement::ExecuteStatement(call) =>
                {
                    let actual = call_result(registry, call, &bindings);
                    bindings.methods = check_binding_call(registry, call, &bindings, return_type)?;
                    if let (Some(expected), Some(actual)) = (expected, actual)
                        && !accepts_results(registry, expected, actual)
                    {
                        return Err(error(&call.location, format!("Expected {}, got {}",
                            registry.get(expected).name, registry.get(actual).name)));
                    }
                    forget_inferred(&mut bindings);
                },
            AstStatement::ReturnStatement(item) =>
                {
                    if let Some(expression) = &item.expression
                    {
                        bindings.methods = check_binding_expression(
                            registry, expression, &bindings, return_type, return_type)?;
                    }
                    else if let Some(id) = return_type
                    {
                        registry.validate(id, &Value::None).map_err(|message|
                            error(&item.location, format!("Return value: {}", message)))?;
                    }
                    reachable = false;
                },
            AstStatement::FunctionDefinition(item) =>
                {
                    if let Some(id) = item.receiver_type
                    { Rc::make_mut(&mut bindings.methods).insert(method_key(id, &item.name),
                        MethodResult::declared(item)); }
                    let value = item.receiver_type.and_then(|id|
                        registry.collection_member_type(id, "value_type"));
                    let key = item.receiver_type.and_then(|id|
                        registry.collection_member_type(id, "key_type"));
                    let specialize = |id| registry.specialize_receiver_type(id, value, key);
                    let checked_result = item.return_type.map(specialize);
                    let mut parameters: Bindings = item
                        .parameters
                        .iter()
                        .map(|parameter|
                            (parameter.name.clone(),
                             Binding::declared(parameter.type_id.map(specialize))))
                        .collect();
                    parameters.methods = bindings.methods.clone();
                    for result in Rc::make_mut(&mut parameters.methods).values_mut() { result.local = false; }
                    check_binding_scope(
                        registry,
                        &item.body,
                        &parameters,
                        checked_result,
                        checked_result,
                    )?;
                    if    item.body.iter()
                        .all(|statement| matches!(statement, AstStatement::NullStatement))
                       && let Some(id) = checked_result
                    {
                        registry.validate(id, &Value::None).map_err(|message|
                            error(&item.location,
                                  format!("Return value of '{}': {}", item.name, message)))?;
                    }
                },
            AstStatement::BlockStatement(block) =>
                {
                    bindings.methods = check_binding_scope(
                        registry, &block.body, &bindings, return_type, expected)?;
                    forget_inferred(&mut bindings);
                },
            AstStatement::LoopStatement(block) =>
                {
                    forget_inferred(&mut bindings);
                    bindings.methods = check_binding_scope(
                        registry, &block.body, &bindings, return_type, None)?;
                },
            AstStatement::ForStatement(item) =>
                {
                    bindings.methods = check_binding_expression(
                        registry, &item.iterable, &bindings, return_type, None)?;
                    let mut loop_bindings = bindings.clone();
                    // Outer inferred values may change on a previous loop iteration.
                    // The private iterator supplies fresh item bindings on every step.
                    forget_inferred(&mut loop_bindings);
                    let item_type = iterator_item_type(registry, &item.iterable, &bindings);
                    for (index, name) in item.bindings.iter().enumerate()
                    {
                        let Some(name) = name else { continue; };
                        let inferred = if item.destructure || item.bindings.len() > 1
                            {
                                item_type.filter(|id| matches!(registry
                                    .get(registry.underlying_type(*id)).kind,
                                    TypeKind::Array(_) | TypeKind::FixedArray(_)))
                                    .and_then(|id| indexed_type(
                                        registry, id, Some(index), Some(false)))
                            }
                            else { item_type };
                        loop_bindings.insert(name.clone(), Binding
                            { constraint: None, inferred, refined: None, type_value: None });
                    }
                    bindings.methods = check_binding_scope(
                        registry, &item.body.body, &loop_bindings, return_type, None)?;
                    forget_inferred(&mut bindings);
                },
            AstStatement::ConditionalLoopStatement(item) =>
                {
                    forget_inferred(&mut bindings);
                    bindings.methods = check_binding_expression(
                        registry, &item.condition, &bindings, return_type, None)?;
                    let selected = condition_bindings(registry, &item.condition,
                                                      &bindings, !item.until);
                    bindings.methods = check_binding_scope(
                        registry, &item.body.body, &selected, return_type, None)?;
                },
            _ => {}
        }
    }
    Ok(bindings.methods)
}


fn check_binding_call(registry: &TypeRegistry, call: &AstExecuteStatement, bindings: &Bindings,
                      return_type: Option<TypeId>) -> CompileResult<MethodResults>
{
    let mut current = bindings.clone();
    current.methods = check_binding_expression(
        registry, &call.executable, bindings, return_type, None)?;
    if expression_may_mutate(registry, &call.executable, &current)
    { forget_inferred(&mut current); }
    for argument in &call.arguments
    {
        current.methods = check_binding_expression(
            registry, argument, &current, return_type, None)?;
        if expression_may_mutate(registry, argument, &current)
        { forget_inferred(&mut current); }
    }
    let mut parameters = bindings.clone();
    parameters.methods = current.methods.clone();
    if call.arguments.iter().any(|argument| expression_may_mutate(registry, argument, bindings))
    { forget_inferred(&mut parameters); }
    check_receiver_arguments(registry, call, &parameters)?;
    Ok(current.methods)
}


fn check_receiver_arguments(registry: &TypeRegistry, call: &AstExecuteStatement,
                            bindings: &Bindings) -> CompileResult<()>
{
    let AstExpressionKind::Field(object, name, _) = &ungroup(&call.executable).kind
        else { return Ok(()); };
    let Some(receiver) = method_receiver_type(registry, object, bindings) else { return Ok(()); };
    for id in registry.method_types(receiver)
    {
        let Some(method) = bindings.methods.get(&method_key(id, name)) else
        {
            if registry.direct_method(id, name).is_some() { return Ok(()); }
            continue;
        };
        let value = registry.collection_member_type(receiver, "value_type");
        let key = registry.collection_member_type(receiver, "key_type");
        let fixed = method.parameters.len().saturating_sub(usize::from(method.variadic));
        for (index, argument) in call.arguments.iter().enumerate()
        {
            let position = index + 1;
            let expansion = match &argument.kind
            {
                AstExpressionKind::Splat(inner) => binding_type(registry, inner, bindings),
                AstExpressionKind::VariableSplat(variable) =>
                    bindings.get(&variable.name).and_then(|item| item.type_id()),
                _ => None,
            };
            if matches!(argument.kind, AstExpressionKind::Splat(_)
                | AstExpressionKind::VariableSplat(_))
            {
                if method.variadic && position >= fixed
                    && let Some(template) = method.parameters.get(fixed).copied().flatten()
                    && registry.has_receiver_type(template)
                    && let Some(actual) = expansion
                {
                    let expected = registry.specialize_receiver_type(template, value, key);
                    let TypeKind::Array(element) = registry.get(expected).kind
                        else { break; };
                    let items = match &registry.get(registry.underlying_type(actual)).kind
                    {
                        TypeKind::Array(item) => vec![*item],
                        TypeKind::FixedArray(items) => items.clone(),
                        _ if Some(actual) == registry.builtin_id("Range") =>
                            vec![registry.builtin_id("Integer").unwrap()],
                        _ => Vec::new(),
                    };
                    if !items.into_iter().all(|item| accepts_results(registry, element, item))
                    {
                        return Err(error(&argument.location, format!("Expected {}, got {}",
                            registry.get(expected).name, registry.get(actual).name)));
                    }
                }
                // Expansion changes subsequent argument indexes. Runtime binding checks
                // all items, including expansions before the final variadic parameter.
                break;
            }
            let template = if position < fixed { method.parameters[position] }
                else if method.variadic { method.parameters.get(fixed).copied().flatten() }
                else { None };
            let Some(template) = template.filter(|id| registry.has_receiver_type(*id))
                else { continue; };
            let mut expected = registry.specialize_receiver_type(template, value, key);
            if method.variadic && position >= fixed
                && let TypeKind::Array(item) = registry.get(expected).kind { expected = item; }
            check_known_value(registry, expected, argument)
                .map_err(|message| error(&argument.location, message))?;
            if let Some(actual) = binding_type(registry, argument, bindings)
                && !accepts_results(registry, expected, actual)
            {
                return Err(error(&argument.location, format!("Expected {}, got {}",
                    registry.get(expected).name, registry.get(actual).name)));
            }
        }
        break;
    }
    Ok(())
}


// A stored string is data even when it happens to name an executable. Only
// expression statements have the implicit execution caveat used below.
fn check_stored_string(registry: &TypeRegistry, expression: &AstExpression,
                       bindings: &Bindings, expected: Option<TypeId>) -> CompileResult<()>
{
    if expression_may_mutate(registry, expression, bindings) { return Ok(()); }
    if    let Some(expected) = expected
       && let Some(actual) = binding_type(registry, expression, bindings)
       && registry.get(actual).name == "String"
       && !registry.may_assign(expected, actual)
    {
        return Err(error(&expression.location, format!("Expected {}, got String",
            registry.get(expected).name)));
    }
    Ok(())
}


fn check_binding_expression(
    registry: &TypeRegistry, expression: &AstExpression, bindings: &Bindings,
    return_type: Option<TypeId>, expected: Option<TypeId>,
) -> CompileResult<MethodResults>
{
    if let AstExpressionKind::Field(object, _, _) = &expression.kind
    {
        let receiver = method_receiver_type(registry, object, bindings);
        expression.dispatch_type.set(receiver.and_then(|id|
            registry.union_interface(id).or_else(||
                refined_receiver(object, bindings).then_some(id))));
    }
    let members = interpolated_members(expression);
    if !members.is_empty()
    {
        let mut current = bindings.clone();
        for member in &members
        {
            current.methods = check_binding_expression(
                registry, member, &current, return_type, None)?;
            if expression_may_mutate(registry, member, &current)
            { forget_inferred(&mut current); }
        }
        *expression.interpolation_members.borrow_mut() = members;
    }
    if let AstExpressionKind::TypeConversion(_, value, target, source_type) = &expression.kind
    {
        // The source is evaluated before the factory can mutate caller bindings.
        source_type.set(declared_conversion_source(registry, value, bindings));
        expression.dispatch_type.set(target.and_then(|id| registry.union_interface(id)));
    }
    if let AstExpressionKind::Field(value, name, _) = &expression.kind
        && name != "type" && !name.starts_with('$') && !name.starts_with("::")
        && refined_receiver(value, bindings)
        && let Some(id) = binding_type(registry, value, bindings)
    {
        let definition = registry.get(registry.underlying_type(id));
        if    !matches!(definition.kind, TypeKind::Optional(_) | TypeKind::Union(_))
           && definition.name != "any"
           && !registry.properties.contains_key(&(id, name.clone()))
           && registry.method(id, name).is_none()
           && !registry.has_extension(id, name)
           && registry.collection_member_type(id, name).is_none()
           && !(matches!(definition.kind, TypeKind::Enum(_)) && name == "index")
           && !(definition.name == "Type" && name == "name")
           && !matches!(&definition.kind, TypeKind::Struct(fields)
                if fields.iter().any(|field| field.name == *name))
        {
            return Err(error(&expression.location,
                format!("Cannot access field '{}' on {}", name, definition.name)));
        }
    }
    let mut stable;
    let bindings = if expression_may_mutate(registry, expression, bindings)
        && !matches!(ungroup(expression).kind, AstExpressionKind::Field(_, _, _))
        {
            stable = bindings.clone();
            // Preserve entry facts until the expression actually evaluates a call.
            // Sequential operands and short-circuit guards handle invalidation below.
            for binding in stable.values_mut() { binding.inferred = None; }
            &stable
        } else { bindings };
    if let Some(id) = expected
    {
        check_known_value(registry, id, expression)
            .map_err(|message| error(&expression.location, message))?;
        if    let Some(actual) = binding_type(registry, expression, bindings)
            // A string in an implicit return may execute and yield another type.
           && (returns_call_data(registry, expression, bindings)
                || refined_receiver(expression, bindings)
                || registry.validate(actual, &Value::from_string(String::new())).is_err())
           && !(if returns_call_data(registry, expression, bindings)
                    || matches!(ungroup(expression).kind, AstExpressionKind::StructConstructor(_))
                    || optional_constructor_result(registry, actual)
                { accepts_results(registry, id, actual) }
                else { registry.may_assign(id, actual) })
        {
            return Err(error(&expression.location, format!("Expected {}, got {}",
                registry.get(id).name, registry.get(actual).name)));
        }
    }
    let mut methods = bindings.methods.clone();
    match &expression.kind
    {
        AstExpressionKind::Pipeline(pipeline) =>
            {
                let mut current = bindings.clone();
                for (index, stage) in pipeline.stages.iter().enumerate()
                {
                    if index > 0
                    {
                        let boundary = &pipeline.boundaries[index - 1];
                        let mut input = boundary.type_id;
                        if pipeline.parents[index - 1] == 0
                            && boundary.decode.is_none() && boundary.annotation.is_none()
                        {
                            let source = &pipeline.stages[0];
                            input = iterator_item_type(registry, source, bindings)
                                .or_else(|| binding_type(registry, source, bindings)
                                    .filter(|id| !registry.has_extension(*id, "next_item")));
                        }
                        if    boundary.decode.as_deref().is_some_and(|format|
                                matches!(format, "csv" | "tsv" | "ssv"))
                           && let Some(id) = input
                           && let TypeKind::Array(item) = registry
                                .get(registry.underlying_type(id)).kind
                           && matches!(registry.get(registry.underlying_type(item)).kind,
                                TypeKind::Struct(_))
                        { input = Some(item); }
                        if let Some(id) = input
                            && let Some(signature) = registry.constructors.get(&id)
                        { input = Some(signature.result); }
                        current.insert("$".into(), Binding::declared(input));
                    }
                    if !pipeline.parents.contains(&index)
                        && let AstExpressionKind::Variable(variable) = &stage.kind
                        && variable.name != "$"
                    {
                        if !current.contains_key(&variable.name)
                        {
                            return Err(error(&stage.location, format!(
                                "Variable '{}' not found; declare it with let", variable.name)));
                        }
                    }
                    current.methods = check_binding_expression(
                        registry, stage, &current, return_type, None)?;
                    forget_inferred(&mut current);
                }
                methods = current.methods;
            },
        AstExpressionKind::AnonymousFunction(function) =>
            {
                let mut captured = bindings.clone();
                for result in Rc::make_mut(&mut captured.methods).values_mut() { result.local = false; }
                forget_inferred(&mut captured);
                for parameter in &function.parameters
                {
                    captured.insert(parameter.name.clone(), Binding::declared(parameter.type_id));
                }
                check_binding_scope(registry, &function.body, &captured,
                                    function.return_type, function.return_type)?;
                if function.body.iter().all(|item| matches!(item, AstStatement::NullStatement))
                    && let Some(id) = function.return_type
                {
                    registry.validate(id, &Value::None)
                        .map_err(|message| error(&function.location, message))?;
                }
            },
        AstExpressionKind::IfExpression(item) =>
            {
                let mut remaining = bindings.clone();
                for branch in &item.branches
                {
                    remaining.methods = check_binding_expression(
                        registry, &branch.condition, &remaining, return_type, None)?;
                    let selected = condition_bindings(registry, &branch.condition,
                                                      &remaining, true);
                    remaining.methods = check_binding_scope(
                        registry, &branch.body.body, &selected, return_type, expected)?;
                    remaining = condition_bindings(registry, &branch.condition, &remaining, false);
                }
                if let Some(block) = &item.else_body
                { remaining.methods = check_binding_scope(
                    registry, &block.body, &remaining, return_type, expected)?; }
                methods = remaining.methods;
            },
        AstExpressionKind::MatchExpression(item) =>
            {
                methods = check_binding_expression(
                    registry, &item.value, bindings, return_type, None)?;
                let subject = type_subject(&item.value);
                let mut remaining = bindings.clone();
                remaining.methods = methods;
                let mut stable_subject = true;
                for arm in &item.arms
                {
                    let mut selected = remaining.clone();
                    if let Some(pattern) = &arm.pattern
                    {
                        selected.methods = check_binding_expression(
                            registry, pattern, &remaining, return_type, None)?;
                        remaining.methods = selected.methods.clone();
                        if expression_may_mutate(registry, pattern, &remaining)
                        {
                            stable_subject = false;
                            forget_inferred(&mut remaining);
                            selected = remaining.clone();
                        }
                        if stable_subject && let (Some(name), Some(id)) =
                            (subject, type_value(pattern, &remaining))
                        {
                            refine_type(registry, &mut selected, name, id, true);
                            refine_type(registry, &mut remaining, name, id, false);
                        }
                    }
                    remaining.methods = check_binding_scope(
                        registry, &arm.body.body, &selected, return_type, expected)?;
                }
                methods = remaining.methods;
            },
        AstExpressionKind::Grouped(inner) =>
            {
                let expected = if implicit_callable_result(registry, inner, bindings).is_some()
                    { None } else { expected };
                methods = check_binding_expression(
                    registry, inner, bindings, return_type, expected)?;
            },
        AstExpressionKind::Execute(call) =>
            methods = check_binding_call(registry, call, bindings, return_type)?,
        AstExpressionKind::Redirect(source, redirects) =>
            {
                let mut current = bindings.clone();
                current.methods = check_binding_expression(
                    registry, source, bindings, return_type, None)?;
                for redirect in redirects
                {
                    current.methods = check_binding_expression(
                        registry, &redirect.target, &current, return_type, None)?;
                }
                methods = current.methods;
            },
        AstExpressionKind::Array(values) =>
            {
                let mut current = bindings.clone();
                for value in values
                {
                    current.methods = check_binding_expression(
                        registry, value, &current, return_type, None)?;
                    if expression_may_mutate(registry, value, &current)
                    { forget_inferred(&mut current); }
                }
                methods = current.methods;
            },
        AstExpressionKind::HashMap(entries) =>
            {
                let mut current = bindings.clone();
                for (key, value) in entries
                {
                    for expression in [key, value]
                    {
                        current.methods = check_binding_expression(registry, expression,
                                                 &current, return_type, None)?;
                        if expression_may_mutate(registry, expression, &current)
                        { forget_inferred(&mut current); }
                    }
                }
                methods = current.methods;
            },
        AstExpressionKind::StructConstructor(item) =>
            {
                let mut current = bindings.clone();
                for (name, _, value) in &item.fields
                {
                    let expected = item.type_id.and_then(|id|
                        {
                            if !item.raw && let Some(signature) = registry.constructors.get(&id)
                            {
                                return signature.parameters.iter().find(|parameter|
                                    parameter.name.trim_start_matches('$') == name)
                                    .map(|parameter| parameter.type_id);
                            }
                            let definition = registry.get(id);
                            if let TypeKind::Struct(fields) = &definition.kind
                            { fields.iter().find(|field| field.name == *name)
                                .map(|field| field.type_id) }
                            else { None }
                        });
                    current.methods = check_binding_expression(
                        registry, value, &current, return_type, expected)?;
                    if expression_may_mutate(registry, value, &current)
                    { forget_inferred(&mut current); }
                }
                methods = current.methods;
            },
        AstExpressionKind::Range(start, end, _) =>
            {
                let mut current = bindings.clone();
                for value in [start, end].into_iter().flatten()
                {
                    current.methods = check_binding_expression(
                        registry, value, &current, return_type, None)?;
                    if expression_may_mutate(registry, value, &current)
                    { forget_inferred(&mut current); }
                }
                methods = current.methods;
            },
        AstExpressionKind::BooleanExpression(operator, left, right) =>
            {
                methods = check_binding_expression(
                    registry, left, bindings, return_type, None)?;
                let mut right_bindings = match operator
                    {
                        AstBooleanOperator::And =>
                            condition_bindings(registry, left, bindings, true),
                        AstBooleanOperator::Or =>
                            condition_bindings(registry, left, bindings, false),
                        _ =>
                            {
                                let mut current = bindings.clone();
                                if expression_may_mutate(registry, left, bindings)
                                { forget_inferred(&mut current); }
                                current
                            },
                    };
                right_bindings.methods = methods;
                methods = check_binding_expression(
                    registry, right, &right_bindings, return_type, None)?;
            },
        AstExpressionKind::Field(value, name, _) =>
            {
                methods = check_binding_expression(registry, value, bindings, return_type, None)?;
                if name.strip_prefix("::").unwrap_or(name).starts_with('$')
                {
                    if expression_may_mutate(registry, value, bindings)
                    {
                        let mut current = bindings.clone();
                        forget_inferred(&mut current);
                        check_field_name(registry, name, &current, &expression.location)?;
                    }
                    else { check_field_name(registry, name, bindings, &expression.location)?; }
                }
            },
        AstExpressionKind::MathExpression(_, left, right)
        | AstExpressionKind::Index(left, right) =>
            {
                methods = check_binding_expression(
                    registry, left, bindings, return_type, None)?;
                let mut current = bindings.clone();
                current.methods = methods;
                if expression_may_mutate(registry, left, bindings)
                { forget_inferred(&mut current); }
                methods = check_binding_expression(
                    registry, right, &current, return_type, None)?;
            },
        AstExpressionKind::Splat(value)
        | AstExpressionKind::MathNegate(value) | AstExpressionKind::BooleanNot(value)
        | AstExpressionKind::ExecutableReference(value)
        | AstExpressionKind::TryExecute(value) =>
            { methods = check_binding_expression(
                registry, value, bindings, return_type, None)?; },
        AstExpressionKind::TypeConversion(_, value, _, _) =>
            {
                methods = check_binding_expression(
                    registry, value, bindings, return_type, None)?;
            },
        _ => {}
    }
    Ok(methods)
}


// Only declared constraints override runtime inference. Inferred facts can be
// stale after mutation, and an unknown element may have a concrete runtime type.
fn declared_conversion_source(registry: &TypeRegistry, expression: &AstExpression,
                              bindings: &Bindings) -> Option<TypeId>
{
    match &expression.kind
    {
        AstExpressionKind::Variable(item) =>
            {
                let binding = bindings.get(&item.name)?;
                if binding.refined.is_some() { method_receiver_type(registry, expression, bindings) }
                else { binding.constraint }
            },
        AstExpressionKind::Grouped(inner) | AstExpressionKind::TryExecute(inner) =>
            {
                let id = declared_conversion_source(registry, inner, bindings)?;
                match registry.get(registry.underlying_type(id)).kind
                {
                    TypeKind::Function(_, result) => Some(result),
                    _ => Some(id),
                }
            },
        AstExpressionKind::Execute(call) =>
            {
                let id = declared_conversion_source(registry, &call.executable, bindings)?;
                match registry.get(registry.underlying_type(id)).kind
                {
                    TypeKind::Function(_, result) => Some(result),
                    _ => None,
                }
            },
        AstExpressionKind::Field(object, name, _) =>
            {
                let id = binding_type(registry, object, bindings)?;
                if let Some(name) = name.strip_prefix("::")
                {
                    let definition = registry.get(registry.underlying_type(id));
                    return if let TypeKind::Struct(fields) = &definition.kind
                        { fields.iter().find(|field| field.name == name)
                            .map(|field| field.type_id) }
                        else { None };
                }
                if let Some(ty) = registry.properties.get(&(id, name.clone()))
                { return Some(*ty); }
                if registry.collection_member_type(id, name).is_some()
                { return registry.builtin_id("Type"); }
                let definition = registry.get(registry.underlying_type(id));
                let TypeKind::Struct(fields) = &definition.kind else { return None; };
                fields.iter().find(|field| field.name == *name).map(|field| field.type_id)
            },
        AstExpressionKind::Index(object, index) =>
            {
                let id = declared_conversion_source(registry, object, bindings)?;
                let slice = slice_index(registry, binding_type(registry, index, bindings));
                let index = match constant_value(registry, index)
                {
                    Some(Value::Integer(index)) => usize::try_from(index).ok(),
                    _ => None,
                };
                indexed_type(registry, id, index, slice)
            },
        _ => None,
    }
}


fn binding_type(
    registry: &TypeRegistry, expression: &AstExpression, bindings: &Bindings,
) -> Option<TypeId>
{
    let mut stable;
    let bindings = if expression_may_mutate(registry, expression, bindings)
        && !matches!(ungroup(expression).kind, AstExpressionKind::Field(_, _, _))
        {
            stable = bindings.clone();
            forget_inferred(&mut stable);
            &stable
        } else { bindings };
    binding_type_inner(registry, expression, bindings)
}

fn call_result(registry: &TypeRegistry, call: &AstExecuteStatement,
               bindings: &Bindings) -> Option<TypeId>
{
    if let AstExpressionKind::Field(object, name, _) = &call.executable.kind
    {
        let id = binding_type(registry, object, bindings)?;
        let definition = registry.get(registry.underlying_type(id));
        if definition.name == "Type" && name == "name" && call.arguments.is_empty()
        { return registry.builtin_id("String"); }
        if !matches!(&definition.kind, TypeKind::Struct(fields)
            if fields.iter().any(|field| field.name == *name))
        { return method_receiver_type(registry, object, bindings)
                .and_then(|id| method_result(registry, id, name, bindings)); }
    }
    let id = binding_type(registry, &call.executable, bindings)?;
    match registry.get(registry.underlying_type(id)).kind
    {
        TypeKind::Function(_, result) => Some(result),
        _ => None,
    }
}

fn binding_type_inner(
    registry: &TypeRegistry, expression: &AstExpression, bindings: &Bindings,
) -> Option<TypeId>
{
    match &expression.kind
    {
        AstExpressionKind::Variable(variable) =>
            bindings.get(&variable.name).and_then(|binding| binding.type_id()),
        AstExpressionKind::Literal(item) => Some(registry.inferred_value_type(&item.value)),
        AstExpressionKind::Array(values) =>
            {
                let any = registry.builtin_id("any").unwrap();
                let element = registry.common_type(values.iter().map(|value|
                    {
                        let expanded = match &value.kind
                            {
                                AstExpressionKind::Splat(inner) =>
                                    binding_type(registry, inner, bindings),
                                AstExpressionKind::VariableSplat(variable) => bindings
                                    .get(&variable.name).and_then(|binding| binding.type_id()),
                                _ => return binding_type(registry, value, bindings).unwrap_or(any),
                            };
                        if let Some(id) = expanded
                        {
                            return match &registry.get(id).kind
                                {
                                    TypeKind::Array(item) => *item,
                                    TypeKind::FixedArray(items) =>
                                        registry.common_type(items.iter().copied()),
                                    _ if registry.get(id).name == "Range" =>
                                        registry.builtin_id("Integer").unwrap(),
                                    _ => any,
                                };
                        }
                        any
                    }));
                Some(registry.intern(TypeKind::Array(element)))
            },
        AstExpressionKind::HashMap(entries) =>
            {
                let any = registry.builtin_id("any").unwrap();
                let keys = registry.common_type(entries.iter().map(|(key, _)|
                    binding_type(registry, key, bindings).unwrap_or(any)));
                let values = registry.common_type(entries.iter().map(|(_, value)|
                    binding_type(registry, value, bindings).unwrap_or(any)));
                Some(registry.intern(TypeKind::Map(keys, values)))
            },
        AstExpressionKind::Range(_, _, _) => registry.builtin_id("Range"),
        AstExpressionKind::Grouped(inner) => implicit_callable_result(registry, inner, bindings)
            .or_else(|| binding_type(registry, inner, bindings)),
        AstExpressionKind::Execute(call) => call_result(registry, call, bindings),
        AstExpressionKind::Field(object, name, _) =>
            {
                if name == "type" { return registry.builtin_id("Type"); }
                let id = binding_type(registry, object, bindings)?;
                if let Some(name) = name.strip_prefix("::")
                {
                    let definition = registry.get(registry.underlying_type(id));
                    return if let TypeKind::Struct(fields) = &definition.kind
                        { fields.iter().find(|field| field.name == name)
                            .map(|field| field.type_id) }
                        else { None };
                }
                if let Some(ty) = registry.properties.get(&(id, name.clone()))
                { return Some(*ty); }
                if registry.collection_member_type(id, name).is_some()
                { return registry.builtin_id("Type"); }
                let definition = registry.get(registry.underlying_type(id));
                if matches!(definition.kind, TypeKind::Enum(_)) && name == "index"
                { return registry.builtin_id("Integer"); }
                if definition.name == "Type" && name == "name"
                { return registry.builtin_id("String"); }
                if let TypeKind::Struct(fields) = &definition.kind
                {
                    if let Some(field) = fields.iter().find(|field| field.name == *name)
                    { return Some(field.type_id); }
                }
                method_receiver_type(registry, object, bindings)
                    .and_then(|id| method_result(registry, id, name, bindings))
            },
        AstExpressionKind::Index(object, index) =>
            {
                let slice = slice_index(registry, binding_type(registry, index, bindings));
                let index = match constant_value(registry, index)
                    {
                        Some(Value::Integer(index)) => usize::try_from(index).ok(),
                        _ => None,
                    };
                indexed_type(registry, binding_type(registry, object, bindings)?, index, slice)
            },
        _ => known_type(registry, expression)
    }
}


// These reference expressions are implicitly called in groups and statement position.
fn implicit_callable_result(registry: &TypeRegistry, expression: &AstExpression,
                            bindings: &Bindings) -> Option<TypeId>
{
    if !matches!(expression.kind, AstExpressionKind::Variable(_)
        | AstExpressionKind::Index(_, _) | AstExpressionKind::Field(_, _, _)
        | AstExpressionKind::ExecutableReference(_)) { return None; }
    if returns_call_data(registry, expression, bindings) { return None; }
    let id = binding_type(registry, expression, bindings)?;
    match registry.get(registry.underlying_type(id)).kind
    {
        TypeKind::Function(_, result) => Some(result),
        _ => None,
    }
}


// An unknown index may be an integer or a range. Do not assume an element result.
fn slice_index(registry: &TypeRegistry, index_type: Option<TypeId>) -> Option<bool>
{
    match index_type
    {
        Some(id) if Some(id) == registry.builtin_id("Range") => Some(true),
        Some(id) if Some(id) == registry.builtin_id("Integer") => Some(false),
        _ => None,
    }
}


fn destructured_type(registry: &TypeRegistry, expression: &AstExpression,
                     bindings: &Bindings, ty: Option<TypeId>, index: usize) -> Option<TypeId>
{
    match &expression.kind
    {
        AstExpressionKind::Grouped(inner) =>
            destructured_type(registry, inner, bindings, ty, index),
        AstExpressionKind::Array(items) if !items.iter().any(|item| matches!(item.kind,
            AstExpressionKind::Splat(_) | AstExpressionKind::VariableSplat(_))) =>
            items.get(index).and_then(|item| binding_type(registry, item, bindings)),
        _ => ty.and_then(|id| indexed_type(registry, id, Some(index), Some(false))),
    }
}


fn indexed_type(registry: &TypeRegistry, id: TypeId,
                index: Option<usize>, slice: Option<bool>) -> Option<TypeId>
{
    let id = registry.underlying_type(id);
    match &registry.get(id).kind
    {
        TypeKind::Array(element) => Some(if slice? { id } else { *element }),
        TypeKind::FixedArray(items) if slice == Some(true) => Some(registry.intern(TypeKind::Array(
            registry.common_type(items.iter().copied())))),
        TypeKind::Map(_, element) => Some(registry.intern(TypeKind::Optional(*element))),
        TypeKind::FixedArray(items) if slice == Some(false) => match index
            {
                Some(index) => items.get(index).copied(),
                None => Some(registry.common_type(items.iter().copied())),
            },
        _ => None,
    }
}


fn iterator_item_type(registry: &TypeRegistry, expression: &AstExpression,
                      bindings: &Bindings) -> Option<TypeId>
{
    let receiver = binding_type(registry, expression, bindings)?;
    // An override has its own scoped/versioned signature, not the native one.
    if registry.has_extension(receiver, "next_item") { return None; }
    let method = registry.method(receiver, "next_item")?;
    match registry.get(method.return_type).kind
    {
        TypeKind::Optional(item) => match registry.get(item).kind
            {
                TypeKind::Optional(inner) => Some(inner),
                _ if registry.get(item).name == "None" => None,
                _ => Some(item),
            },
        _ => None,
    }
}


// Calls can write dynamically scoped variables. Inferred contents are snapshots,
// not constraints: discard them across calls and control-flow joins.
fn expression_may_mutate(registry: &TypeRegistry, expression: &AstExpression,
                          bindings: &Bindings) -> bool
{
    if interpolated_members(expression).iter()
        .any(|member| expression_may_mutate(registry, member, bindings))
    { return true; }
    let mutates = |item: &AstExpression| expression_may_mutate(registry, item, bindings);
    match &expression.kind
    {
        AstExpressionKind::Field(value, name, _) =>
            {
                if mutates(value) { return true; }
                if name == "type" { return false; }
                if let Some(id) = binding_type(registry, value, bindings)
                {
                    if registry.collection_member_type(id, name).is_some() { return false; }
                    if registry.get(registry.underlying_type(id)).name == "Type" && name == "name"
                    { return false; }
                    match &registry.get(registry.underlying_type(id)).kind
                    {
                        TypeKind::Struct(fields)
                            if fields.iter().any(|field| field.name == *name) =>
                            return false,
                        TypeKind::Enum(_) if name == "index" => return false,
                        _ => {},
                    }
                }
                true
            },
        AstExpressionKind::Execute(_) | AstExpressionKind::Pipeline(_)
        | AstExpressionKind::IfExpression(_) | AstExpressionKind::MatchExpression(_)
        | AstExpressionKind::Redirect(_, _) | AstExpressionKind::TryExecute(_) => true,
        AstExpressionKind::Grouped(value) =>
            {
                let callable = binding_type(registry, value, bindings).is_some_and(|id|
                    matches!(registry.get(registry.underlying_type(id)).kind,
                             TypeKind::Function(_, _)));
                callable || matches!(value.kind, AstExpressionKind::Variable(_)
                    | AstExpressionKind::Index(_, _) | AstExpressionKind::ExecutableReference(_))
                    || mutates(value)
            },
        AstExpressionKind::Splat(value)
        | AstExpressionKind::ExecutableReference(value)
        | AstExpressionKind::MathNegate(value) | AstExpressionKind::BooleanNot(value) =>
            mutates(value),
        AstExpressionKind::TypeConversion(_, value, target, _) =>
            target.is_some_and(|id| registry.has_conversion_methods(id)) || mutates(value),
        AstExpressionKind::Array(values) => values.iter().any(mutates),
        AstExpressionKind::HashMap(entries) => entries.iter().any(|(key, value)|
            mutates(key) || mutates(value)),
        AstExpressionKind::StructConstructor(item) =>
            (!item.raw && item.type_id.is_some_and(|id| registry.constructors.contains_key(&id)))
                || item.fields.iter().any(|(_, _, value)| mutates(value)),
        AstExpressionKind::MathExpression(_, left, right)
        | AstExpressionKind::BooleanExpression(_, left, right)
        | AstExpressionKind::Index(left, right) => mutates(left) || mutates(right),
        AstExpressionKind::Range(start, end, _) =>
            start.iter().chain(end).any(|value| mutates(value)),
        _ => false,
    }
}


fn check_field_name(registry: &TypeRegistry, name: &str, bindings: &Bindings,
                    location: &Location) -> CompileResult<()>
{
    let name = name.strip_prefix("::").unwrap_or(name);
    if !name.starts_with('$') { return Ok(()); }
    if let Some(id) = bindings.get(name).and_then(|binding| binding.type_id())
    {
        let definition = registry.get(registry.underlying_type(id));
        if !field_name_can_be_string(registry, id)
        {
            return Err(error(location, format!(
                "Field name must be String, got {}", definition.name)));
        }
    }
    Ok(())
}


fn field_name_can_be_string(registry: &TypeRegistry, id: TypeId) -> bool
{
    let definition = registry.get(registry.underlying_type(id));
    match &definition.kind
    {
        TypeKind::Optional(inner) => field_name_can_be_string(registry, *inner),
        TypeKind::Union(items) => items.iter().any(|id| field_name_can_be_string(registry, *id)),
        _ => definition.name == "String" || definition.name == "any",
    }
}


fn interpolated_members(expression: &AstExpression) -> Vec<AstExpression>
{
    let Some(AstStringFlag::Interpolated(escaped)) = &expression.string_flag
    else { return Vec::new(); };
    let text = match &expression.kind
        {
            AstExpressionKind::Literal(item) => match &item.value
                { Value::String(text, _) => Some(text.as_str()), _ => None },
            AstExpressionKind::Symbol(item) => Some(item.name.as_str()),
            _ => None,
        };
    let Some(text) = text else { return Vec::new(); };
    interpolation_references(text, escaped).into_iter()
        .filter(|reference| reference.error.is_none() && reference.name.contains('.'))
        .map(|reference|
            {
                let mut parts = reference.name.split('.');
                let mut member = AstExpression
                    {
                        interpolation_members: Default::default(),
                        dispatch_type: Default::default(),
                        location: expression.location.clone(), string_flag: None,
                        kind: AstExpressionKind::Variable(AstSymbol
                            { name: parts.next().unwrap().into() }),
                    };
                for name in parts
                {
                    member = AstExpression
                        {
                            interpolation_members: Default::default(),
                            dispatch_type: Default::default(),
                            location: expression.location.clone(), string_flag: None,
                            kind: AstExpressionKind::Field(Box::new(member), name.into(), None),
                        };
                }
                member
            }).collect()
}


pub(crate) fn check_protocol(
    registry: &mut TypeRegistry,
    function: &mut AstFunctionStatement,
    names: &Names,
) -> CompileResult<()>
{
    let Some(id) = function.receiver_type else { return Ok(()); };
    let definition = registry.get(id);
    if function.name != "new" && !function.name.starts_with("get_")
        && !function.name.starts_with("set_") { return Ok(()); }
    let getter_extension = function.name.starts_with("get_")
        && matches!(definition.kind, TypeKind::Named(_) | TypeKind::Builtin);
    if !getter_extension
        && !(function.name == "new" && matches!(definition.kind, TypeKind::Named(_)))
        && !matches!(definition.kind, TypeKind::Struct(_) | TypeKind::Enum(_))
    { return Err(error(&function.location,
        "Struct/enum protocols require their owning type".into())); }
    for parameter in &mut function.parameters
    {
        if let Some(annotation) = &parameter.annotation
        { parameter.type_id = Some(resolve_type(
            registry, annotation, names, &parameter.location)?); }
    }
    if let Some(annotation) = &function.return_annotation
    { function.return_type = Some(resolve_type(registry, annotation, names, &function.location)?); }
    if function.name == "new"
    {
        if definition.location.as_ref().is_some_and(|location|
            location.origin != function.location.origin)
        { return Err(error(&function.location,
            "Constructor must be declared in the type's defining module".into())); }
        let result = function.return_type.ok_or_else(|| error(&function.location,
            "::new must declare its result type".into()))?;
        let nullable = registry.intern(TypeKind::Optional(id));
        if !registry.valid_factory_result(id, result) && result != nullable
        { return Err(error(&function.location,
            "::new must return its own type or that type | ()".into())); }
        if matches!(definition.kind, TypeKind::Enum(_))
            && (!function.parameters.is_empty() || result != id)
        { return Err(error(&function.location,
            "Enum ::new must take no arguments and return its enum".into())); }
        let parameters = function.parameters.iter().map(|parameter|
            ConstructorParameter
                {
                    name: parameter.name.trim_start_matches('$').into(),
                    type_id: parameter.type_id
                        .unwrap_or_else(|| registry.builtin_id("any").unwrap()),
                    optional: parameter.optional || parameter.variadic
                        || parameter.type_id.is_some_and(|id|
                            matches!(registry.get(id).kind, TypeKind::Optional(_))),
                }).collect();
        let signature = ConstructorSignature { parameters, result,
            variadic: function.parameters.last().is_some_and(|parameter| parameter.variadic) };
        if registry.constructors.get(&id).is_some_and(|previous| *previous != signature)
        { return Err(error(&function.location, "A type has one constructor interface".into())); }
        registry.constructors.insert(id, signature);
        registry.constructor_permissions.insert(id);
        return Ok(());
    }
    if !getter_extension && !matches!(definition.kind, TypeKind::Struct(_))
    { return Err(error(&function.location, "Enums do not support getters/setters".into())); }
    let getter = function.name.starts_with("get_");
    if !getter && function.return_type.is_none()
    { function.return_type = registry.builtin_id("None"); }
    let property = &function.name[4..];
    if property.is_empty() || function.parameters.len() != if getter { 1 } else { 2 }
        || function.parameters.iter().any(|p| p.variadic)
    { return Err(error(&function.location,
        "Getter/setter protocol has an invalid parameter list".into())); }
    let ty = if getter { function.return_type } else { function.parameters[1].type_id }
        .ok_or_else(|| error(&function.location,
            "Getter/setter property type must be declared".into()))?;
    if !getter && function.return_type.is_some_and(|ty| Some(ty) != registry.builtin_id("None"))
    { return Err(error(&function.location, "Setters must return ()".into())); }
    let key = (id, property.to_string());
    if registry.properties.get(&key).is_some_and(|previous| *previous != ty)
    { return Err(error(&function.location,
        "Getter and setter property types must match exactly".into())); }
    registry.properties.insert(key, ty);
    Ok(())
}


fn optional_constructor_result(registry: &TypeRegistry, id: TypeId) -> bool
{
    match &registry.get(id).kind
    {
        TypeKind::Optional(inner) => registry.constructors.contains_key(inner),
        TypeKind::Union(items) => items.iter().any(|id| optional_constructor_result(registry, *id)),
        _ => false,
    }
}

// Lexical control flow may finish loops created in finally, but cannot escape its boundary.
fn check_finally(statements: &AstTopLevel, loops: usize) -> CompileResult<()>
{
    for statement in statements
    {
        match statement
        {
            AstStatement::ReturnStatement(item) =>
            {
                return Err(error(&item.location, "return cannot escape finally".into()));
            }
            AstStatement::BreakStatement(location) | AstStatement::ContinueStatement(location)
                if loops == 0 =>
            {
                return Err(error(location, "loop control cannot escape finally".into()));
            }
            AstStatement::ExpressionStatement(value)
            | AstStatement::DiscardStatement(value)
            | AstStatement::ThrowStatement(value) => check_finally_expression(value, loops)?,
            AstStatement::LetStatement(item) => check_finally_expression(&item.expression, loops)?,
            AstStatement::DestructureStatement(item) =>
                check_finally_expression(&item.expression, loops)?,
            AstStatement::SetStatement(item) => check_finally_expression(&item.expression, loops)?,
            AstStatement::ExecuteStatement(item) =>
            {
                check_finally_expression(&item.executable, loops)?;
                for argument in &item.arguments
                {
                    check_finally_expression(argument, loops)?;
                }
            }
            AstStatement::BlockStatement(item) => check_finally(&item.body, loops)?,
            AstStatement::LoopStatement(item) => check_finally(&item.body, loops + 1)?,
            AstStatement::ConditionalLoopStatement(item) =>
            {
                check_finally_expression(&item.condition, loops)?;
                check_finally(&item.body.body, loops + 1)?;
            }
            AstStatement::ForStatement(item) =>
            {
                check_finally_expression(&item.iterable, loops)?;
                check_finally(&item.body.body, loops + 1)?;
            }
            AstStatement::TryStatement(item) =>
            {
                check_finally(&item.body.body, loops)?;
                for catch in &item.catches
                {
                    check_finally(&catch.body.body, loops)?;
                }
                if let Some(finally) = &item.finally
                {
                    check_finally(&finally.body, loops)?;
                }
            }
            _ =>
            {}
        }
    }
    Ok(())
}

fn check_finally_expression(expression: &AstExpression, loops: usize) -> CompileResult<()>
{
    match &expression.kind
    {
        AstExpressionKind::IfExpression(item) =>
        {
            for branch in &item.branches
            {
                check_finally_expression(&branch.condition, loops)?;
                check_finally(&branch.body.body, loops)?;
            }
            if let Some(body) = &item.else_body
            {
                check_finally(&body.body, loops)?;
            }
        }
        AstExpressionKind::MatchExpression(item) =>
        {
            check_finally_expression(&item.value, loops)?;
            for arm in &item.arms
            {
                if let Some(pattern) = &arm.pattern
                {
                    check_finally_expression(pattern, loops)?;
                }
                check_finally(&arm.body.body, loops)?;
            }
        }
        AstExpressionKind::Grouped(value)
        | AstExpressionKind::TryExecute(value)
        | AstExpressionKind::Splat(value)
        | AstExpressionKind::BooleanNot(value)
        | AstExpressionKind::MathNegate(value)
        | AstExpressionKind::ExecutableReference(value)
        | AstExpressionKind::Field(value, _, _)
        | AstExpressionKind::TypeConversion(_, value, _, _) =>
        {
            check_finally_expression(value, loops)?
        }
        AstExpressionKind::Index(left, right)
        | AstExpressionKind::MathExpression(_, left, right)
        | AstExpressionKind::BooleanExpression(_, left, right) =>
        {
            check_finally_expression(left, loops)?;
            check_finally_expression(right, loops)?;
        }
        AstExpressionKind::Array(items) =>
        {
            for item in items
            {
                check_finally_expression(item, loops)?;
            }
        }
        AstExpressionKind::HashMap(items) =>
        {
            for (key, value) in items
            {
                check_finally_expression(key, loops)?;
                check_finally_expression(value, loops)?;
            }
        }
        AstExpressionKind::StructConstructor(item) =>
        {
            for (_, _, value) in &item.fields
            {
                check_finally_expression(value, loops)?;
            }
        }
        AstExpressionKind::Execute(item) =>
        {
            check_finally_expression(&item.executable, loops)?;
            for argument in &item.arguments
            {
                check_finally_expression(argument, loops)?;
            }
        }
        AstExpressionKind::Pipeline(item) =>
        {
            for stage in &item.stages
            {
                check_finally_expression(stage, loops)?;
            }
        }
        AstExpressionKind::Redirect(value, redirects) =>
        {
            check_finally_expression(value, loops)?;
            for redirect in redirects
            {
                check_finally_expression(&redirect.target, loops)?;
            }
        }
        AstExpressionKind::Range(start, end, _) =>
        {
            for value in start.iter().chain(end.iter())
            {
                check_finally_expression(value, loops)?;
            }
        }
        _ =>
        {}
    }
    Ok(())
}

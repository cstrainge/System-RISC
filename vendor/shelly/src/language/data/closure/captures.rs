
use std::collections::HashSet;

use crate::language::{ ast::*, data::value::Value,
                       text::interpolation::interpolation_references };

pub fn free_variables(function: &AstFunctionStatement) -> Vec<String>
{
    // Capture used bindings, retaining their Rc rather than their current value.
    // Nested literals contribute outer references needed when they are created later.
    let mut free = HashSet::new();
    let bound = function.parameters.iter().map(|item| item.name.clone()).collect();
    statements(&function.body, &bound, &mut free);
    let mut names: Vec<_> = free.into_iter().collect();
    names.sort();
    names
}

fn name(value: &str, bound: &HashSet<String>, free: &mut HashSet<String>)
{
    if !bound.contains(value) { free.insert(value.to_string()); }
}

fn text(value: &str, escaped: &[usize], bound: &HashSet<String>, free: &mut HashSet<String>)
{
    for reference in interpolation_references(value, escaped)
    {
        for (index, part) in reference.name.split('.').enumerate()
        {
            let part = part.strip_prefix("::").unwrap_or(part);
            if index == 0 || part.starts_with('$') { name(part, bound, free); }
        }
    }
}

fn statements(items: &AstTopLevel, parent: &HashSet<String>, free: &mut HashSet<String>)
{
    let mut bound = parent.clone();
    for item in items
    {
        match item
        {
            AstStatement::TryStatement(item) =>
                {
                    statements(&item.body.body, &bound, free);
                    for catch in &item.catches
                    {
                        let mut handler = bound.clone();
                        handler.insert(catch.error.clone());
                        if let Some(stack) = &catch.stack { handler.insert(stack.clone()); }
                        statements(&catch.body.body, &handler, free);
                    }
                    if let Some(finally) = &item.finally { statements(&finally.body, &bound,
                        free); }
                },
            AstStatement::ThrowStatement(item) => expression(item, &bound, free),
            AstStatement::LetStatement(item) =>
                {
                    if item.annotation.as_ref().is_some_and(AstType::uses_receiver)
                    { name("$self", &bound, free); }
                    expression(&item.expression, &bound, free);
                    bound.insert(item.identifier.clone());
                },
            AstStatement::DestructureStatement(item) =>
                {
                    expression(&item.expression, &bound, free);
                    bound.extend(item.bindings.iter().filter_map(|(_, name)| name.clone()));
                },
            AstStatement::SetStatement(item) =>
                {
                    name(&item.identifier, &bound, free);
                    for access in &item.indexes
                    {
                        match access
                        {
                            AstAccess::Index(index) => expression(index, &bound, free),
                            AstAccess::Field(field)
                                if field.strip_prefix("::").unwrap_or(field).starts_with('$') =>
                                name(field.strip_prefix("::").unwrap_or(field), &bound, free),
                            _ => {},
                        }
                    }
                    expression(&item.expression, &bound, free);
                },
            AstStatement::ExpressionStatement(item) | AstStatement::DiscardStatement(item) =>
                expression(item, &bound, free),
            AstStatement::ExecuteStatement(item) => call(item, &bound, free),
            AstStatement::ReturnStatement(item) =>
                { if let Some(item) = &item.expression { expression(item, &bound, free); } },
            AstStatement::FunctionDefinition(item) =>
                {
                    let mut nested = bound.clone();
                    nested.extend(item.parameters.iter().map(|item| item.name.clone()));
                    statements(&item.body, &nested, free);
                },
            AstStatement::BlockStatement(item) | AstStatement::LoopStatement(item) =>
                statements(&item.body, &bound, free),
            AstStatement::ForStatement(item) =>
                {
                    expression(&item.iterable, &bound, free);
                    let mut nested = bound.clone();
                    nested.extend(item.bindings.iter().flatten().cloned());
                    statements(&item.body.body, &nested, free);
                },
            AstStatement::ConditionalLoopStatement(item) =>
                {
                    expression(&item.condition, &bound, free);
                    statements(&item.body.body, &bound, free);
                },
            AstStatement::ImportStatement(_) | AstStatement::EnumDeclaration(_)
            | AstStatement::StructDeclaration(_) | AstStatement::TypeDeclaration(_)
            | AstStatement::AliasStatement(_) | AstStatement::BreakStatement(_)
            | AstStatement::ContinueStatement(_) | AstStatement::NullStatement => {},
        }
    }
}

fn call(item: &AstExecuteStatement, bound: &HashSet<String>, free: &mut HashSet<String>)
{
    expression(&item.executable, bound, free);
    for item in &item.arguments { expression(item, bound, free); }
}

fn expression(item: &AstExpression, bound: &HashSet<String>, free: &mut HashSet<String>)
{
    match &item.kind
    {
        AstExpressionKind::Pipeline(pipeline) =>
            {
                expression(&pipeline.stages[0], bound, free);
                let mut nested = bound.clone();
                nested.insert("$".into());
                for item in &pipeline.stages[1..] { expression(item, &nested, free); }
            },
        AstExpressionKind::Variable(item) | AstExpressionKind::VariableSplat(item) =>
            name(&item.name, bound, free),
        AstExpressionKind::Literal(item_value) =>
            {
                if let Value::String(value, _) = &item_value.value
                    && let Some(AstStringFlag::Interpolated(escaped)) = &item.string_flag
                { text(value, escaped, bound, free); }
            },
        AstExpressionKind::Symbol(symbol) =>
            {
                match &item.string_flag
                {
                    Some(AstStringFlag::NonInterpolated) => {},
                    Some(AstStringFlag::Interpolated(escaped)) =>
                        text(&symbol.name, escaped, bound, free),
                    None => text(&symbol.name, &[], bound, free),
                }
            },
        AstExpressionKind::AnonymousFunction(function) =>
            {
                let mut nested = bound.clone();
                nested.extend(function.parameters.iter().map(|item| item.name.clone()));
                statements(&function.body, &nested, free);
            },
        AstExpressionKind::Execute(item) => call(item, bound, free),
        AstExpressionKind::Array(items) =>
            { for item in items { expression(item, bound, free); } },
        AstExpressionKind::HashMap(items) =>
            {
                for (key, value) in items
                { expression(key, bound, free); expression(value, bound, free); }
            },
        AstExpressionKind::StructConstructor(item) =>
            { for (_, _, item) in &item.fields { expression(item, bound, free); } },
        AstExpressionKind::Range(start, end, _) =>
            { for item in start.iter().chain(end) { expression(item, bound, free); } },
        AstExpressionKind::Index(a, b) | AstExpressionKind::MathExpression(_, a, b)
        | AstExpressionKind::BooleanExpression(_, a, b) =>
            { expression(a, bound, free); expression(b, bound, free); },
        AstExpressionKind::Field(item, field, _) =>
            {
                expression(item, bound, free);
                let field = field.strip_prefix("::").unwrap_or(field);
                if field.starts_with('$') { name(field, bound, free); }
            },
        AstExpressionKind::Splat(item)
        | AstExpressionKind::Grouped(item) | AstExpressionKind::ExecutableReference(item)
        | AstExpressionKind::TryExecute(item) | AstExpressionKind::MathNegate(item)
        | AstExpressionKind::BooleanNot(item) | AstExpressionKind::TypeConversion(_, item, _, _) =>
            expression(item, bound, free),
        AstExpressionKind::Redirect(item, redirects) =>
            {
                expression(item, bound, free);
                for item in redirects { expression(&item.target, bound, free); }
            },
        AstExpressionKind::IfExpression(item) =>
            {
                for branch in &item.branches
                {
                    expression(&branch.condition, bound, free);
                    statements(&branch.body.body, bound, free);
                }
                if let Some(body) = &item.else_body { statements(&body.body, bound, free); }
            },
        AstExpressionKind::MatchExpression(item) =>
            {
                expression(&item.value, bound, free);
                for arm in &item.arms
                {
                    if let Some(pattern) = &arm.pattern { expression(pattern, bound, free); }
                    statements(&arm.body.body, bound, free);
                }
            },
        AstExpressionKind::SpacedEmptyCall(_) | AstExpressionKind::EnumVariant(_, _) => {},
    }
}

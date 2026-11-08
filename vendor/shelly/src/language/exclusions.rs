
use std::mem::take;

use crate::language::ast::*;


// This compilation prepass runs before imports and name/type registration. Its evaluator
// belongs to the interpreter, so conditions use the same values and errors as import when.
pub fn exclude_statements<E>(
    statements: &mut AstTopLevel,
    evaluate: &mut impl FnMut(AstExpression) -> Result<bool, E>,
) -> Result<(), E>
{
    let mut included = Vec::new();
    for mut statement in take(statements)
    {
        match &mut statement
        {
            AstStatement::TryStatement(item) =>
                {
                    exclude_statements(&mut item.body.body, evaluate)?;
                    for catch in &mut item.catches
                    { exclude_statements(&mut catch.body.body, evaluate)?; }
                    if let Some(finally) = &mut item.finally
                    { exclude_statements(&mut finally.body, evaluate)?; }
                },
            AstStatement::ThrowStatement(item) => exclude_expression(item, evaluate)?,
            AstStatement::FunctionDefinition(function) =>
                {
                    if let Some(mut condition) = function.condition.take()
                    {
                        exclude_expression(&mut condition, evaluate)?;
                        if !evaluate(condition)? { continue; }
                    }
                    exclude_statements(&mut function.body, evaluate)?;
                },
            AstStatement::BlockStatement(block) =>
                {
                    if let Some(mut condition) = block.condition.take()
                    {
                        exclude_expression(&mut condition, evaluate)?;
                        if !evaluate(condition)? { continue; }
                        exclude_statements(&mut block.body, evaluate)?;
                        // An annotated block is a declaration group, not a runtime scope.
                        included.append(&mut block.body);
                        continue;
                    }
                    exclude_statements(&mut block.body, evaluate)?;
                },
            AstStatement::LoopStatement(block) => exclude_statements(&mut block.body, evaluate)?,
            AstStatement::ForStatement(item) =>
                {
                    exclude_expression(&mut item.iterable, evaluate)?;
                    exclude_statements(&mut item.body.body, evaluate)?;
                },
            AstStatement::ConditionalLoopStatement(item) =>
                {
                    exclude_expression(&mut item.condition, evaluate)?;
                    exclude_statements(&mut item.body.body, evaluate)?;
                },
            AstStatement::LetStatement(item) => exclude_expression(&mut item.expression, evaluate)?,
            AstStatement::DestructureStatement(item) =>
                exclude_expression(&mut item.expression, evaluate)?,
            AstStatement::SetStatement(item) =>
                {
                    for access in &mut item.indexes
                    {
                        if let AstAccess::Index(index) = access
                        { exclude_expression(index, evaluate)?; }
                    }
                    exclude_expression(&mut item.expression, evaluate)?;
                },
            AstStatement::ExpressionStatement(expression)
                | AstStatement::DiscardStatement(expression) =>
                exclude_expression(expression, evaluate)?,
            AstStatement::ExecuteStatement(call) => exclude_call(call, evaluate)?,
            AstStatement::ReturnStatement(item) =>
                {
                    if let Some(expression) = &mut item.expression
                    { exclude_expression(expression, evaluate)?; }
                },
            AstStatement::ImportStatement(item) =>
                {
                    if let Some(expression) = &mut item.condition
                    { exclude_expression(expression, evaluate)?; }
                },
            AstStatement::EnumDeclaration(_) | AstStatement::StructDeclaration(_)
                | AstStatement::TypeDeclaration(_)
                | AstStatement::AliasStatement(_) | AstStatement::BreakStatement(_)
                | AstStatement::ContinueStatement(_) | AstStatement::NullStatement => {},
        }
        included.push(statement);
    }
    *statements = included;
    Ok(())
}


fn exclude_call<E>(call: &mut AstExecuteStatement,
                   evaluate: &mut impl FnMut(AstExpression) -> Result<bool, E>) -> Result<(), E>
{
    exclude_expression(&mut call.executable, evaluate)?;
    for argument in &mut call.arguments { exclude_expression(argument, evaluate)?; }
    Ok(())
}


fn exclude_expression<E>(expression: &mut AstExpression,
                         evaluate: &mut impl FnMut(AstExpression) -> Result<bool, E>)
    -> Result<(), E>
{
    match &mut expression.kind
    {
        AstExpressionKind::Pipeline(pipeline) =>
            {
                for stage in &mut pipeline.stages { exclude_expression(stage, evaluate)?; }
            },
        AstExpressionKind::AnonymousFunction(function) =>
            exclude_statements(&mut function.body, evaluate)?,
        AstExpressionKind::IfExpression(item) =>
            {
                for branch in &mut item.branches
                {
                    exclude_expression(&mut branch.condition, evaluate)?;
                    exclude_statements(&mut branch.body.body, evaluate)?;
                }
                if let Some(block) = &mut item.else_body
                { exclude_statements(&mut block.body, evaluate)?; }
            },
        AstExpressionKind::MatchExpression(item) =>
            {
                exclude_expression(&mut item.value, evaluate)?;
                for arm in &mut item.arms
                {
                    if let Some(pattern) = &mut arm.pattern
                    { exclude_expression(pattern, evaluate)?; }
                    exclude_statements(&mut arm.body.body, evaluate)?;
                }
            },
        AstExpressionKind::Execute(call) => exclude_call(call, evaluate)?,
        AstExpressionKind::Array(values) =>
            { for value in values { exclude_expression(value, evaluate)?; } },
        AstExpressionKind::HashMap(entries) =>
            {
                for (key, value) in entries
                {
                    exclude_expression(key, evaluate)?;
                    exclude_expression(value, evaluate)?;
                }
            },
        AstExpressionKind::StructConstructor(item) =>
            { for (_, _, value) in &mut item.fields { exclude_expression(value, evaluate)?; } },
        AstExpressionKind::Range(start, end, _) =>
            {
                for bound in [start, end].into_iter().flatten()
                { exclude_expression(bound, evaluate)?; }
            },
        AstExpressionKind::Index(left, right) | AstExpressionKind::MathExpression(_, left, right)
            | AstExpressionKind::BooleanExpression(_, left, right) =>
            {
                exclude_expression(left, evaluate)?;
                exclude_expression(right, evaluate)?;
            },
        AstExpressionKind::Field(value, _, _) | AstExpressionKind::Splat(value)
            | AstExpressionKind::Grouped(value) | AstExpressionKind::ExecutableReference(value)
            | AstExpressionKind::TryExecute(value) | AstExpressionKind::MathNegate(value)
            | AstExpressionKind::BooleanNot(value)
            | AstExpressionKind::TypeConversion(_, value, _, _) =>
            exclude_expression(value, evaluate)?,
        AstExpressionKind::Redirect(value, redirects) =>
            {
                exclude_expression(value, evaluate)?;
                for redirect in redirects { exclude_expression(&mut redirect.target, evaluate)?; }
            },
        AstExpressionKind::SpacedEmptyCall(_) | AstExpressionKind::EnumVariant(_, _)
            | AstExpressionKind::Variable(_) | AstExpressionKind::VariableSplat(_)
            | AstExpressionKind::Symbol(_) | AstExpressionKind::Literal(_) => {},
    }
    Ok(())
}


use std::collections::HashSet;

use crate::language::{ ast::*,
                       text::location::Location,
                       data::{ value::Value, conversion::is_conversion_method },
                       tokenizer::{ TokenBuffer, TokenKind, TokenValue, TokenLiteral },
                       parser::{ source::{ SyntaxContext, SyntaxRole },
                                 base_utils::{ Lookahead,
                                               expect_token,
                                               try_expect_token,
                                               expect_block_list_of },
                                 expressions::{ parse_expression,
                                                parse_redirection_tail,
                                                parse_exec_expression,
                                                parse_value_expression,
                                                parse_condition_expression,
                                                parse_command_arguments,
                                                parse_colon_word,
                                                expect_type_name,
                                                expect_field_label,
                                                parse_indexes,
                                                parse_parameter_type,
                                                parse_type },
                                 results::{ ParseResult, ParserError, ParserErrorKind } } };

fn expect_statement_end(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<()>
{
    let lookahead = Lookahead::new(buffer);
    if    let Some(token) = lookahead.buffer.next()?
       && !matches!(
            token.kind,
            TokenKind::LineBreak | TokenKind::StatementBreak | TokenKind::BlockClose
        )
    {
        return Err(ParserError
            {
                location: Some(token.location),
                kind: ParserErrorKind::ExpectedToken(TokenKind::StatementBreak, Some(token.kind))
            });
    }
    Ok(())
}

fn parse_try_statement(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstStatement>>
{
    let keyword = expect_token(buffer, TokenKind::Try)?;
    while try_expect_token(buffer, TokenKind::LineBreak)?.is_some()
    {}
    let body = parse_block(buffer)?;
    let mut catches = Vec::new();
    let mut finally = None;
    loop
    {
        let clause = {
            let mut probe = Lookahead::new(buffer);
            while try_expect_token(probe.buffer, TokenKind::LineBreak)?.is_some()
            {}
            let kind = probe.buffer.next()?.map(|token| token.kind);
            if matches!(kind, Some(TokenKind::Catch | TokenKind::Finally))
            {
                probe.commit();
            }
            kind
        };
        match clause
        {
            Some(TokenKind::Catch) =>
            {
                expect_token(buffer, TokenKind::ParenOpen)?;
                let first = buffer.next()?.ok_or(ParserError {
                    location: Some(keyword.location.clone()),
                    kind: ParserErrorKind::ExpectedExpression,
                })?;
                let first_name = first.token_value_text();
                if first.kind != TokenKind::Identifier && first_name != "_"
                {
                    return Err(ParserError {
                        location: Some(first.location),
                        kind: ParserErrorKind::InvalidType("Expected catch binding".into()),
                    });
                }
                buffer.annotate(first.span, SyntaxRole::VariableDeclaration);
                let typed = try_expect_token(buffer, TokenKind::TypeDelimiter)?.is_some();
                let stack_type = if typed
                {
                    false
                }
                else
                {
                    let probe = Lookahead::new(buffer);
                    probe
                        .buffer
                        .next()?
                        .is_some_and(|token| token.token_value_text() == "CallStack")
                };
                let annotation = if typed || stack_type
                {
                    Some(parse_type(buffer)?)
                }
                else
                {
                    None
                };
                let (stack, error, annotation) = if try_expect_token(buffer, TokenKind::Comma)?
                    .is_some()
                {
                    if annotation.as_ref().is_some_and(
                        |ty| !matches!(ty, AstType::Named(name) if name == "CallStack"),
                    )
                    {
                        return Err(ParserError {
                            location: Some(first.location),
                            kind: ParserErrorKind::InvalidType(
                                "The first catch binding must be CallStack".into(),
                            ),
                        });
                    }
                    let error = expect_token(buffer, TokenKind::Identifier)?;
                    buffer.annotate(error.span, SyntaxRole::VariableDeclaration);
                    let annotation = if try_expect_token(buffer, TokenKind::TypeDelimiter)?
                        .is_some()
                    {
                        Some(parse_type(buffer)?)
                    }
                    else
                    {
                        None
                    };
                    (
                        (first_name != "_").then_some(first_name),
                        error.token_value_text(),
                        annotation,
                    )
                }
                else
                {
                    if first_name == "_"
                    {
                        return Err(ParserError {
                            location: Some(first.location),
                            kind: ParserErrorKind::InvalidType(
                                "A catch needs an error binding".into(),
                            ),
                        });
                    }
                    (None, first_name, annotation)
                };
                expect_token(buffer, TokenKind::ParenClose)?;
                while try_expect_token(buffer, TokenKind::LineBreak)?.is_some()
                {}
                catches.push(AstCatch {
                    location: first.location,
                    stack,
                    error,
                    annotation,
                    type_id: None,
                    body: parse_block(buffer)?,
                });
            }
            Some(TokenKind::Finally) =>
            {
                while try_expect_token(buffer, TokenKind::LineBreak)?.is_some()
                {}
                finally = Some(parse_block(buffer)?);
                break;
            }
            _ => break,
        }
    }
    if catches.is_empty() && finally.is_none()
    {
        return Err(ParserError {
            location: Some(keyword.location),
            kind: ParserErrorKind::InvalidType("try needs catch or finally".into()),
        });
    }
    expect_statement_end(buffer)?;
    Ok(Some(AstStatement::TryStatement(Box::new(AstTry {
        location: keyword.location,
        body,
        catches,
        finally,
    }))))
}

fn parse_return_statement(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstStatement>>
{
    let keyword = expect_token(buffer, TokenKind::Return)?;
    let has_expression =
        {
            let lookahead = Lookahead::new(buffer);
            lookahead.buffer.next()?.is_some_and(|token|
                !matches!(token.kind,
                          TokenKind::LineBreak | TokenKind::StatementBreak | TokenKind::BlockClose))
        };

    let expression = if has_expression
        {
            Some(parse_value_expression(buffer)?.ok_or_else(|| ParserError
                {
                    location: Some(keyword.location.clone()),
                    kind: ParserErrorKind::ExpectedExpression
                })?)
        }
        else
        {
            None
        };

    expect_statement_end(buffer)?;
    Ok(Some(AstStatement::ReturnStatement(Box::new(AstReturnStatement
        {
            location: keyword.location,
            expression
        }))))
}


fn parse_let_statement(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstStatement>>
{
    let keyword = expect_token(buffer, TokenKind::Let)?;
    if try_expect_token(buffer, TokenKind::Type)?.is_some()
    {
        let identifier = expect_token(buffer, TokenKind::Identifier)?;
        buffer.annotate(identifier.span, SyntaxRole::Variable);
        expect_token(buffer, TokenKind::Assign)?;
        let annotation = parse_type(buffer)?;
        expect_statement_end(buffer)?;
        let name = identifier.token_value_text();
        let expression = AstExpression
            {
                location: identifier.location.clone(),
                kind: AstExpressionKind::Variable(AstSymbol { name: name.clone() }),
                interpolation_members: Default::default(),
                dispatch_type: Default::default(),
                string_flag: None,
            };
        let mut statement = new_ast_let_statement(identifier.location,
            AstExportFlag::NonExported, name, expression).unwrap();
        if let AstStatement::LetStatement(item) = &mut statement
        {
            item.annotation = Some(annotation);
            item.retype = true;
        }
        return Ok(Some(statement));
    }
    let mut lookahead = Lookahead::new(buffer);
    let discard = lookahead.buffer.next()?.is_some_and(|token|
        {
            let discard = token.kind == TokenKind::Symbol && token.token_value_text() == "_";
            if discard { lookahead.buffer.annotate(token.span, SyntaxRole::Discard); }
            discard
        });
    if discard { lookahead.commit(); }
    drop(lookahead);
    if discard
    {
        let assignment = expect_token(buffer, TokenKind::Assign)?;
        let expression = parse_value_expression(buffer)?.ok_or(ParserError
            {
                location: Some(assignment.location),
                kind: ParserErrorKind::ExpectedExpression,
            })?;
        expect_statement_end(buffer)?;
        return Ok(Some(AstStatement::DiscardStatement(expression)));
    }
    let export_flag = if try_expect_token(buffer, TokenKind::Export)?.is_some()
        { AstExportFlag::Exported } else { AstExportFlag::NonExported };
    if try_expect_token(buffer, TokenKind::ParenOpen)?.is_some()
    {
        let bindings = parse_binding_pattern(buffer, false)?;
        expect_token(buffer, TokenKind::Assign)?;
        let expression = parse_value_expression(buffer)?.ok_or_else(|| ParserError
            {
                location: Some(keyword.location.clone()),
                kind: ParserErrorKind::ExpectedExpression,
            })?;
        expect_statement_end(buffer)?;
        return Ok(Some(AstStatement::DestructureStatement(Box::new(AstDestructureStatement
            {
                public: false,
                location: keyword.location,
                export_flag,
                bindings,
                expression,
            }))));
    }
    let identifier = expect_token(buffer, TokenKind::Identifier)?;
    buffer.annotate(identifier.span, SyntaxRole::VariableDeclaration);
    if identifier.token_value_text().contains("::")
    {
        return Err(ParserError
            {
                location: Some(identifier.location),
                kind: ParserErrorKind::InvalidType(
                    "Declarations require an unqualified name".into()),
            });
    }
    let annotation = if try_expect_token(buffer, TokenKind::TypeDelimiter)?.is_some()
        { Some(parse_type(buffer)?) } else { None };
    let initialized = try_expect_token(buffer, TokenKind::Assign)?.is_some();
    if !initialized && annotation.is_none()
    {
        return Err(ParserError { location: Some(identifier.location),
            kind: ParserErrorKind::ExpectedToken(TokenKind::Assign, None) });
    }
    let expression = if initialized
        {
            parse_value_expression(buffer)?.ok_or_else(|| ParserError
                {
                    location: Some(identifier.location.clone()),
                    kind: ParserErrorKind::ExpectedExpression,
                })?
        }
        else { new_ast_literal(identifier.location.clone(), Value::None, None) };
    expect_statement_end(buffer)?;
    let mut statement = new_ast_let_statement(identifier.location.clone(), export_flag,
        identifier.token_value_text(), expression).unwrap();
    if let AstStatement::LetStatement(item) = &mut statement
    {
        item.annotation = annotation;
        item.default_initialize = !initialized;
    }
    Ok(Some(statement))
}


fn parse_set_statement(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstStatement>>
{
    let mut lookahead = Lookahead::new(buffer);
    let identifier = expect_token(&mut *lookahead.buffer, TokenKind::Identifier)?;
    let indexes = parse_indexes(&mut *lookahead.buffer)?;
    if try_expect_token(&mut *lookahead.buffer, TokenKind::Assign)?.is_none()
    {
        return Ok(None);
    }
    lookahead.commit();
    drop(lookahead);

    let expression = parse_value_expression(buffer)?;

    if let Some(expression) = expression
    {
        expect_statement_end(buffer)?;
        Ok(new_ast_set_statement(identifier.location.clone(),
                                 identifier.token_value_text(),
                                 indexes,
                                 expression))
    }
    else
    {
        Err(ParserError
            {
                location: Some(identifier.location.clone()),
                kind: ParserErrorKind::ExpectedExpression
            })
    }
}


fn parse_alias_statement(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstStatement>>
{
    expect_token(buffer, TokenKind::Alias)?;

    let alias = expect_token(buffer, TokenKind::Symbol)?;
    buffer.annotate(alias.span, SyntaxRole::AliasDeclaration);

    expect_token(buffer, TokenKind::Assign)?;

    let target = expect_token(buffer, TokenKind::Symbol)?;
    buffer.annotate(target.span, SyntaxRole::CommandName);

    let mut arguments = Vec::new();

    loop
    {
        if let Some(word) = buffer.with_context(SyntaxContext::Argument, parse_colon_word)?
        {
            let text = word.token_value_text();
            arguments.push(AstAliasArgument
                {
                    expand_path: text.starts_with('~'), value: Value::from_string(text),
                });
            continue;
        }
        let mut lookahead = Lookahead::new(buffer);
        let Some(token) = lookahead.buffer.with_context(SyntaxContext::Argument,
            |buffer| buffer.next())? else { break; };
        if token.kind == TokenKind::BlockClose { break; }
        lookahead.commit();
        drop(lookahead);

        match token.kind
        {
            TokenKind::LineBreak | TokenKind::StatementBreak => break,

            TokenKind::LineContinue =>
                {
                    expect_token(buffer, TokenKind::LineBreak)?;
                    continue;
                },

            _ => {}
        }

        if token.kind != TokenKind::Literal
        {
            buffer.with_context(SyntaxContext::Argument, |buffer|
                buffer.annotate(token.span, SyntaxRole::Argument));
        }

        // Alias arguments are stored values, not expressions to evaluate here.
        let value = match &token.value
            {
                TokenValue::Literal(TokenLiteral::Integer(value, _)) => Value::Integer(*value),
                TokenValue::Literal(TokenLiteral::Float(value, text)) =>
                    Value::Float(*value, Some(text.clone())),
                TokenValue::Literal(TokenLiteral::Boolean(value)) => Value::Boolean(*value),
                _ => Value::from_string(token.token_value_text())
            };

        arguments.push(AstAliasArgument
            {
                expand_path: token.kind != TokenKind::Literal
                    && token.token_value_text().starts_with('~'),
                value
            });
    }

    Ok(new_ast_alias_statement(alias.location.clone(),
                              alias.token_value_text(),
                              target.token_value_text(),
                              arguments))
}


fn parse_value_statement(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstStatement>>
{
    // The shared value rule already parses the complete command/pipeline tail.
    // Reuse that AST instead of speculatively parsing it, discarding it, and
    // parsing the same nested expressions again as a statement.
    let Some(expression) = parse_value_expression(buffer)? else { return Ok(None); };
    let expression = match expression
    {
        AstExpression { kind: AstExpressionKind::Execute(command), .. } =>
            return Ok(Some(AstStatement::ExecuteStatement(command))),
        AstExpression { kind: AstExpressionKind::TryExecute(command), .. } =>
        {
            // A bare word in statement position must execute, rather than use
            // the value rule's command-or-literal interpretation.
            let location = command.location.clone();
            let expand_path = matches!(&command.kind,
                AstExpressionKind::Symbol(symbol) if symbol.name.starts_with('~'));
            return Ok(new_ast_execute_statement(location, expand_path, *command, Vec::new()));
        }
        expression if matches!(expression.kind,
            AstExpressionKind::Pipeline(_) | AstExpressionKind::Redirect(_, _)) =>
            return Ok(Some(AstStatement::ExpressionStatement(expression))),
        expression => expression,
    };

    if matches!(expression.kind, AstExpressionKind::Symbol(_) | AstExpressionKind::VariableSplat(_)
        | AstExpressionKind::Splat(_))
    {
        return Ok(None);
    }

    let lookahead = Lookahead::new(buffer);
    if let Some(token) = lookahead.buffer.next()?
    {
        if !matches!(
            token.kind,
            TokenKind::LineBreak | TokenKind::StatementBreak | TokenKind::BlockClose
        )
        {
            return Ok(None);
        }
    }

    Ok(Some(AstStatement::ExpressionStatement(expression)))
}


fn parse_execute_statement(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstStatement>>
{
    let Some(exec_expression) = parse_exec_expression(buffer)? else { return Ok(None); };
    let location = exec_expression.location.clone();
    let parameter_expressions = parse_command_arguments(buffer)?;

    let redirected =
        {
            let peek = Lookahead::new(buffer);
            matches!(peek.buffer.next()?, Some(token) if matches!(token.kind,
                TokenKind::RedirectTo | TokenKind::RedirectErrorTo | TokenKind::RedirectBothTo))
        };
    if redirected
    {
        let source = if parameter_expressions.is_empty()
            { exec_expression }
            else
            {
                AstExpression
                    {
                        location: location.clone(),
                        interpolation_members: Default::default(),
                        dispatch_type: Default::default(),
                        string_flag: None,
                        kind: AstExpressionKind::Execute(Box::new(AstExecuteStatement
                            {
                                public: false,
                                location,
                                expand_path: matches!(&exec_expression.kind,
                                    AstExpressionKind::Symbol(symbol)
                                        if symbol.name.starts_with('~')),
                                executable: exec_expression,
                                arguments: parameter_expressions,
                            })),
                    }
            };
        return Ok(parse_redirection_tail(buffer, source)?
            .map(AstStatement::ExpressionStatement));
    }

    Ok(new_ast_execute_statement(location,
                            matches!(&exec_expression.kind,
                                AstExpressionKind::Symbol(symbol) if symbol.name.starts_with('~')),
                            exec_expression,
                            parameter_expressions))
}


fn parse_null_statement(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstStatement>>
{
    if let None = try_expect_token(buffer, TokenKind::LineBreak)?
    {
        if let None = try_expect_token(buffer, TokenKind::StatementBreak)?
        {
            return Ok(None);
        }
    }

    Ok(Some(AstStatement::NullStatement))
}


fn parse_function_statement(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstStatement>>
{
    expect_token(buffer, TokenKind::Function)?;
    let name = expect_token(buffer, TokenKind::Symbol)?;
    buffer.annotate(name.span, SyntaxRole::FunctionDeclaration);
    let (receiver, name) = if try_expect_token(buffer, TokenKind::Scope)?.is_some()
        {
            buffer.annotate(name.span, SyntaxRole::TypeName);
            let mut receiver = name.token_value_text();
            let mut member = expect_field_label(buffer)?;
            while try_expect_token(buffer, TokenKind::Scope)?.is_some()
            {
                buffer.annotate(member.span, SyntaxRole::TypeName);
                receiver.push_str("::");
                receiver.push_str(&member.token_value_text());
                member = expect_field_label(buffer)?;
            }
            buffer.annotate(member.span, SyntaxRole::FunctionDeclaration);
            (Some(receiver), member)
        }
        else { (None, name) };
    let function = parse_function_body(buffer, name.location.clone(),
                                       name.token_value_text(), receiver)?;
    Ok(Some(AstStatement::FunctionDefinition(Box::new(function))))
}

pub(super) fn parse_anonymous_function(buffer: &mut TokenBuffer<'_, '_>, location: Location)
    -> ParseResult<AstFunctionStatement>
{
    parse_function_body(buffer, location, "<anonymous>".into(), None)
}

fn parse_function_body(
    buffer: &mut TokenBuffer<'_, '_>,
    location: Location,
    name: String,
    receiver: Option<String>,
) -> ParseResult<AstFunctionStatement>
{
    expect_token(buffer, TokenKind::ParenOpen)?;
    let mut parameters = Vec::new();
    let mut names = HashSet::new();
    if receiver.is_some() { names.insert("$self".to_string()); }
    loop
    {
        while try_expect_token(buffer, TokenKind::LineBreak)?.is_some() {}
        if parameters.is_empty() && try_expect_token(buffer, TokenKind::ParenClose)?.is_some()
        {
            break;
        }
        let parameter = expect_token(buffer, TokenKind::Identifier)?;
        buffer.annotate(parameter.span, SyntaxRole::ParameterDeclaration);
        let parameter_name = parameter.token_value_text();
        let mut variadic = try_expect_token(buffer, TokenKind::Splat)?.is_some();
        if !names.insert(parameter_name.clone())
        {
            return Err(ParserError { location: Some(parameter.location),
                kind: ParserErrorKind::DuplicateParameter(parameter_name) });
        }
        let annotation = if try_expect_token(buffer, TokenKind::TypeDelimiter)?.is_some()
            {
                if variadic
                {
                    return Err(ParserError
                        {
                            location: Some(parameter.location),
                            kind: ParserErrorKind::InvalidType(
                                "Put '...' after the element type: $rest: Number...".to_string(),
                            ),
                        });
                }
                let (annotation, typed_variadic) = parse_parameter_type(buffer)?;
                variadic = typed_variadic;
                Some(if variadic { AstType::Array(Box::new(annotation)) } else { annotation })
            }
            else { None };
        parameters.push(AstParameter { location: parameter.location, name: parameter_name,
            annotation, optional: false, variadic, type_id: None });
        while try_expect_token(buffer, TokenKind::LineBreak)?.is_some() {}
        if try_expect_token(buffer, TokenKind::ParenClose)?.is_some() { break; }
        if variadic
        {
            return Err(ParserError
                {
                    location: Some(parameters.last().unwrap().location.clone()),
                    kind: ParserErrorKind::InvalidType(
                        "A variadic parameter must be last.".to_string(),
                    ),
                });
        }
        expect_token(buffer, TokenKind::Comma)?;
    }
    let return_annotation = if try_expect_token(buffer, TokenKind::TypeDelimiter)?.is_some()
        { Some(parse_type(buffer)?) } else { None };

    while try_expect_token(buffer, TokenKind::LineBreak)?.is_some() {}

    let code = expect_block_list_of(buffer,
                                   TokenKind::BlockOpen,
                                   TokenKind::BlockClose,
                                   None,
                                   &(parse_statement as fn(&mut TokenBuffer<'_, '_>)
                                     -> ParseResult<Option<AstStatement>>))?;

    let mut function = AstFunctionStatement
        {
            public: false,
            location: location.clone(), name, receiver, receiver_type: None,
            condition: None, parameters, return_annotation, return_type: None, body: code,
        };
    if    let Some(receiver) = &function.receiver
       && !is_conversion_method(&function.name) && function.name != "new"
       && function.name != "json_codec"
    {
        function.parameters.insert(0, AstParameter
            {
                location, name: "$self".to_string(),
                annotation: Some(AstType::Named(receiver.clone())),
                optional: false, variadic: false, type_id: None,
            });
    }
    Ok(function)
}


fn parse_block(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<AstBlockStatement>
{
    let location =
        {
            let peek = Lookahead::new(buffer);
            expect_token(peek.buffer, TokenKind::BlockOpen)?.location
        };
    let body = expect_block_list_of(buffer,
                                    TokenKind::BlockOpen,
                                    TokenKind::BlockClose,
                                    None,
                                    &(parse_statement as fn(&mut TokenBuffer<'_, '_>)
                                      -> ParseResult<Option<AstStatement>>))?;

    Ok(AstBlockStatement { location, body, condition: None })
}


pub(super) fn parse_match_expression(buffer: &mut TokenBuffer<'_, '_>,
                                     location: Location) -> ParseResult<AstExpression>
{
    let value = parse_condition_expression(buffer)?.ok_or_else(|| ParserError
        { location: Some(location.clone()), kind: ParserErrorKind::ExpectedExpression })?;
    while try_expect_token(buffer, TokenKind::LineBreak)?.is_some() {}
    expect_token(buffer, TokenKind::BlockOpen)?;
    let mut arms = Vec::new();
    let mut wildcard = false;
    loop
    {
        while try_expect_token(buffer, TokenKind::LineBreak)?.is_some() {}
        if try_expect_token(buffer, TokenKind::BlockClose)?.is_some() { break; }
        if wildcard
        {
            return Err(ParserError
                {
                    location: Some(location.clone()),
                    kind: ParserErrorKind::InvalidMatch(
                        "The wildcard match arm must be last.".to_string()),
                });
        }
        let pattern =
            {
                let mut peek = Lookahead::new(buffer);
                let bare_wildcard = peek.buffer.next()?.is_some_and(|token|
                    {
                        let wildcard = token.kind == TokenKind::Symbol
                            && token.token_value_text() == "_";
                        if wildcard { peek.buffer.annotate(token.span, SyntaxRole::Discard); }
                        wildcard
                    });
                if bare_wildcard
                {
                    wildcard = true;
                    peek.commit();
                    None
                }
                else
                {
                    drop(peek);
                    Some(parse_condition_expression(buffer)?.ok_or_else(|| ParserError
                        {
                            location: Some(location.clone()),
                            kind: ParserErrorKind::ExpectedExpression,
                        })?)
                }
            };
        expect_token(buffer, TokenKind::MatchArrow)?;
        while try_expect_token(buffer, TokenKind::LineBreak)?.is_some() {}
        let body = parse_block(buffer)?;
        arms.push(AstMatchArm { pattern, body });
        try_expect_token(buffer, TokenKind::Comma)?;
    }
    if arms.is_empty()
    {
        return Err(ParserError
            {
                location: Some(location),
                kind: ParserErrorKind::InvalidMatch(
                    "A match expression requires at least one arm.".to_string()),
            });
    }
    Ok(AstExpression
        {
            location: location.clone(),
            kind: AstExpressionKind::MatchExpression(Box::new(AstMatchExpression
                { location, value, arms })),
            interpolation_members: Default::default(),
            dispatch_type: Default::default(),
            string_flag: None,
        })
}


pub(super) fn parse_if_expression(buffer: &mut TokenBuffer<'_, '_>,
                                  location: Location) -> ParseResult<AstExpression>
{
    let mut branches = Vec::new();
    let mut else_body = None;
    let mut condition_location = location.clone();

    loop
    {
        let condition = parse_condition_expression(buffer)?.ok_or_else(|| ParserError
            {
                location: Some(condition_location.clone()),
                kind: ParserErrorKind::ExpectedExpression
            })?;
        while try_expect_token(buffer, TokenKind::LineBreak)?.is_some() {}
        let body = parse_block(buffer)?;
        branches.push(AstIfBranch { condition, body });

        // Newlines and comments may separate branches. Preserve them for the
        // enclosing statement parser if no else follows.
        let mut lookahead = Lookahead::new(buffer);
        while try_expect_token(&mut *lookahead.buffer, TokenKind::LineBreak)?.is_some() {}
        if try_expect_token(&mut *lookahead.buffer, TokenKind::Else)?.is_none() { break; }
        while try_expect_token(&mut *lookahead.buffer, TokenKind::LineBreak)?.is_some() {}
        let else_if = try_expect_token(&mut *lookahead.buffer, TokenKind::If)?;
        lookahead.commit();
        drop(lookahead);

        if let Some(keyword) = else_if
        {
            condition_location = keyword.location;
        }
        else
        {
            else_body = Some(parse_block(buffer)?);
            break;
        }
    }

    Ok(AstExpression
        {
            location: location.clone(),
            kind: AstExpressionKind::IfExpression(Box::new(AstIfExpression
                {
                    location,
                    branches,
                    else_body
                })),
            interpolation_members: Default::default(),
            dispatch_type: Default::default(),
            string_flag: None
        })
}


fn expect_loop_binding(buffer: &mut TokenBuffer<'_, '_>)
    -> ParseResult<(Location, Option<String>)>
{
    let mut lookahead = Lookahead::new(buffer);
    if    let Some(token) = try_expect_token(lookahead.buffer, TokenKind::Symbol)?
       && token.token_value_text() == "_"
    {
        lookahead.buffer.annotate(token.span, SyntaxRole::Discard);
        lookahead.commit();
        return Ok((token.location, None));
    }
    drop(lookahead);
    let token = expect_token(buffer, TokenKind::Identifier)?;
    buffer.annotate(token.span, SyntaxRole::VariableDeclaration);
    let name = token.token_value_text();
    Ok((token.location, Some(name)))
}


fn parse_binding_pattern(buffer: &mut TokenBuffer<'_, '_>, loop_binding: bool)
    -> ParseResult<Vec<(Location, Option<String>)>>
{
    let mut bindings: Vec<(Location, Option<String>)> = Vec::new();
    let mut names = HashSet::new();
    loop
    {
        while try_expect_token(buffer, TokenKind::LineBreak)?.is_some() {}
        let (location, name) = expect_loop_binding(buffer)?;
        if let Some(name) = &name
        {
            if name.contains("::")
            {
                return Err(ParserError
                    {
                        location: Some(location),
                        kind: ParserErrorKind::InvalidType(
                            "Declarations require an unqualified name".into()),
                    });
            }
            if !names.insert(name.clone())
            {
                return Err(ParserError
                    {
                        location: Some(location),
                        kind: if loop_binding
                            { ParserErrorKind::DuplicateLoopBinding(name.clone()) }
                            else { ParserErrorKind::DuplicateDestructureBinding(name.clone()) },
                    });
            }
        }
        bindings.push((location, name));
        while try_expect_token(buffer, TokenKind::LineBreak)?.is_some() {}
        if try_expect_token(buffer, TokenKind::ParenClose)?.is_some() { break; }
        expect_token(buffer, TokenKind::Comma)?;
        while try_expect_token(buffer, TokenKind::LineBreak)?.is_some() {}
        if try_expect_token(buffer, TokenKind::ParenClose)?.is_some() { break; }
    }
    Ok(bindings)
}


fn parse_for_statement(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstStatement>>
{
    let keyword = expect_token(buffer, TokenKind::For)?;
    let destructure = try_expect_token(buffer, TokenKind::ParenOpen)?.is_some();
    let bindings = if destructure
        { parse_binding_pattern(buffer, true)?.into_iter().map(|(_, name)| name).collect() }
        else
        {
            let (_, first) = expect_loop_binding(buffer)?;
            let mut bindings = vec![first];
            if try_expect_token(buffer, TokenKind::Comma)?.is_some()
            {
                let (location, name) = expect_loop_binding(buffer)?;
                if name.is_some() && name == bindings[0]
                {
                    return Err(ParserError
                        {
                            location: Some(location),
                            kind: ParserErrorKind::DuplicateLoopBinding(name.unwrap()),
                        });
                }
                bindings.push(name);
            }
            bindings
        };
    expect_token(buffer, TokenKind::In)?;
    let iterable = parse_condition_expression(buffer)?.ok_or_else(|| ParserError
        {
            location: Some(keyword.location.clone()),
            kind: ParserErrorKind::ExpectedExpression
        })?;
    while try_expect_token(buffer, TokenKind::LineBreak)?.is_some() {}
    let body = parse_block(buffer)?;
    expect_statement_end(buffer)?;
    Ok(Some(AstStatement::ForStatement(Box::new(AstForStatement
        {
            location: keyword.location,
            bindings,
            destructure,
            iterable,
            body
        }))))
}


fn parse_struct_statement(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstStatement>>
{
    let error = try_expect_token(buffer, TokenKind::Error)?.is_some();
    expect_token(buffer, TokenKind::Struct)?;
    let name = expect_type_name(buffer)?;
    buffer.annotate(name.span, SyntaxRole::TypeDeclaration);
    while try_expect_token(buffer, TokenKind::LineBreak)?.is_some() {}
    expect_token(buffer, TokenKind::BlockOpen)?;
    let mut fields = Vec::new();
    loop
    {
        while try_expect_token(buffer, TokenKind::LineBreak)?.is_some() {}
        if try_expect_token(buffer, TokenKind::BlockClose)?.is_some() { break; }
        let public = try_expect_token(buffer, TokenKind::Public)?.is_some();
        let field = expect_field_label(buffer)?;
        let field_name = field.token_value_text();
        let mut annotation = AstType::Named("any".to_string());
        if try_expect_token(buffer, TokenKind::TypeDelimiter)?.is_some()
        {
            annotation = parse_type(buffer)?;
        }
        fields.push(AstFieldDeclaration
            {
                public, name: field_name,
                annotation,
                location: field.location,
            });
        while try_expect_token(buffer, TokenKind::LineBreak)?.is_some() {}
        if try_expect_token(buffer, TokenKind::Comma)?.is_some() { continue; }
        expect_token(buffer, TokenKind::BlockClose)?;
        break;
    }
    expect_statement_end(buffer)?;
    Ok(Some(AstStatement::StructDeclaration(Box::new(AstStructDeclaration
        { public: false, error, location: name.location.clone(), name: name.token_value_text(),
          fields }))))
}


fn parse_type_statement(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstStatement>>
{
    expect_token(buffer, TokenKind::Type)?;
    let name = expect_type_name(buffer)?;
    buffer.annotate(name.span, SyntaxRole::TypeDeclaration);
    expect_token(buffer, TokenKind::Assign)?;
    let annotation = parse_type(buffer)?;
    expect_statement_end(buffer)?;
    Ok(Some(AstStatement::TypeDeclaration(Box::new(AstTypeDeclaration
        { public: false, location: name.location.clone(), name: name.token_value_text(),
          annotation, conversion: None }))))
}

fn parse_enum_statement(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstStatement>>
{
    let keyword = expect_token(buffer, TokenKind::Enum)?;
    let name = expect_type_name(buffer)?;
    buffer.annotate(name.span, SyntaxRole::TypeDeclaration);
    while try_expect_token(buffer, TokenKind::LineBreak)?.is_some() {}
    expect_token(buffer, TokenKind::BlockOpen)?;
    let mut variants = Vec::new();
    loop
    {
        while try_expect_token(buffer, TokenKind::LineBreak)?.is_some() {}
        if try_expect_token(buffer, TokenKind::BlockClose)?.is_some() { break; }
        let variant = expect_type_name(buffer)?;
        buffer.annotate(variant.span, SyntaxRole::EnumVariantDeclaration);
        variants.push((variant.token_value_text(), variant.location));
        while try_expect_token(buffer, TokenKind::LineBreak)?.is_some() {}
        if try_expect_token(buffer, TokenKind::Comma)?.is_some() { continue; }
        expect_token(buffer, TokenKind::BlockClose)?;
        break;
    }
    if variants.is_empty()
    {
        return Err(ParserError
            {
                location: Some(keyword.location),
                kind: ParserErrorKind::InvalidEnum(
                    "An enum requires at least one variant.".to_string(),
                ),
            });
    }
    expect_statement_end(buffer)?;
    Ok(Some(AstStatement::EnumDeclaration(Box::new(AstEnumDeclaration
        { public: false, location: name.location.clone(), name: name.token_value_text(),
          variants }))))
}


fn parse_import_statement(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstStatement>>
{
    let keyword = expect_token(buffer, TokenKind::Import)?;
    let token = expect_type_name(buffer)?;
    buffer.annotate(token.span, SyntaxRole::ModuleName);
    let mut module = token.token_value_text();
    let mut selection = false;
    while try_expect_token(buffer, TokenKind::Scope)?.is_some()
    {
        let lookahead = Lookahead::new(buffer);
        selection = lookahead.buffer.next()?.is_some_and(|token|
            token.kind == TokenKind::BlockOpen);
        drop(lookahead);
        if selection { break; }
        module.push_str("::");
        let token = expect_type_name(buffer)?;
        buffer.annotate(token.span, SyntaxRole::ModuleName);
        module.push_str(&token.token_value_text());
    }
    if module == "std"
    {
        return Err(ParserError { location: Some(token.location),
            kind: ParserErrorKind::InvalidImport("Expected a module after 'std::'".into()) });
    }
    let mut names = Vec::new();
    if selection
    {
        expect_token(buffer, TokenKind::BlockOpen)?;
        loop
        {
            while try_expect_token(buffer, TokenKind::LineBreak)?.is_some() {}
            if try_expect_token(buffer, TokenKind::BlockClose)?.is_some() { break; }
            let name = if let Some(token) = try_expect_token(buffer, TokenKind::Identifier)?
                { token } else { expect_type_name(buffer)? };
            buffer.annotate(name.span, SyntaxRole::ImportName);
            let text = name.token_value_text();
            if names.contains(&text)
            {
                return Err(ParserError { location: Some(name.location),
                    kind: ParserErrorKind::InvalidImport(format!("Duplicate import '{}'", text)) });
            }
            names.push(text);
            while try_expect_token(buffer, TokenKind::LineBreak)?.is_some() {}
            if try_expect_token(buffer, TokenKind::BlockClose)?.is_some() { break; }
            expect_token(buffer, TokenKind::Comma)?;
        }
        if names.is_empty()
        {
            return Err(ParserError { location: Some(keyword.location),
                kind: ParserErrorKind::InvalidImport("Import selection cannot be empty".into()) });
        }
    }
    let mut lookahead = Lookahead::new(buffer);
    let conditional = lookahead.buffer.next()?.is_some_and(|token|
        {
            let conditional = token.kind == TokenKind::Symbol && token.token_value_text() == "when";
            if conditional { lookahead.buffer.annotate(token.span, SyntaxRole::Keyword); }
            conditional
        });
    if conditional { lookahead.commit(); }
    drop(lookahead);
    let condition = if conditional
        {
            Some(parse_condition_expression(buffer)?.ok_or_else(|| ParserError
                {
                    location: Some(keyword.location.clone()),
                    kind: ParserErrorKind::ExpectedExpression,
                })?)
        }
        else { None };
    expect_statement_end(buffer)?;
    Ok(Some(AstStatement::ImportStatement(Box::new(AstImportStatement
        { public: false, location: keyword.location, module, names, condition, enabled: true }))))
}


fn parse_exclusion(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstStatement>>
{
    let mut lookahead = Lookahead::new(buffer);
    expect_token(lookahead.buffer, TokenKind::SquareOpen)?;
    let Some(keyword) = lookahead.buffer.next()? else { return Ok(None); };
    if keyword.kind != TokenKind::Symbol || keyword.token_value_text() != "when"
    { return Ok(None); }
    lookahead.buffer.annotate(keyword.span, SyntaxRole::Keyword);
    // Once [when is recognized, preserve errors instead of retrying it as an array.
    lookahead.commit();
    drop(lookahead);
    let condition = parse_condition_expression(buffer)?.ok_or_else(|| ParserError
        { location: Some(keyword.location.clone()), kind: ParserErrorKind::ExpectedExpression })?;
    expect_token(buffer, TokenKind::SquareClose)?;
    while try_expect_token(buffer, TokenKind::LineBreak)?.is_some() {}
    let kind =
        {
            let peek = Lookahead::new(buffer);
            peek.buffer.next()?.map(|token| token.kind)
        };
    let mut statement = match kind
        {
            Some(TokenKind::Function) => parse_function_statement(buffer)?.unwrap(),
            Some(TokenKind::Public) => parse_public_statement(buffer)?.unwrap(),
            Some(TokenKind::BlockOpen) =>
                AstStatement::BlockStatement(Box::new(parse_block(buffer)?)),
            None => return Err(ParserError { location: Some(keyword.location),
                kind: ParserErrorKind::UnexpectedEOF(TokenKind::Function) }),
            _ => return Err(ParserError { location: Some(keyword.location),
                kind: ParserErrorKind::InvalidExclusion(
                    "[when ...] must precede a function or a declaration block".into()) }),
        };
    match &mut statement
    {
        AstStatement::FunctionDefinition(function) => function.condition = Some(condition),
        AstStatement::BlockStatement(block) => block.condition = Some(condition),
        _ => return Err(ParserError { location: Some(keyword.location),
            kind: ParserErrorKind::InvalidExclusion(
                "[when ...] must precede a function or a declaration block".into()) }),
    }
    Ok(Some(statement))
}


fn parse_public_statement(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstStatement>>
{
    let keyword = expect_token(buffer, TokenKind::Public)?;
    let invalid = || ParserError { location: Some(keyword.location.clone()),
        kind: ParserErrorKind::InvalidVisibility(
            "pub must precede a named module declaration or import".into()) };
    let kind =
        {
            let peek = Lookahead::new(buffer);
            peek.buffer.next()?.map(|token| (token.kind, token.token_value_text()))
        };
    if matches!(&kind, Some((TokenKind::Symbol, name)) if name == "visible")
    {
        let visible = expect_token(buffer, TokenKind::Symbol)?;
        buffer.annotate(visible.span, SyntaxRole::CommandName);
        let arguments = parse_command_arguments(buffer)?;
        expect_statement_end(buffer)?;
        if arguments.len() != 1
        { return Err(ParserError { location: Some(keyword.location),
            kind: ParserErrorKind::InvalidVisibility(
                "pub visible expects one function reference or type name".into()) }); }
        let mut statement = new_ast_execute_statement(keyword.location.clone(), false,
            new_ast_symbol(keyword.location, "visible".into(), None), arguments).unwrap();
        if let AstStatement::ExecuteStatement(call) = &mut statement { call.public = true; }
        return Ok(Some(statement));
    }
    let kind = kind.map(|(kind, _)| kind);
    let mut statement = match kind
        {
            Some(TokenKind::Let) => parse_let_statement(buffer)?,
            Some(TokenKind::Function) => parse_function_statement(buffer)?,
            Some(TokenKind::Struct | TokenKind::Error) => parse_struct_statement(buffer)?,
            Some(TokenKind::Enum) => parse_enum_statement(buffer)?,
            Some(TokenKind::Type) => parse_type_statement(buffer)?,
            Some(TokenKind::Alias) => parse_alias_statement(buffer)?,
            Some(TokenKind::Import) => parse_import_statement(buffer)?,
            Some(TokenKind::SquareOpen) => parse_exclusion(buffer)?,
            _ => return Err(invalid()),
        }.ok_or_else(invalid)?;
    match &mut statement
    {
        AstStatement::LetStatement(item) => item.public = true,
        AstStatement::DestructureStatement(item) => item.public = true,
        AstStatement::FunctionDefinition(item) => item.public = true,
        AstStatement::StructDeclaration(item) => item.public = true,
        AstStatement::EnumDeclaration(item) => item.public = true,
        AstStatement::TypeDeclaration(item) => item.public = true,
        AstStatement::AliasStatement(item) => item.public = true,
        AstStatement::ImportStatement(item) => item.public = true,
        _ => return Err(invalid()),
    }
    Ok(Some(statement))
}


pub fn parse_statement(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstStatement>>
{
    buffer.with_context(SyntaxContext::Expression, |buffer|
        {
            if buffer.is_editor() { buffer.parse_editor_statement(parse_statement_inner) }
            else { parse_statement_inner(buffer) }
        })
}


fn parse_statement_inner(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstStatement>>
{
    // EOF is normal between statements. Rewind a real token so the statement rules see it, and
    // leave EOF within required syntax to those rules.

    let next_kind =
    {
        let lookahead = Lookahead::new(buffer);
        let Some(token) = lookahead.buffer.next()? else { return Ok(None); };
        token.kind
    };

    if matches!(next_kind, TokenKind::Branch | TokenKind::Pipe)
    {
        let token = buffer.next()?.unwrap();
        let layout = buffer.anchor(&token);
        let hint = if layout.hard_tabs
            { " Hard tabs occur here; add or check '#tab-size N' in the file header." }
            else { "" };
        return Err(ParserError
            {
                location: Some(token.location),
                kind: ParserErrorKind::InvalidPipeline(format!(
                    "Orphan pipeline connector: no connected parent pipe on the preceding row.{}",
                    hint)),
            });
    }

    if next_kind == TokenKind::Public { return parse_public_statement(buffer); }

    // A function's parameter list can also look like a parenthesized command argument. Once fn
    // starts a declaration, preserve its errors (including incomplete input) instead of falling
    // back to parsing it as a command.
    if    next_kind == TokenKind::SquareOpen
       && let Some(statement) = parse_exclusion(buffer)?
    { return Ok(Some(statement)); }

    if next_kind == TokenKind::Import { return parse_import_statement(buffer); }

    if matches!(next_kind, TokenKind::Struct | TokenKind::Error)
    {
        return parse_struct_statement(buffer);
    }
    if next_kind == TokenKind::Try
    {
        return parse_try_statement(buffer);
    }
    if next_kind == TokenKind::Throw
    {
        let keyword = expect_token(buffer, TokenKind::Throw)?;
        let value = parse_value_expression(buffer)?.ok_or(ParserError {
            location: Some(keyword.location),
            kind: ParserErrorKind::ExpectedExpression,
        })?;
        expect_statement_end(buffer)?;
        return Ok(Some(AstStatement::ThrowStatement(value)));
    }

    if next_kind == TokenKind::Enum { return parse_enum_statement(buffer); }
    if next_kind == TokenKind::Type { return parse_type_statement(buffer); }

    if next_kind == TokenKind::Function
    {
        let anonymous =
            {
                let probe = Lookahead::new(buffer);
                probe.buffer.next()?;
                matches!(probe.buffer.next()?, Some(token) if token.kind == TokenKind::ParenOpen)
            };
        if !anonymous { return parse_function_statement(buffer); }
    }

    if next_kind == TokenKind::Return
    {
        return parse_return_statement(buffer);
    }

    if next_kind == TokenKind::For { return parse_for_statement(buffer); }

    if next_kind == TokenKind::Loop
    {
        expect_token(buffer, TokenKind::Loop)?;
        while try_expect_token(buffer, TokenKind::LineBreak)?.is_some() {}
        let body = parse_block(buffer)?;
        expect_statement_end(buffer)?;
        return Ok(Some(AstStatement::LoopStatement(Box::new(body))));
    }

    if matches!(next_kind, TokenKind::Break | TokenKind::Continue)
    {
        let keyword = expect_token(buffer, next_kind)?;
        expect_statement_end(buffer)?;
        return Ok(Some(if next_kind == TokenKind::Break
            { AstStatement::BreakStatement(keyword.location) }
            else { AstStatement::ContinueStatement(keyword.location) }));
    }

    if matches!(next_kind, TokenKind::While | TokenKind::Until)
    {
        let keyword = expect_token(buffer, next_kind)?;
        let condition = parse_condition_expression(buffer)?.ok_or_else(|| ParserError
            {
                location: Some(keyword.location),
                kind: ParserErrorKind::ExpectedExpression
            })?;
        while try_expect_token(buffer, TokenKind::LineBreak)?.is_some() {}
        let body = parse_block(buffer)?;
        expect_statement_end(buffer)?;
        return Ok(Some(AstStatement::ConditionalLoopStatement(Box::new(AstConditionalLoopStatement
            {
                condition,
                body,
                until: next_kind == TokenKind::Until
            }))));
    }

    if next_kind == TokenKind::If
    {
        let expression = parse_expression(buffer)?.ok_or(ParserError
            {
                location: None,
                kind: ParserErrorKind::ExpectedExpression
            })?;
        expect_statement_end(buffer)?;
        return Ok(Some(AstStatement::ExpressionStatement(expression)));
    }
    if next_kind == TokenKind::Else
    {
        return Err(ParserError
            {
                location: Some(expect_token(buffer, TokenKind::Else)?.location),
                kind: ParserErrorKind::UnexpectedElse
            });
    }
    if next_kind == TokenKind::BlockOpen
    {
        return Ok(Some(AstStatement::BlockStatement(Box::new(parse_block(buffer)?))));
    }
    if next_kind == TokenKind::Let { return parse_let_statement(buffer); }
    if next_kind == TokenKind::Alias { return parse_alias_statement(buffer); }
    if next_kind == TokenKind::Identifier
    {
        if let Some(statement) = parse_set_statement(buffer)? { return Ok(Some(statement)); }
    }

    if let Some(statement) = parse_null_statement(buffer)? { return Ok(Some(statement)); }

    // A value mismatch can fall back to command syntax, but malformed expressions
    // must stay errors, including inside branches that will not be executed.
    {
        let mut lookahead = Lookahead::new(buffer);
        if let Some(statement) = parse_value_statement(&mut *lookahead.buffer)?
        {
            lookahead.commit();
            return Ok(Some(statement));
        }
    }
    parse_execute_statement(buffer)
}

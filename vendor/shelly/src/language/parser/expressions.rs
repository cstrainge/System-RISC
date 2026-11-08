
use std::{ cell::Cell, mem::replace };

use crate::language::{ ast::*,
                       data::{ value::Value, types::TypeRegistry },
                       tokenizer::{ TokenBuffer,
                                    TokenKind,
                                    TokenLiteral,
                                    TokenValue,
                                    StringFlag,
                                    Token },
                       parser::{ pipeline::parse_branches,
                                 source::{ SyntaxContext, SyntaxRole },
                                 base_utils::{ expect_token,
                                               match_one_of,
                                               Lookahead,
                                               try_expect_token,
                                               try_expect_one_of_tokens },
                                 results::{ ParseResult, ParserError, ParserErrorKind },
                                 statements::{ parse_if_expression, parse_match_expression,
                                               parse_anonymous_function } } };

pub(super) fn expect_type_name(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Token>
{
    let token = expect_token(buffer, TokenKind::Symbol)?;
    let name = token.token_value_text();
    if !valid_type_name(&name)
    {
        return Err(ParserError
            {
                location: Some(token.location),
                kind: ParserErrorKind::InvalidEnum(
                    "Expected an identifier containing letters, digits, or underscores."
                        .to_string(),
                ),
            });
    }
    Ok(token)
}

pub(super) fn valid_type_name(name: &str) -> bool
{
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_')
}


// Keep ambiguous dotted words intact until a known type supplies their receiver.
pub(crate) fn member_word_parts(name: &str) -> Option<(&str, Vec<&str>)>
{
    let (owner, members) = name.split_once('.')?;
    if !owner.split("::").all(valid_type_name) { return None; }
    let members: Vec<_> = members.split('.').collect();
    members.iter().all(|name| valid_type_name(name)).then_some((owner, members))
}


fn math_operator(kind: TokenKind) -> Option<(AstMathOperator, u8)>
{
    match kind
    {
        TokenKind::Plus     => Some((AstMathOperator::Add,      1)),
        TokenKind::Minus    => Some((AstMathOperator::Subtract, 1)),
        TokenKind::Asterisk => Some((AstMathOperator::Multiply, 2)),
        TokenKind::Slash    => Some((AstMathOperator::Divide,   2)),
        TokenKind::Percent  => Some((AstMathOperator::Modulo,   2)),
        _                   => None
    }
}


enum ConstructorStart
{
    Fields,
    TypeConversion,
    SpacedEmpty
}


fn try_qualified_scope(buffer: &mut TokenBuffer<'_, '_>, previous: &Token) -> ParseResult<bool>
{
    let mut lookahead = Lookahead::new(buffer);
    let Some(scope) = try_expect_token(lookahead.buffer, TokenKind::Scope)?
        else { return Ok(false); };
    let attached = scope.location.line == previous.location.line
        && scope.location.column
            == previous.location.column + previous.token_value_text().chars().count();
    if !attached
    {
        let peek = Lookahead::new(&mut *lookahead.buffer);
        if    let Some(next) = peek.buffer.next()?
           && next.location.line == scope.location.line
           && next.location.column == scope.location.column + 2
        {
            // A separate ::name begins an argument, rather than qualifying the preceding word.
            return Ok(false);
        }
    }
    lookahead.commit();
    Ok(true)
}


fn constructor_follows(
    buffer: &mut TokenBuffer<'_, '_>, name: &Token,
) -> ParseResult<Option<ConstructorStart>>
{
    let peek = Lookahead::new(buffer);
    let multiline = try_expect_token(&mut *peek.buffer, TokenKind::LineBreak)?.is_some();
    if multiline { skip_array_newlines(&mut *peek.buffer)?; }
    let Some(open) = try_expect_token(&mut *peek.buffer, TokenKind::ParenOpen)?
    else
    {
        return Ok(None);
    };
    let attached = !multiline && open.location.line == name.location.line
        && open.location.column == name.location.column + name.token_value_text().chars().count();
    if !multiline && (TypeRegistry::is_builtin_name(&name.token_value_text()) || attached)
    {
        let argument = Lookahead::new(&mut *peek.buffer);
        skip_array_newlines(&mut *argument.buffer)?;
        if let Some(first) = argument.buffer.next()?
        {
            let named_field = valid_type_name(&first.token_value_text())
                && try_expect_token(&mut *argument.buffer, TokenKind::TypeDelimiter)?.is_some();
            if first.kind != TokenKind::ParenClose && !named_field
            {
                return Ok(Some(ConstructorStart::TypeConversion));
            }
        }
    }
    if attached
    {
        return Ok(Some(ConstructorStart::Fields));
    }

    // Named fields distinguish spaced constructors from grouped command
    // arguments and new statements. Empty spaced constructors are resolved
    // against the type namespace during AST checking.
    skip_array_newlines(&mut *peek.buffer)?;
    let Some(first) = peek.buffer.next()? else { return Ok(None); };
    if first.kind == TokenKind::ParenClose
    {
        return Ok((!multiline && valid_type_name(&name.token_value_text()))
            .then_some(ConstructorStart::SpacedEmpty));
    }
    if first.kind != TokenKind::Identifier && !valid_type_name(&first.token_value_text())
    {
        return Ok(None);
    }
    Ok(try_expect_token(&mut *peek.buffer, TokenKind::TypeDelimiter)?
        .map(|_| ConstructorStart::Fields))
}


/**
 * Parse one arithmetic operand. A mismatch consumes nothing. Parentheses accept
 * any value expression, including a command whose result is used as the operand.
 */
fn parse_math_primary(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstExpression>>
{
    buffer.with_editor_depth(parse_math_primary_inner)
}


fn parse_math_primary_inner(buffer: &mut TokenBuffer<'_, '_>)
    -> ParseResult<Option<AstExpression>>
{
    let head = buffer.syntax_checkpoint();
    let mut lookahead = Lookahead::new(buffer);
    let Some(token) = lookahead.buffer.next()? else { return Ok(None); };

    let expression = match token.kind
        {
            TokenKind::Function =>
                {
                    let probe = Lookahead::new(&mut *lookahead.buffer);
                    if !matches!(probe.buffer.next()?, Some(token)
                        if token.kind == TokenKind::ParenOpen) { return Ok(None); }
                    drop(probe);
                    AstExpression
                        {
                            interpolation_members: Default::default(),
                            dispatch_type: Default::default(),
                            location: token.location.clone(), string_flag: None,
                            kind: AstExpressionKind::AnonymousFunction(Box::new(
                                parse_anonymous_function(&mut *lookahead.buffer, token.location)?)),
                        }
                },
            TokenKind::Symbol =>
                {
                    let mut name = token.token_value_text();
                    let mut tail = token.clone();
                    while try_qualified_scope(&mut *lookahead.buffer, &tail)?
                    {
                        if !name.split("::").all(valid_type_name)
                        {
                            return Err(ParserError
                                {
                                    location: Some(token.location.clone()),
                                    kind: ParserErrorKind::InvalidType(
                                        "Invalid qualified name".into()),
                                });
                        }
                        tail = expect_type_name(&mut *lookahead.buffer)?;
                        name.push_str("::");
                        name.push_str(&tail.token_value_text());
                    }
                    if let Some(start) = constructor_follows(&mut *lookahead.buffer, &tail)?
                    {
                        lookahead.buffer.annotate_names_since(head, SyntaxRole::TypeName);
                        if !name.split("::").all(valid_type_name)
                        {
                            return Err(ParserError
                                {
                                    location: Some(token.location),
                                    kind: ParserErrorKind::InvalidType(
                                        "Invalid struct name.".to_string()),
                                });
                        }
                        skip_array_newlines(&mut *lookahead.buffer)?;
                        expect_token(&mut *lookahead.buffer, TokenKind::ParenOpen)?;
                        if matches!(start, ConstructorStart::TypeConversion)
                        {
                            skip_array_newlines(&mut *lookahead.buffer)?;
                            let value = parse_collection_value(&mut *lookahead.buffer)?;
                            skip_array_newlines(&mut *lookahead.buffer)?;
                            try_expect_token(&mut *lookahead.buffer, TokenKind::Comma)?;
                            skip_array_newlines(&mut *lookahead.buffer)?;
                            expect_token(&mut *lookahead.buffer, TokenKind::ParenClose)?;
                            let expression = AstExpression
                                {
                                    location: token.location,
                                    kind: AstExpressionKind::TypeConversion(
                                        name, Box::new(value), None, Cell::new(None)),
                                    interpolation_members: Default::default(),
                                    dispatch_type: Default::default(),
                                    string_flag: None,
                                };
                            let expression = parse_postfix(&mut *lookahead.buffer, expression)?;
                            lookahead.commit();
                            return Ok(Some(expression));
                        }
                        let mut fields = Vec::new();
                        loop
                        {
                            skip_array_newlines(&mut *lookahead.buffer)?;
                            if try_expect_token(&mut *lookahead.buffer,
                                                TokenKind::ParenClose)?.is_some()
                            {
                                break;
                            }
                            let field = expect_field_label(&mut *lookahead.buffer)?;
                            expect_token(&mut *lookahead.buffer, TokenKind::TypeDelimiter)?;
                            skip_array_newlines(&mut *lookahead.buffer)?;
                            let value = parse_collection_value(&mut *lookahead.buffer)?;
                            fields.push((field.token_value_text(), field.location, value));
                            skip_array_newlines(&mut *lookahead.buffer)?;
                            if try_expect_token(&mut *lookahead.buffer,
                                                TokenKind::ParenClose)?.is_some()
                            {
                                break;
                            }
                            expect_token(&mut *lookahead.buffer, TokenKind::Comma)?;
                        }
                        let kind = match start
                            {
                                ConstructorStart::SpacedEmpty =>
                                    AstExpressionKind::SpacedEmptyCall(name),
                                ConstructorStart::Fields =>
                                    AstExpressionKind::StructConstructor(
                                        Box::new(AstStructConstructor
                                    {
                                raw: false, name, fields, type_id: None,
                                field_indexes: Vec::new(),
                            })),
                                ConstructorStart::TypeConversion => unreachable!(),
                            };
                        let expression = AstExpression
                            {
                                location: token.location,
                                kind,
                                interpolation_members: Default::default(),
                                dispatch_type: Default::default(),
                                string_flag: None,
                            };
                        let expression = parse_postfix(&mut *lookahead.buffer, expression)?;
                        lookahead.commit();
                        return Ok(Some(expression));
                    }
                    let Some((owner, member)) = name.rsplit_once("::")
                    else { return Ok(None); };
                    AstExpression
                        {
                            location: token.location,
                            kind: AstExpressionKind::EnumVariant(owner.into(), member.into()),
                            interpolation_members: Default::default(),
                            dispatch_type: Default::default(),
                            string_flag: None,
                        }
                },

            TokenKind::SquareOpen => AstExpression
                {
                    location: token.location,
                    kind: parse_collection(&mut *lookahead.buffer)?,
                    interpolation_members: Default::default(),
                    dispatch_type: Default::default(),
                    string_flag: None
                },

            TokenKind::If => parse_if_expression(
                &mut *lookahead.buffer, token.location)?,

            TokenKind::Match => parse_match_expression(
                &mut *lookahead.buffer, token.location)?,

            TokenKind::Literal =>
                {
                    let mut string_flag = None;

                    let value = match token.value
                        {
                            TokenValue::Literal(TokenLiteral::Integer(value, _)) =>
                                {
                                    Value::Integer(value)
                                },

                            TokenValue::Literal(TokenLiteral::Float(value, text)) =>
                                {
                                    Value::Float(value, Some(text))
                                },

                            TokenValue::Literal(TokenLiteral::Boolean(value)) =>
                                Value::Boolean(value),

                            TokenValue::Literal(TokenLiteral::String(value, flag,
                                escaped_dollars)) =>
                                {
                                    string_flag = Some(match flag
                                        {
                                            StringFlag::Interpolated =>
                                                AstStringFlag::Interpolated(escaped_dollars),

                                            StringFlag::NonInterpolated =>
                                                AstStringFlag::NonInterpolated,
                                        });

                                    Value::from_string(value)
                                },

                            _ => return Ok(None)
                        };

                    new_ast_literal(token.location, value, string_flag)
                },

            TokenKind::Not =>
                {
                    let operand = match parse_math_primary(&mut *lookahead.buffer)?
                        {
                            Some(operand) => operand,
                            None => parse_scalar_expression(&mut *lookahead.buffer)?
                                .ok_or_else(|| ParserError
                                    {
                                        location: Some(token.location.clone()),
                                        kind: ParserErrorKind::ExpectedExpression
                                    })?
                        };
                    AstExpression
                        {
                            location: token.location,
                            kind: AstExpressionKind::BooleanNot(Box::new(operand)),
                            interpolation_members: Default::default(),
                            dispatch_type: Default::default(),
                            string_flag: None
                        }
                },

            TokenKind::Minus =>
                {
                    // A standalone '-' remains a command argument. With an operand it is unary
                    // negation, preserving a named numeric operand's type.
                    let Some(operand) = parse_math_primary(&mut *lookahead.buffer)? else
                    {
                        return Ok(None);
                    };
                    AstExpression
                        {
                            location: token.location.clone(),
                            kind: AstExpressionKind::MathNegate(Box::new(operand)),
                            interpolation_members: Default::default(),
                            dispatch_type: Default::default(),
                            string_flag: None
                        }
                },

            TokenKind::AutoIdentifier => new_ast_variable(token.location, "$".into()),

            TokenKind::Identifier =>
                {
                    let TokenValue::Identifier(name) = token.value else { return Ok(None); };

                    // Argument expansion is not a scalar arithmetic operand.
                    if try_expect_token(&mut *lookahead.buffer, TokenKind::Splat)?.is_some()
                    {
                        return Ok(None);
                    }

                    new_ast_variable(token.location, name)
                },

            TokenKind::ParenOpen =>
                {
                    if try_expect_token(&mut *lookahead.buffer, TokenKind::ParenClose)?.is_some()
                    {
                        let expression = parse_postfix(&mut *lookahead.buffer,
                            new_ast_literal(token.location, Value::None, None))?;
                        lookahead.commit();
                        return Ok(Some(expression));
                    }

                    let expression = parse_value_expression(&mut *lookahead.buffer)?
                        .ok_or_else(|| ParserError
                            {
                                location: Some(token.location.clone()),
                                kind: ParserErrorKind::ExpectedExpression
                            })?;
                    expect_token(&mut *lookahead.buffer, TokenKind::ParenClose)?;
                    AstExpression
                        {
                            location: token.location,
                            kind: AstExpressionKind::Grouped(Box::new(expression)),
                            interpolation_members: Default::default(),
                            dispatch_type: Default::default(),
                            string_flag: None
                        }
                },

            _ => return Ok(None)
        };

    let expression = parse_postfix(&mut *lookahead.buffer, expression)?;
    lookahead.commit();
    Ok(Some(expression))
}


fn skip_array_newlines(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<()>
{
    while try_expect_token(buffer, TokenKind::LineBreak)?.is_some() {}
    Ok(())
}


pub fn parse_indexes(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Vec<AstAccess>>
{
    let mut indexes = Vec::new();
    loop
    {
        if let Some(field) = try_expect_token(buffer, TokenKind::Member)?
        {
            indexes.push(AstAccess::Field(field.token_value_text()));
        }
        else if let Some(open) = try_expect_token(buffer, TokenKind::IndexOpen)?
        {
            skip_array_newlines(buffer)?;
            let index = parse_value_expression(buffer)?.ok_or_else(|| ParserError
                { location: Some(open.location), kind: ParserErrorKind::ExpectedExpression })?;
            skip_array_newlines(buffer)?;
            expect_token(buffer, TokenKind::SquareClose)?;
            indexes.push(AstAccess::Index(index));
        }
        else { break; }
    }
    Ok(indexes)
}


fn parse_postfix(
    buffer: &mut TokenBuffer<'_, '_>, mut expression: AstExpression,
) -> ParseResult<AstExpression>
{
    for access in parse_indexes(buffer)?
    {
        expression = AstExpression
            {
                location: expression.location.clone(),
                kind: match access
                    {
                        AstAccess::Index(index) =>
                            AstExpressionKind::Index(Box::new(expression), Box::new(index)),
                        AstAccess::Field(name) =>
                            AstExpressionKind::Field(Box::new(expression), name, None)
                    },
                interpolation_members: Default::default(),
                dispatch_type: Default::default(),
                string_flag: None
            };
    }
    if let Some(splat) = try_expect_token(buffer, TokenKind::Splat)?
    {
        expression = AstExpression { location: splat.location,
            interpolation_members: Default::default(),
            dispatch_type: Default::default(),
            kind: AstExpressionKind::Splat(Box::new(expression)), string_flag: None };
    }
    Ok(expression)
}


pub(super) fn expect_field_label(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Token>
{
    let token = buffer.next()?.ok_or(ParserError { location: None,
        kind: ParserErrorKind::UnexpectedEOF(TokenKind::Symbol) })?;
    if    !valid_type_name(&token.token_value_text())
       || token.kind == TokenKind::Identifier
       || token.kind == TokenKind::Literal
       && !matches!(token.value, TokenValue::Literal(TokenLiteral::Boolean(_)))
    {
        return Err(ParserError { location: Some(token.location),
            kind: ParserErrorKind::InvalidType("Expected a field name.".to_string()) });
    }
    buffer.annotate(token.span, SyntaxRole::FieldName);
    Ok(token)
}


pub(super) fn parse_type(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<AstType>
{
    buffer.with_context(SyntaxContext::Type, |buffer|
        parse_type_inner(buffer, false, &mut false))
}


pub(super) fn parse_parameter_type(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<(AstType, bool)>
{
    let mut variadic = false;
    let annotation = buffer.with_context(SyntaxContext::Type, |buffer|
        parse_type_inner(buffer, true, &mut variadic))?;
    if !variadic && try_expect_token(buffer, TokenKind::Splat)?.is_some() { variadic = true; }
    Ok((annotation, variadic))
}


fn parse_type_inner(
    buffer: &mut TokenBuffer<'_, '_>, allow_variadic: bool, variadic: &mut bool,
) -> ParseResult<AstType>
{
    buffer.with_editor_depth(|buffer| parse_type_inner_impl(buffer, allow_variadic, variadic))
}

fn parse_type_inner_impl(
    buffer: &mut TokenBuffer<'_, '_>, allow_variadic: bool, variadic: &mut bool,
) -> ParseResult<AstType>
{
    let first = parse_type_atom(buffer, allow_variadic, variadic)?;
    let mut members = vec![first];
    while try_expect_token(buffer, TokenKind::Pipe)?.is_some()
    {
        members.push(parse_type_atom(buffer, allow_variadic, variadic)?);
    }
    if members.len() == 1 { Ok(members.pop().unwrap()) }
    else { Ok(AstType::Union(members)) }
}

fn parse_type_atom(
    buffer: &mut TokenBuffer<'_, '_>, allow_variadic: bool, variadic: &mut bool,
) -> ParseResult<AstType>
{
    if let Some(receiver) = try_expect_token(buffer, TokenKind::Identifier)?
    {
        let member = expect_token(buffer, TokenKind::Member)?;
        let name = member.token_value_text();
        if    receiver.token_value_text() != "$self"
           || !matches!(name.as_str(), "key_type" | "value_type")
        {
            return Err(ParserError { location: Some(receiver.location),
                kind: ParserErrorKind::InvalidType(
                    "Expected $self.key_type or $self.value_type".into()) });
        }
        return Ok(AstType::ReceiverMember(name));
    }
    if try_expect_token(buffer, TokenKind::Function)?.is_some()
    {
        expect_token(buffer, TokenKind::ParenOpen)?;
        let mut parameters = Vec::new();
        skip_array_newlines(buffer)?;
        if try_expect_token(buffer, TokenKind::ParenClose)?.is_none()
        {
            loop
            {
                parameters.push(parse_type(buffer)?);
                skip_array_newlines(buffer)?;
                if try_expect_token(buffer, TokenKind::Comma)?.is_none()
                {
                    expect_token(buffer, TokenKind::ParenClose)?;
                    break;
                }
                skip_array_newlines(buffer)?;
                if try_expect_token(buffer, TokenKind::ParenClose)?.is_some() { break; }
            }
        }
        let result = if try_expect_token(buffer, TokenKind::TypeDelimiter)?.is_some()
            { parse_type(buffer)? } else { AstType::Named("None".into()) };
        return Ok(AstType::Function(parameters, Box::new(result)));
    }
    if try_expect_token(buffer, TokenKind::ParenOpen)?.is_some()
    {
        if try_expect_token(buffer, TokenKind::ParenClose)?.is_some()
        { return Ok(AstType::Named("None".into())); }
        let annotation = parse_type(buffer)?;
        expect_token(buffer, TokenKind::ParenClose)?;
        return Ok(annotation);
    }
    if try_expect_token(buffer, TokenKind::SquareOpen)?.is_some()
    {
        skip_array_newlines(buffer)?;
        let first = parse_type(buffer)?;
        skip_array_newlines(buffer)?;
        let result = if try_expect_token(buffer, TokenKind::TypeDelimiter)?.is_some()
            {
                skip_array_newlines(buffer)?;
                AstType::Map(Box::new(first), Box::new(parse_type(buffer)?))
            }
            else if try_expect_token(buffer, TokenKind::Comma)?.is_some()
            {
                let mut items = vec![first];
                loop
                {
                    skip_array_newlines(buffer)?;
                    if try_expect_token(buffer, TokenKind::SquareClose)?.is_some()
                    { return Ok(AstType::FixedArray(items)); }
                    items.push(parse_type(buffer)?);
                    skip_array_newlines(buffer)?;
                    if try_expect_token(buffer, TokenKind::Comma)?.is_none() { break; }
                }
                AstType::FixedArray(items)
            }
            else { AstType::Array(Box::new(first)) };
        skip_array_newlines(buffer)?;
        expect_token(buffer, TokenKind::SquareClose)?;
        return Ok(result);
    }
    let token = expect_token(buffer, TokenKind::Symbol)?;
    let mut name = token.token_value_text();
    // Shell words retain dots, so recognize an attached type suffix here,
    // without changing tokenization of ordinary command arguments like foo....
    if allow_variadic && let Some(element) = name.strip_suffix("...")
    {
        name = element.to_string();
        *variadic = true;
    }
    if !valid_type_name(&name)
    {
        return Err(ParserError { location: Some(token.location),
            kind: ParserErrorKind::InvalidType("Expected a type name.".to_string()) });
    }
    while try_expect_token(buffer, TokenKind::Scope)?.is_some()
    {
        name.push_str("::");
        name.push_str(&expect_type_name(buffer)?.token_value_text());
    }
    Ok(AstType::Named(name))
}


fn parse_collection_value(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<AstExpression>
{
    parse_value_before_block(buffer, false, true)?.ok_or_else(|| ParserError
        {
            location: None,
            kind: ParserErrorKind::ExpectedExpression
        })
}


fn parse_collection(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<AstExpressionKind>
{
    skip_array_newlines(buffer)?;
    if try_expect_token(buffer, TokenKind::SquareClose)?.is_some()
    {
        return Ok(AstExpressionKind::Array(Vec::new()));
    }
    // [] remains an array; [:] denotes an empty map.
    if try_expect_token(buffer, TokenKind::TypeDelimiter)?.is_some()
    {
        skip_array_newlines(buffer)?;
        expect_token(buffer, TokenKind::SquareClose)?;
        return Ok(AstExpressionKind::HashMap(Vec::new()));
    }

    let first = parse_collection_value(buffer)?;
    skip_array_newlines(buffer)?;
    let is_map = try_expect_token(buffer, TokenKind::TypeDelimiter)?.is_some();
    let mut elements = Vec::new();
    let mut pairs = Vec::new();
    let mut item = first;
    loop
    {
        if is_map
        {
            skip_array_newlines(buffer)?;
            pairs.push((item, parse_collection_value(buffer)?));
        }
        else
        {
            elements.push(item);
        }
        skip_array_newlines(buffer)?;
        if try_expect_token(buffer, TokenKind::SquareClose)?.is_some() { break; }
        expect_token(buffer, TokenKind::Comma)?;
        skip_array_newlines(buffer)?;
        if try_expect_token(buffer, TokenKind::SquareClose)?.is_some() { break; }
        item = parse_collection_value(buffer)?;
        if is_map
        {
            skip_array_newlines(buffer)?;
            expect_token(buffer, TokenKind::TypeDelimiter)?;
        }
    }
    Ok(if is_map { AstExpressionKind::HashMap(pairs) } else { AstExpressionKind::Array(elements) })
}


/**
 * Extend a left operand using precedence climbing. The higher right-hand
 * threshold makes equal-precedence operators left-associative.
 */
fn parse_math_binary_tail(buffer: &mut TokenBuffer<'_, '_>,
                          mut left: AstExpression,
                          min_precedence: u8,
                          allow_glob_fallback: bool) -> ParseResult<AstExpression>
{
    let mut matched_operator = false;

    loop
    {
        // Keep the operator speculative until its right operand has parsed.
        // EOF, statement terminators, closing parentheses, **, and other
        // non-operators remain unread for the enclosing parser.
        let mut lookahead = Lookahead::new(buffer);
        let Some(token) = lookahead.buffer.next()? else { break; };

        if matches!(
            token.kind,
            TokenKind::LineBreak | TokenKind::StatementBreak | TokenKind::ParenClose
        )
        {
            break;
        }

        let Some((operator, precedence)) = math_operator(token.kind) else { break; };

        if precedence < min_precedence
        {
            break;
        }

        let right = match parse_math_primary(&mut *lookahead.buffer)?
            {
                Some(right) => right,
                None =>
                    {
                        if    allow_glob_fallback
                           && !matched_operator
                           && matches!(operator, AstMathOperator::Multiply)
                        {
                            break;
                        }

                        lookahead.buffer.capture_editor_expected();
                        return Err(ParserError
                            {
                                location: Some(token.location),
                                kind: ParserErrorKind::ExpectedExpression
                            });
                    }
            };

        let right = parse_math_binary_tail(&mut *lookahead.buffer, right, precedence + 1, false)?;

        left = AstExpression
            {
                location: left.location.clone(),
                kind: AstExpressionKind::MathExpression(operator, Box::new(left), Box::new(right)),
                interpolation_members: Default::default(),
                dispatch_type: Default::default(),
                string_flag: None
            };

        lookahead.commit();
        matched_operator = true;
    }

    Ok(left)
}


fn parse_math_expression(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstExpression>>
{
    let mut lookahead = Lookahead::new(buffer);
    let starts_with_group =
        {
            let peek = Lookahead::new(&mut *lookahead.buffer);
            matches!(peek.buffer.next()?, Some(token) if token.kind == TokenKind::ParenOpen)
        };

    let Some(left) = parse_math_primary(&mut *lookahead.buffer)? else { return Ok(None); };
    let expression = parse_math_binary_tail(&mut *lookahead.buffer, left, 0, !starts_with_group)?;

    // A successfully parsed scalar already has its complete postfix and
    // arithmetic tail. Keep it even without an arithmetic operator, rather
    // than discard its AST and parse the same variable/literal again. The
    // executable rule retains its own lookahead and executable-kind filter.
    lookahead.commit();
    Ok(Some(expression))
}


fn parse_variable_expression(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstExpression>>
{
    if let Some(token) = try_expect_token(buffer, TokenKind::AutoIdentifier)?
    { return Ok(Some(parse_postfix(buffer, new_ast_variable(token.location, "$".into()))?)); }
    let identifier = expect_token(buffer, TokenKind::Identifier)?;

    let identifier_value = match identifier.value
        {
            TokenValue::Identifier(identifier) => identifier,

            _ => return Err(ParserError
                {
                    location: Some(identifier.location.clone()),
                    kind: ParserErrorKind::ExpectedExpression,
                })
        };

    // Check for the splat operator, `...`, which may follow the variable.
    let splat_operator = try_expect_token(buffer, TokenKind::Splat)?;

    if let Some(_) = splat_operator
    {
        Ok(Some(new_ast_variable_splat(identifier.location.clone(), identifier_value)))
    }
    else
    {
        Ok(Some(parse_postfix(buffer,
            new_ast_variable(identifier.location.clone(), identifier_value))?))
    }
}


fn parse_outer_expression(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstExpression>>
{
    let Some(operator) = try_expect_token(buffer, TokenKind::Scope)? else { return Ok(None); };
    let name = buffer.next()?.ok_or_else(|| ParserError
        {
            location: Some(operator.location.clone()),
            kind: ParserErrorKind::UnexpectedEOF(TokenKind::Symbol),
        })?;
    buffer.annotate(name.span, SyntaxRole::CommandName);
    let text = name.token_value_text();
    // Keywords and Boolean literals can also name external programs, such as true.
    if    name.kind != TokenKind::Symbol
       && !valid_type_name(&text)
    {
        return Err(ParserError
            {
                location: Some(name.location),
                kind: ParserErrorKind::ExpectedToken(TokenKind::Symbol, Some(name.kind)),
            });
    }
    let string_flag = if text.contains('$')
        { AstStringFlag::Interpolated(Vec::new()) } else { AstStringFlag::NonInterpolated };
    Ok(Some(new_ast_symbol(operator.location, format!("::{}", text), Some(string_flag))))
}


fn parse_symbol_expression(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstExpression>>
{
    let symbol = expect_token(buffer, TokenKind::Symbol)?;

    let symbol_value = match symbol.value
        {
            TokenValue::Symbol(symbol) => symbol,

            _ => return Err(ParserError
                {
                    location: Some(symbol.location.clone()),
                    kind: ParserErrorKind::ExpectedExpression,
                })
        };

    // Does the symbol have any $s in it?
    let string_flag = if symbol_value.contains('$')
        {
            Some(AstStringFlag::Interpolated(Vec::new()))
        }
        else
        {
            Some(AstStringFlag::NonInterpolated)
        };

    Ok(Some(new_ast_symbol(symbol.location.clone(), symbol_value, string_flag)))
}


fn parse_literal_expression(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstExpression>>
{
    let literal = expect_token(buffer, TokenKind::Literal)?;

    let literal_value = match literal.value
        {
            TokenValue::Literal(literal) => literal,

            _ => return Err(ParserError
                {
                    location: Some(literal.location.clone()),
                    kind: ParserErrorKind::ExpectedExpression,
                })
        };

    let mut string_flag = None;

    let literal_value = match literal_value
        {
            TokenLiteral::Integer(value, _) => Value::Integer(value),
            TokenLiteral::Float(value, text) => Value::Float(value, Some(text)),
            TokenLiteral::Boolean(value) => Value::Boolean(value),
            TokenLiteral::String(value, flag, escaped_dollars) =>
                {
                    string_flag = match flag
                        {
                            StringFlag::Interpolated =>
                                Some(AstStringFlag::Interpolated(escaped_dollars)),
                            StringFlag::NonInterpolated => Some(AstStringFlag::NonInterpolated),
                        };
                    Value::from_string(value)
                }
        };

    Ok(Some(new_ast_literal(literal.location.clone(), literal_value, string_flag)))
}


fn parse_lonely_glob_expression(
    buffer: &mut TokenBuffer<'_, '_>,
) -> ParseResult<Option<AstExpression>>
{
    let glob_operator = try_expect_token(buffer, TokenKind::Glob)?;

    if let Some(glob_operator) = glob_operator
    {
        buffer.annotate_word(glob_operator.span);
        let location = glob_operator.location.clone();
        return Ok(Some(new_ast_symbol(location, "*".to_string(), None)));
    }

    let star = expect_token(buffer, TokenKind::Asterisk)?;
    buffer.annotate_word(star.span);

    Ok(Some(new_ast_symbol(star.location.clone(), "*".to_string(), None)))
}


fn parse_operator_to_symbol(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstExpression>>
{
    let allowed_operators = [ TokenKind::Let,
                              TokenKind::Export,
                              TokenKind::Public,
                              TokenKind::Alias,
                              TokenKind::Sub,
                              TokenKind::If,
                              TokenKind::Else,
                              TokenKind::While,
                              TokenKind::Until,
                              TokenKind::For,
                              TokenKind::In,
                              TokenKind::Break,
                              TokenKind::Continue,
                              TokenKind::Loop,
                              TokenKind::Match,
                              TokenKind::Return,
                              TokenKind::Function,
                              TokenKind::Struct,
                              TokenKind::Enum,
                              TokenKind::Type,
                              TokenKind::Import,
                              TokenKind::TypeDelimiter,
                              TokenKind::Assign,
                              TokenKind::Minus,
                              TokenKind::Plus,
                              TokenKind::Slash,
                              TokenKind::Percent,
                              TokenKind::Scope,
                              TokenKind::ErrorSource ];

    // Attempt to match one of the allowed operators
    if let Some(operator) = try_expect_one_of_tokens(buffer, &allowed_operators)?
    {
        buffer.annotate_word(operator.span);
        let location = operator.location.clone();

        return Ok(Some(new_ast_symbol(location, operator.token_value_text(), None)));
    }

    Ok(None)
}


fn parse_scalar_expression(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstExpression>>
{
    // Bare executable names remain literal references. Preserve possible type members
    // for the checker to resolve without turning the reference into a call.
    if let Some(escape) = try_expect_token(buffer, TokenKind::ExecEscape)?
    {
        // An escaped name remains a separate argument even when followed by
        // spaced empty parentheses; it cannot name a struct constructor.
        {
            let mut peek = Lookahead::new(buffer);
            if    let Some(name) = peek.buffer.next()?
               && name.kind == TokenKind::Symbol
               && matches!(
                    constructor_follows(&mut *peek.buffer, &name)?,
                    Some(ConstructorStart::SpacedEmpty)
                )
            {
                peek.buffer.annotate(name.span, SyntaxRole::CommandName);
                peek.commit();
                return Ok(Some(new_ast_literal(escape.location,
                    Value::from_executable_string(name.token_value_text()), None)));
            }
        }
        let executable = parse_exec_expression(buffer)?.ok_or_else(|| ParserError
            {
                location: Some(escape.location.clone()),
                kind: ParserErrorKind::ExpectedExpression
            })?;

        if    matches!(executable.kind,
                       AstExpressionKind::Variable(_)
                           | AstExpressionKind::Index(_, _)
                           | AstExpressionKind::Field(_, _, _)
                           | AstExpressionKind::EnumVariant(_, _))
           || matches!(&executable.kind, AstExpressionKind::Symbol(symbol)
                if member_word_parts(&symbol.name).is_some())
        {
            return Ok(Some(AstExpression
                {
                    location: escape.location,
                    kind: AstExpressionKind::ExecutableReference(Box::new(executable)),
                    interpolation_members: Default::default(),
                    dispatch_type: Default::default(),
                    string_flag: None,
                }));
        }

        return Ok(Some(new_ast_literal(escape.location,
                                       Value::from_executable_string(executable.resolve_as_text()?),
                                       None)));
    }

    // The math rule itself decides when to fall back. Once it reports malformed
    // arithmetic, do not retry it as separate literal/variable/glob arguments.
    if let Some(expression) = parse_outer_expression(buffer)?
    {
        return Ok(Some(expression));
    }
    if let Some(expression) = parse_math_expression(buffer)?
    {
        return Ok(Some(expression));
    }

    match_one_of(buffer, &[parse_variable_expression,
                           parse_symbol_expression,
                           parse_literal_expression,
                           parse_lonely_glob_expression,
                           parse_operator_to_symbol])
}


fn parse_boolean_tail(buffer: &mut TokenBuffer<'_, '_>,
                      mut left: AstExpression,
                      min_precedence: u8) -> ParseResult<AstExpression>
{
    loop
    {
        let mut lookahead = Lookahead::new(buffer);
        // Newlines belong to this expression only if an operator follows. Otherwise
        // dropping the lookahead preserves the next statement or block boundary.
        skip_array_newlines(&mut *lookahead.buffer)?;
        let Some(token) = lookahead.buffer.next()? else { break; };
        let (operator, precedence) = match token.kind
            {
                TokenKind::Or => (AstBooleanOperator::Or, 1),
                TokenKind::And => (AstBooleanOperator::And, 2),
                TokenKind::Equal => (AstBooleanOperator::Equal, 3),
                TokenKind::NotEqual => (AstBooleanOperator::NotEqual, 3),
                TokenKind::Less => (AstBooleanOperator::Less, 4),
                TokenKind::LessEqual => (AstBooleanOperator::LessEqual, 4),
                TokenKind::Greater => (AstBooleanOperator::Greater, 4),
                TokenKind::GreaterEqual => (AstBooleanOperator::GreaterEqual, 4),
                _ => break
            };
        if precedence < min_precedence { break; }

        skip_array_newlines(&mut *lookahead.buffer)?;
        let right = parse_range_expression(&mut *lookahead.buffer).map_err(|mut error|
            {
                if error.location.is_none() { error.location = Some(token.location.clone()); }
                error
            })?
            .ok_or_else(|| ParserError
                {
                    location: Some(token.location.clone()),
                    kind: ParserErrorKind::ExpectedExpression
                })?;
        let right = parse_boolean_tail(&mut *lookahead.buffer, right, precedence + 1)?;
        left = AstExpression
            {
                location: token.location,
                kind: AstExpressionKind::BooleanExpression(
                    operator, Box::new(left), Box::new(right)),
                interpolation_members: Default::default(),
                dispatch_type: Default::default(),
                string_flag: None,
            };
        lookahead.commit();
    }
    Ok(left)
}


pub fn parse_expression(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstExpression>>
{
    let Some(left) = parse_range_expression(buffer)? else { return Ok(None); };
    let mut expression = parse_boolean_tail(buffer, left, 0)?;
    if buffer.syntax_context() != SyntaxContext::Argument
    { resolve_logical_commands(&mut expression, false); }
    Ok(Some(expression))
}


// Truth-tested command words follow the same call rule as a standalone condition.
// Arguments, quoted text, variables, and explicit references retain their value semantics.
fn resolve_logical_commands(expression: &mut AstExpression, operand: bool)
{
    // Flat logical chains can be long while editing, so do not recurse through their AST.
    let mut pending = vec![(expression, operand)];
    while let Some((expression, operand)) = pending.pop()
    {
        if    operand
           && matches!(&expression.kind, AstExpressionKind::Symbol(symbol) if !symbol.is_glob())
        {
            let location = expression.location.clone();
            let command = replace(expression, new_ast_literal(location, Value::None, None));
            expression.kind = AstExpressionKind::TryExecute(Box::new(command));
            continue;
        }
        match &mut expression.kind
        {
            AstExpressionKind::BooleanExpression(AstBooleanOperator::And | AstBooleanOperator::Or,
                                                 left, right) =>
                {
                    pending.push((right, true));
                    pending.push((left, true));
                },
            AstExpressionKind::BooleanNot(inner) => pending.push((inner, true)),
            _ => {},
        }
    }
}


// Arithmetic binds inside each bound; comparisons and logical operators combine
// complete ranges. A second range operator must be explicitly parenthesized.
fn parse_range_expression(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstExpression>>
{
    let leading = try_expect_one_of_tokens(buffer, &[TokenKind::Range, TokenKind::RangeInclusive])?;
    let start = if leading.is_none() { parse_scalar_expression(buffer)? } else { None };
    // A bare command name is not a range bound: `echo ..5` and `cd ..`
    // must leave the range for argument parsing.
    if start.as_ref().is_some_and(|value| matches!(value.kind, AstExpressionKind::Symbol(_)))
    {
        return Ok(start);
    }
    let operator = match leading
        {
            Some(operator) => Some(operator),
            None => try_expect_one_of_tokens(buffer,
                                             &[TokenKind::Range, TokenKind::RangeInclusive])?
        };
    let Some(operator) = operator else { return Ok(start); };
    let inclusive = operator.kind == TokenKind::RangeInclusive;
    let at_end =
        {
            let lookahead = Lookahead::new(buffer);
            lookahead.buffer.next()?.is_none_or(|token| matches!(token.kind,
                TokenKind::LineBreak | TokenKind::StatementBreak | TokenKind::BlockOpen
                | TokenKind::BlockClose | TokenKind::ParenClose | TokenKind::SquareClose
                | TokenKind::Comma | TokenKind::TypeDelimiter | TokenKind::Equal
                | TokenKind::NotEqual | TokenKind::And | TokenKind::Or | TokenKind::MatchArrow
                | TokenKind::Less | TokenKind::LessEqual | TokenKind::Greater
                | TokenKind::GreaterEqual | TokenKind::Pipe))
        };
    let end = if at_end { None } else { parse_scalar_expression(buffer)? };
    if    (inclusive && end.is_none())
       || try_expect_one_of_tokens(buffer, &[TokenKind::Range, TokenKind::RangeInclusive])?
            .is_some()
    {
        return Err(ParserError
            {
                location: Some(operator.location),
                kind: ParserErrorKind::InvalidRange,
            });
    }
    if !at_end && end.is_none()
    {
        return Err(ParserError
            {
                location: Some(operator.location),
                kind: ParserErrorKind::ExpectedExpression,
            });
    }
    Ok(Some(AstExpression
        {
            location: operator.location,
            kind: AstExpressionKind::Range(start.map(Box::new), end.map(Box::new), inclusive),
            interpolation_members: Default::default(),
            dispatch_type: Default::default(),
            string_flag: None
        }))
}


/**
 * Parse a value with the command-call rules shared by assignments, returns, and parentheses.
 */
pub fn parse_value_expression(buffer: &mut TokenBuffer<'_,
                              '_>) -> ParseResult<Option<AstExpression>>
{
    parse_value_before_block(buffer, false, false)
}


pub fn parse_condition_expression(buffer: &mut TokenBuffer<'_,
                                  '_>) -> ParseResult<Option<AstExpression>>
{
    parse_value_before_block(buffer, true, false)
}


fn parse_value_before_block(buffer: &mut TokenBuffer<'_, '_>,
                            stop_at_block: bool, stop_at_colon: bool)
    -> ParseResult<Option<AstExpression>>
{
    buffer.with_context(SyntaxContext::Expression, |buffer|
        parse_value_before_block_inner(buffer, stop_at_block, stop_at_colon))
}


fn parse_value_before_block_inner(buffer: &mut TokenBuffer<'_, '_>,
                            stop_at_block: bool,
                            stop_at_colon: bool) -> ParseResult<Option<AstExpression>>
{
    let Some(first) = parse_stage_before_block(buffer, stop_at_block, stop_at_colon)?
        else { return Ok(None); };
    let location = first.location.clone();
    let mut stages = vec![first];
    let mut boundaries = Vec::new();
    while let Some(boundary) = parse_pipe_boundary(buffer)?
    {
        let stage = parse_stage_before_block(buffer, stop_at_block, stop_at_colon)?
            .ok_or_else(|| ParserError { location: Some(boundary.location.clone()),
                kind: ParserErrorKind::ExpectedExpression })?;
        boundaries.push(boundary);
        stages.push(stage);
    }
    if boundaries.is_empty() { return Ok(stages.pop()); }
    Ok(Some(AstExpression { location, string_flag: None,
        interpolation_members: Default::default(),
        dispatch_type: Default::default(),
        kind: AstExpressionKind::Pipeline(Box::new(
            parse_branches(buffer, stages, boundaries,
                stop_at_block, stop_at_colon)?)) }))
}


pub(super) fn parse_pipe_boundary(buffer: &mut TokenBuffer<'_, '_>)
    -> ParseResult<Option<AstPipeBoundary>>
{
    parse_pipe_boundary_kind(buffer, TokenKind::Pipe)
}


pub(super) fn parse_pipe_boundary_kind(buffer: &mut TokenBuffer<'_, '_>, kind: TokenKind)
    -> ParseResult<Option<AstPipeBoundary>>
{
    let mut lookahead = Lookahead::new(buffer);
    let Some(token) = lookahead.buffer.next()? else { return Ok(None); };
    let (decode, pipe) = if token.kind == kind { (None, token) }
    else if token.kind == TokenKind::Symbol
    {
        if try_expect_token(lookahead.buffer, TokenKind::TypeDelimiter)?.is_none()
        { return Ok(None); }
        let Some(pipe) = try_expect_token(lookahead.buffer, kind)?
            else { return Ok(None); };
        lookahead.buffer.annotate(token.span, SyntaxRole::FormatName);
        (Some(token.token_value_text()), pipe)
    }
    else { return Ok(None); };
    let annotation = if try_expect_token(lookahead.buffer, TokenKind::TypeDelimiter)?.is_some()
    {
        let token = expect_type_name(lookahead.buffer)?;
        lookahead.buffer.annotate(token.span, SyntaxRole::PipeAnnotation);
        let mut name = token.token_value_text();
        while try_expect_token(lookahead.buffer, TokenKind::Scope)?.is_some()
        {
            name.push_str("::");
            let token = expect_type_name(lookahead.buffer)?;
            lookahead.buffer.annotate(token.span, SyntaxRole::PipeAnnotation);
            name.push_str(&token.token_value_text());
        }
        Some(name)
    }
    else { None };
    let layout = lookahead.buffer.anchor(&pipe);
    lookahead.commit();
    Ok(Some(AstPipeBoundary { location: pipe.location, layout, decode,
        annotation, type_id: None }))
}


pub(super) fn parse_stage_before_block(buffer: &mut TokenBuffer<'_, '_>,
                            stop_at_block: bool,
                            stop_at_colon: bool) -> ParseResult<Option<AstExpression>>
{
    let head = buffer.syntax_checkpoint();
    let Some(mut expression) = parse_expression(buffer)? else { return Ok(None); };
    if matches!(expression.kind, AstExpressionKind::Symbol(_) | AstExpressionKind::EnumVariant(_, _)
        | AstExpressionKind::SpacedEmptyCall(_))
    { buffer.annotate_names_since(head, SyntaxRole::CommandName); }

    if matches!(&expression.kind, AstExpressionKind::Symbol(_) | AstExpressionKind::Variable(_)
        | AstExpressionKind::EnumVariant(_, _)
        | AstExpressionKind::Index(_, _) | AstExpressionKind::Field(_, _, _)
        | AstExpressionKind::SpacedEmptyCall(_))
    {
        reject_index_assignment(buffer, &expression)?;
        let arguments = parse_arguments_before_block(buffer, stop_at_block, stop_at_colon)?;
        let location = expression.location.clone();

        if !arguments.is_empty()
        {
            expression = AstExpression
                {
                    location: location.clone(),
                    kind: AstExpressionKind::Execute(Box::new(AstExecuteStatement
                        {
                            public: false,
                            location,
                            expand_path: matches!(&expression.kind,
                                AstExpressionKind::Symbol(symbol) if symbol.name.starts_with('~')),
                            executable: expression,
                            arguments
                        })),
                    interpolation_members: Default::default(),
                    dispatch_type: Default::default(),
                    string_flag: None
                };
        }
    }

    parse_redirection_tail(buffer, expression)
}


pub(super) fn parse_redirection_tail(buffer: &mut TokenBuffer<'_, '_>,
                                    expression: AstExpression)
                                    -> ParseResult<Option<AstExpression>>
{
    let mut redirects = Vec::new();
    let mut streams = 0;
    while let Some(operator) = try_expect_one_of_tokens(buffer, &[
            TokenKind::RedirectTo, TokenKind::RedirectErrorTo, TokenKind::RedirectBothTo,
        ])?
    {
        let (stream, mask) = match operator.kind
            {
                TokenKind::RedirectTo => (RedirectStream::Output, 2),
                TokenKind::RedirectErrorTo => (RedirectStream::Error, 4),
                TokenKind::RedirectBothTo => (RedirectStream::Both, 6),
                _ => unreachable!("Expected an output redirection operator")
            };
        if streams & mask != 0
        {
            return Err(ParserError
                {
                    location: Some(operator.location),
                    kind: ParserErrorKind::InvalidRedirection(
                        "A stream can only be redirected once per expression.".to_string()),
                });
        }
        streams |= mask;
        let target = parse_expression(buffer).map_err(|mut error|
            {
                if error.location.is_none() { error.location = Some(operator.location.clone()); }
                error
            })?.ok_or_else(|| ParserError
            {
                location: Some(operator.location.clone()),
                kind: ParserErrorKind::ExpectedExpression,
            })?;
        redirects.push(AstRedirection { location: operator.location, stream, target });
    }
    if !redirects.is_empty()
    {
        return Ok(Some(AstExpression
            {
                location: expression.location.clone(),
                kind: AstExpressionKind::Redirect(Box::new(expression), redirects),
                interpolation_members: Default::default(),
                dispatch_type: Default::default(),
                string_flag: None,
            }));
    }

    // Globs remain collection values. Only a lone command word is ambiguous.
    if matches!(&expression.kind, AstExpressionKind::Symbol(symbol) if !symbol.is_glob())
    {
        return Ok(Some(AstExpression
            {
                location: expression.location.clone(),
                kind: AstExpressionKind::TryExecute(Box::new(expression)),
                interpolation_members: Default::default(),
                dispatch_type: Default::default(),
                string_flag: None,
            }));
    }
    Ok(Some(expression))
}


pub fn parse_command_arguments(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Vec<AstExpression>>
{
    parse_arguments_before_block(buffer, false, false)
}


/**
 * Colons inside an attached bare argument are text, as in npm script names or
 * host:port. Type and collection parsers keep their delimiter rules, and a colon
 * before a pipe still belongs to the conversion boundary.
 */
pub(super) fn parse_colon_word(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<Token>>
{
    fn bare_word(token: &Token) -> bool
    {
        match &token.value
        {
            TokenValue::Symbol(_) => token.kind == TokenKind::Symbol,
            TokenValue::Literal(TokenLiteral::Integer(..) | TokenLiteral::Float(..)
                | TokenLiteral::Boolean(..)) => true,
            TokenValue::None => valid_type_name(&token.token_value_text()),
            _ => false,
        }
    }

    let mut lookahead = Lookahead::new(buffer);
    let Some(mut word) = lookahead.buffer.next()? else { return Ok(None); };
    if !bare_word(&word) { return Ok(None); }
    lookahead.buffer.annotate_word(word.span);
    let mut text = word.token_value_text();
    let mut joined = false;
    loop
    {
        let mut tail = Lookahead::new(&mut *lookahead.buffer);
        let Some(colon) = try_expect_token(tail.buffer, TokenKind::TypeDelimiter)?
            else { break; };
        if colon.span.start != word.span.end { break; }
        let Some(part) = tail.buffer.next()? else { break; };
        if !bare_word(&part) || part.span.start != colon.span.end { break; }
        tail.buffer.annotate_word(colon.span);
        tail.buffer.annotate_word(part.span);
        text.push(':');
        text.push_str(&part.token_value_text());
        word.span.end = part.span.end;
        joined = true;
        tail.commit();
    }
    if !joined { return Ok(None); }
    word.kind = TokenKind::Symbol;
    word.value = TokenValue::Symbol(text);
    lookahead.commit();
    Ok(Some(word))
}


fn parse_arguments_before_block(buffer: &mut TokenBuffer<'_, '_>,
                                stop_at_block: bool, stop_at_colon: bool)
    -> ParseResult<Vec<AstExpression>>
{
    buffer.with_context(SyntaxContext::Argument, |buffer|
        parse_arguments_before_block_inner(buffer, stop_at_block, stop_at_colon))
}


fn parse_arguments_before_block_inner(buffer: &mut TokenBuffer<'_, '_>,
                                stop_at_block: bool,
                                stop_at_colon: bool) -> ParseResult<Vec<AstExpression>>
{
    let mut parameter_expressions = Vec::new();

    loop
    {
        {
            let probe = Lookahead::new(buffer);
            if parse_pipe_boundary(probe.buffer)?.is_some() { break; }
        }
        let next =
            {
                let mut lookahead = Lookahead::new(buffer);
                let Some(token) = lookahead.buffer.next()? else { break; };

                match token.kind
                {
                    TokenKind::BlockOpen if stop_at_block => break,
                    TokenKind::TypeDelimiter if stop_at_colon => break,

                    TokenKind::LineBreak | TokenKind::StatementBreak | TokenKind::BlockClose
                    | TokenKind::ParenClose | TokenKind::SquareClose | TokenKind::Comma
                    | TokenKind::RedirectTo | TokenKind::RedirectErrorTo
                    | TokenKind::RedirectBothTo | TokenKind::MatchArrow => break,

                    TokenKind::LineContinue =>
                        {
                            expect_token(lookahead.buffer, TokenKind::LineBreak)?;
                            lookahead.commit();
                            continue;
                        },

                    _ => token
                }
            };

        if !stop_at_colon && let Some(word) = parse_colon_word(buffer)?
        {
            let text = word.token_value_text();
            let flag = if text.contains('$') { AstStringFlag::Interpolated(Vec::new()) }
                else { AstStringFlag::NonInterpolated };
            parameter_expressions.push(new_ast_symbol(word.location, text, Some(flag)));
            continue;
        }

        // The lookahead rewound this token so the full expression can consume it.
        let expression = parse_expression(buffer)?.ok_or_else(|| ParserError
            {
                location: Some(next.location),
                kind: ParserErrorKind::ExpectedExpression
            })?;

        parameter_expressions.push(expression);
    }

    Ok(parameter_expressions)
}


pub fn parse_exec_expression(buffer: &mut TokenBuffer<'_, '_>) -> ParseResult<Option<AstExpression>>
{
    buffer.with_context(SyntaxContext::Command, parse_exec_expression_inner)
}


fn parse_exec_expression_inner(buffer: &mut TokenBuffer<'_, '_>)
    -> ParseResult<Option<AstExpression>>
{
    if let Some(expression) = parse_outer_expression(buffer)?
    {
        return Ok(Some(expression));
    }
    {
        let mut lookahead = Lookahead::new(buffer);
        if    let Some(expression) = parse_math_expression(&mut *lookahead.buffer)?
           && matches!(
                expression.kind,
                AstExpressionKind::Index(_, _)
                    | AstExpressionKind::Field(_, _, _)
                    | AstExpressionKind::EnumVariant(_, _)
                    | AstExpressionKind::AnonymousFunction(_)
                    | AstExpressionKind::StructConstructor(_)
                    | AstExpressionKind::TypeConversion(_, _, _, _)
                    | AstExpressionKind::SpacedEmptyCall(_)
            )
        {
            reject_index_assignment(&mut *lookahead.buffer, &expression)?;
            lookahead.commit();
            return Ok(Some(expression));
        }
    }
    let expression = match_one_of(buffer, &[parse_variable_expression,
                           parse_symbol_expression,
                           parse_literal_expression,
                           parse_operator_to_symbol])?;

    if    let Some(expression) = &expression
       && matches!(
            expression.kind,
            AstExpressionKind::VariableSplat(_) | AstExpressionKind::Splat(_)
        )
    {
        return Err(ParserError
            {
                location: Some(expression.location.clone()),
                kind: ParserErrorKind::SplatExecutable
            });
    }

    Ok(expression)
}


// Valid indexed assignments are consumed by the statement parser. Do not turn
// an invalid target or an assignment inside an expression into a command call.
fn reject_index_assignment(buffer: &mut TokenBuffer<'_, '_>,
                            expression: &AstExpression) -> ParseResult<()>
{
    if    matches!(expression.kind,
                   AstExpressionKind::Index(_, _) | AstExpressionKind::Field(_, _, _))
       && let Some(assign) = try_expect_token(buffer, TokenKind::Assign)?
    {
        return Err(ParserError
            {
                location: Some(assign.location),
                kind: ParserErrorKind::InvalidAssignmentTarget
            });
    }
    Ok(())
}

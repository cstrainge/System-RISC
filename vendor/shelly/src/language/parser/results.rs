
use std::fmt::{ self, Display, Formatter, Debug };

use crate::language::{ text::location::Location,
                       tokenizer::{ TokenKind, TokenizerError },
                       ast::AstError };

#[derive(Clone)]
pub enum ParserErrorKind
{
    TokenizerError(TokenizerError),
    UnexpectedEOF(TokenKind),
    MatchError(Vec<ParserError>),
    ExpectedToken(TokenKind, Option<TokenKind>),
    ExpectedExpression,
    SplatExecutable,
    InvalidAssignmentTarget,
    InvalidRange,
    InvalidPipeline(String),
    InvalidMatch(String),
    InvalidImport(String),
    InvalidVisibility(String),
    InvalidExclusion(String),
    InvalidRedirection(String),
    InvalidEnum(String),
    InvalidType(String),
    UnexpectedElse,
    ExpressionNotString,
    DuplicateParameter(String),
    DuplicateLoopBinding(String),
    DuplicateDestructureBinding(String),
    NoProgress,
}


impl Display for ParserErrorKind
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result
    {
        match self
        {
            ParserErrorKind::TokenizerError(error) => write!(f, "Tokenizer error: {}.", error),

            ParserErrorKind::UnexpectedEOF(kind) =>
                {
                    write!(f, "Unexpected end of file, expected token: {}.", kind)
                },

            ParserErrorKind::MatchError(errors) => write!(f, "Match error: {:?}.", errors),
            ParserErrorKind::ExpectedToken(expected, found) =>
                {
                    match found
                    {
                        Some(found) =>
                            write!(f, "Expected token: {}, but found: {}.", expected, found),
                        None => write!(f, "Expected token: {}, but found end of file.", expected)
                    }
                }

            ParserErrorKind::ExpectedExpression =>
                {
                    write!(f, "Expected expression.")
                },

            ParserErrorKind::SplatExecutable =>
                write!(f, "A variable splat cannot be used as an executable."),

            ParserErrorKind::InvalidAssignmentTarget =>
                write!(f, "Assignment requires a variable or an indexed variable."),

            ParserErrorKind::InvalidType(message) | ParserErrorKind::InvalidEnum(message) =>
                write!(f, "{}", message),

            ParserErrorKind::InvalidRedirection(message) => write!(f, "{}", message),
            ParserErrorKind::InvalidMatch(message) => write!(f, "{}", message),
            ParserErrorKind::InvalidExclusion(message) => write!(f, "{}", message),
            ParserErrorKind::InvalidImport(message) => write!(f, "{}", message),
            ParserErrorKind::InvalidVisibility(message) => write!(f, "{}", message),

            ParserErrorKind::InvalidRange =>
                write!(f, "Ranges cannot be chained and inclusive ranges require an end bound."),

            ParserErrorKind::ExpressionNotString =>
                {
                    write!(f, "Expression does not resolve to a string.")
                },

            ParserErrorKind::UnexpectedElse => write!(
                f,
                "Else must follow an if branch and may appear only once at the end of a chain."
            ),

            ParserErrorKind::DuplicateParameter(name) =>
                write!(f, "Duplicate function parameter: {}.", name),

            ParserErrorKind::DuplicateLoopBinding(name) =>
                write!(f, "Duplicate loop binding: {}.", name),

            ParserErrorKind::DuplicateDestructureBinding(name) =>
                write!(f, "Duplicate destructuring binding: {}.", name),

            ParserErrorKind::InvalidPipeline(message) => write!(f, "Compile error: {}", message),

            ParserErrorKind::NoProgress =>
                write!(f, "Repeated parser succeeded without consuming a token.")
        }
    }
}


#[derive(Clone)]
pub struct ParserError
{
    pub location: Option<Location>,
    pub kind: ParserErrorKind
}


pub type ParseResult<T> = Result<T, ParserError>;


impl From<TokenizerError> for ParserError
{
    fn from(error: TokenizerError) -> Self
    {
        ParserError
            {
                location: None,
                kind: ParserErrorKind::TokenizerError(error),
            }
    }
}


impl From<AstError> for ParserError
{
    fn from(error: AstError) -> Self
    {
        match error
        {
            AstError::ExpressionNotString(location) =>
                ParserError
                    {
                        location: Some(location),
                        kind: ParserErrorKind::ExpressionNotString
                    }
        }
    }
}


impl Display for ParserError
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result
    {
        write!(f, "{:?}", self)
    }
}


impl Debug for ParserError
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result
    {
        if let Some(location) = &self.location
        {
            write!(f, "Parser error: {}: {}", location, self.kind)
        }
        else
        {
            write!(f, "Parser error: {}", self.kind)
        }
    }
}

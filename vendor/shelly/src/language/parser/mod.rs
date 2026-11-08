
use crate::language::{ ast::AstTopLevel,
                       tokenizer::{ TokenBuffer, Tokenizer },
                       parser::{ statements::parse_statement, results::ParseResult,
                                 source::SyntaxMetadata } };
pub use crate::language::parser::results::ParserError;
pub(crate) use crate::language::parser::expressions::member_word_parts;

mod results;
mod base_utils;
mod expressions;
mod pipeline;
mod statements;
pub(crate) mod editor;
pub mod source;
pub use crate::language::parser::editor::{ analyze_editor, EditorAnalysis };

pub type ParserResult<T> = ParseResult<T>;



/**
 * Take a block of source code and parse it into an abstract syntax tree (AST) representing the
 * top-level structure, all the way down to individual expressions.
 */
pub fn parse_text<'a>(tokenizer: &mut Tokenizer<'a>) -> ParserResult<AstTopLevel>
{
    let mut token_buffer = TokenBuffer::new(tokenizer);
    parse_buffer(&mut token_buffer)
}


/// Run exactly the execution grammar, collecting committed syntax facts without compilation.
/// On failure, metadata from accepted prefixes remains available to tooling.
pub fn parse_with_metadata(tokenizer: &mut Tokenizer<'_>)
    -> (ParserResult<AstTopLevel>, SyntaxMetadata)
{
    let mut token_buffer = TokenBuffer::with_syntax(tokenizer);
    let result = parse_buffer(&mut token_buffer);
    (result, token_buffer.syntax_metadata().unwrap_or_default())
}


fn parse_buffer(token_buffer: &mut TokenBuffer<'_, '_>) -> ParserResult<AstTopLevel>
{
    let mut ast_top_level = AstTopLevel::new();

    while let Some(statement) = parse_statement(token_buffer)?
    {
        ast_top_level.push(statement);
    }

    Ok(ast_top_level)
}

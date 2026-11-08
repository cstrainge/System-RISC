
use std::collections::BTreeMap;

use crate::language::text::span::SourceSpan;


/**
 * Grammar contexts, independent of colors and symbol resolution.
 */
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SyntaxContext
{
    #[default]
    Expression,
    Command,
    Argument,
    Type,
}


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxRole
{
    Invalid,
    Name,
    CommandName,
    Argument,
    Keyword,
    Variable,
    Member,
    Literal,
    Operator,
    Delimiter,
    VariableDeclaration,
    ParameterDeclaration,
    FunctionDeclaration,
    AliasDeclaration,
    TypeDeclaration,
    TypeName,
    FieldName,
    EnumVariantDeclaration,
    ModuleName,
    ImportName,
    FormatName,
    PipeAnnotation,
    Discard,
}


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyntaxFact
{
    pub span: SourceSpan,
    pub role: SyntaxRole,
    pub context: SyntaxContext,
}


#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyntaxMetadata
{
    /**
     * One fact per committed token read, in source order. Rolled-back trials are excluded.
     */
    pub facts: Vec<SyntaxFact>,
}


/**
 * Append-only overrides let nested token lookahead undo classifications as well as reads.
 */
#[derive(Default)]
pub(crate) struct SyntaxJournal
{
    pub entries: Vec<SyntaxFact>,
}


impl SyntaxJournal
{
    pub fn snapshot(&self) -> SyntaxMetadata
    {
        let mut facts = BTreeMap::new();
        for fact in &self.entries { facts.insert(fact.span.start, *fact); }
        SyntaxMetadata { facts: facts.into_values().collect() }
    }
}


use std::collections::BTreeMap;

use crate::language::{ analysis::{ SemanticAnalysis, SymbolKind },
                       parser::source::SyntaxRole,
                       text::{ diagnostic::DiagnosticKind, span::SourceSpan },
                       tokenizer::{ TokenKind, TokenLiteral, TokenValue,
                                    source::{ StringPartKind, TriviaKind } } };


/**
 * Language classifications are independent of terminal colors and frontend settings.
 */
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum HighlightKind
{
    Plain,
    Comment,
    Keyword,
    Variable,
    Type,
    Function,
    Method,
    Builtin,
    Alias,
    Command,
    Argument,
    String,
    Number,
    Literal,
    Operator,
    Delimiter,
    Field,
    Module,
    Format,
    Escape,
    Error,
}


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HighlightSpan
{
    pub span: SourceSpan,
    pub kind: HighlightKind,
}


/**
 * Resolve overlapping facts once into disjoint original-source spans. The sweep is
 * bounded by analysis facts, rather than allocating one entry per byte of a large paste.
 */
pub fn highlight_spans(source: &str, analysis: &SemanticAnalysis) -> Vec<HighlightSpan>
{
    let mut layers = Vec::new();
    for token in &analysis.source.tokens
    {
        let kind = match token.kind
            {
                TokenKind::Invalid => HighlightKind::Error,
                TokenKind::Literal => match &token.value
                    {
                        TokenValue::Literal(TokenLiteral::String(..)) => HighlightKind::String,
                        TokenValue::Literal(TokenLiteral::Boolean(..)) => HighlightKind::Literal,
                        _ => HighlightKind::Number,
                    },
                TokenKind::Identifier | TokenKind::AutoIdentifier => HighlightKind::Variable,
                TokenKind::Member => HighlightKind::Field,
                TokenKind::Symbol | TokenKind::Glob => HighlightKind::Argument,
                TokenKind::Let | TokenKind::Export | TokenKind::Public | TokenKind::Alias
                | TokenKind::Sub | TokenKind::If | TokenKind::Else | TokenKind::While
                | TokenKind::Until | TokenKind::For | TokenKind::In | TokenKind::Break
                | TokenKind::Continue | TokenKind::Loop | TokenKind::Match | TokenKind::Return
                | TokenKind::Error | TokenKind::Try | TokenKind::Catch | TokenKind::Finally
                | TokenKind::Throw
                | TokenKind::Function | TokenKind::Struct | TokenKind::Enum | TokenKind::Type
                | TokenKind::Import => HighlightKind::Keyword,
                TokenKind::LineBreak => HighlightKind::Plain,
                TokenKind::Comma | TokenKind::StatementBreak | TokenKind::ParenOpen
                | TokenKind::ParenClose | TokenKind::SquareOpen | TokenKind::SquareClose
                | TokenKind::IndexOpen | TokenKind::BlockOpen | TokenKind::BlockClose =>
                    HighlightKind::Delimiter,
                _ => HighlightKind::Operator,
            };
        layers.push((token.span, kind, 0));
    }
    for trivia in &analysis.source.lexical.trivia
    {
        let kind = match trivia.kind
            {
                TriviaKind::Comment => HighlightKind::Comment,
                TriviaKind::LineContinuation => HighlightKind::Operator,
                TriviaKind::Whitespace => HighlightKind::Plain,
            };
        layers.push((trivia.span, kind, 1));
    }
    for fact in &analysis.source.syntax.facts
    {
        let kind = match fact.role
            {
                SyntaxRole::Invalid => HighlightKind::Error,
                SyntaxRole::Name | SyntaxRole::Argument | SyntaxRole::ImportName =>
                    HighlightKind::Argument,
                SyntaxRole::CommandName => HighlightKind::Command,
                SyntaxRole::Keyword => HighlightKind::Keyword,
                SyntaxRole::Variable | SyntaxRole::VariableDeclaration
                | SyntaxRole::ParameterDeclaration => HighlightKind::Variable,
                SyntaxRole::Member | SyntaxRole::FieldName => HighlightKind::Field,
                SyntaxRole::FunctionDeclaration => HighlightKind::Function,
                SyntaxRole::AliasDeclaration => HighlightKind::Alias,
                SyntaxRole::TypeDeclaration | SyntaxRole::TypeName => HighlightKind::Type,
                SyntaxRole::EnumVariantDeclaration | SyntaxRole::Discard => HighlightKind::Literal,
                SyntaxRole::ModuleName => HighlightKind::Module,
                SyntaxRole::FormatName | SyntaxRole::PipeAnnotation => HighlightKind::Format,
                SyntaxRole::Operator => HighlightKind::Operator,
                SyntaxRole::Delimiter => HighlightKind::Delimiter,
                SyntaxRole::Literal => continue,
            };
        layers.push((fact.span, kind, 2));
    }
    for string in &analysis.source.lexical.strings
    {
        for part in &string.parts
        {
            if part.kind == StringPartKind::Escape
            { layers.push((part.source, HighlightKind::Escape, 3)); }
        }
    }
    for fact in &analysis.facts
    {
        let kind = match fact.kind
            {
                SymbolKind::Variable => HighlightKind::Variable,
                SymbolKind::Type => HighlightKind::Type,
                SymbolKind::Function => HighlightKind::Function,
                SymbolKind::Method => HighlightKind::Method,
                SymbolKind::Builtin => HighlightKind::Builtin,
                SymbolKind::Alias => HighlightKind::Alias,
                SymbolKind::ExternalCommand => HighlightKind::Command,
                SymbolKind::Field => HighlightKind::Field,
                SymbolKind::EnumVariant => HighlightKind::Literal,
                SymbolKind::Module => HighlightKind::Module,
            };
        layers.push((fact.span, kind, 4));
    }
    for diagnostic in &analysis.source.diagnostics
    {
        // Unfinished input is normal while editing. Only actual damaged spans turn red.
        if diagnostic.kind == DiagnosticKind::Invalid
        { layers.push((diagnostic.span, HighlightKind::Error, 5)); }
    }
    if let Some(span) = analysis.source.unparsed
    { layers.push((span, HighlightKind::Plain, 6)); }

    let mut events: BTreeMap<usize, Vec<(bool, usize)>> = BTreeMap::new();
    events.entry(0).or_default();
    events.entry(source.len()).or_default();
    for (index, (span, _, _)) in layers.iter().enumerate()
    {
        if span.start == span.end || span.slice(source).is_none() { continue; }
        events.entry(span.start).or_default().push((true, index));
        events.entry(span.end).or_default().push((false, index));
    }
    let mut active = BTreeMap::new();
    let mut spans: Vec<HighlightSpan> = Vec::new();
    let mut previous = 0;
    for (offset, changes) in events
    {
        if previous < offset
        {
            let kind = active.last_key_value().map_or(HighlightKind::Plain, |(_, kind)| *kind);
            if let Some(last) = spans.last_mut().filter(|span| span.kind == kind)
            { last.span.end = offset; }
            else { spans.push(HighlightSpan { span: SourceSpan::new(previous, offset), kind }); }
        }
        for (start, index) in changes
        {
            let (_, kind, priority) = layers[index];
            if start { active.insert((priority, index), kind); }
            else { active.remove(&(priority, index)); }
        }
        previous = offset;
    }
    spans
}

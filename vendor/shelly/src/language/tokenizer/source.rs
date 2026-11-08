
use crate::language::text::span::SourceSpan;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriviaKind
{
    Whitespace,
    Comment,
    LineContinuation,
}

#[derive(Clone, Debug)]
pub struct Trivia
{
    pub kind: TriviaKind,
    pub span: SourceSpan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StringPartKind
{
    Text,
    Escape,
}

#[derive(Clone, Debug)]
pub struct StringPart
{
    /// Absolute source offsets, including the backslash and digits of an escape.
    pub source: SourceSpan,
    /// Byte offsets relative to the decoded string value.
    pub decoded: SourceSpan,
    pub kind: StringPartKind,
}

#[derive(Clone, Debug)]
pub struct StringSource
{
    pub span: SourceSpan,
    pub opening: SourceSpan,
    pub closing: Option<SourceSpan>,
    pub interpolated: bool,
    pub decoded: String,
    pub parts: Vec<StringPart>,
}

impl StringSource
{
    /// Translate a decoded reference to original source. An escape maps to its
    /// complete spelling; a reference spanning removed indentation covers that gap.
    pub fn source_span(&self, decoded: SourceSpan) -> Option<SourceSpan>
    {
        if decoded.start == decoded.end || decoded.slice(&self.decoded).is_none() { return None; }
        let mut result: Option<SourceSpan> = None;
        for part in &self.parts
        {
            let start = decoded.start.max(part.decoded.start);
            let end = decoded.end.min(part.decoded.end);
            if start >= end { continue; }
            let source = if part.kind == StringPartKind::Escape
                {
                    part.source
                }
                else
                {
                    SourceSpan::new(part.source.start + start - part.decoded.start,
                                    part.source.start + end - part.decoded.start)
                };
            result = Some(result.map_or(source, |span| span.cover(source)));
        }
        result
    }
}

/// Optional lexical facts, with no colors, interpreter objects, or executable AST.
#[derive(Clone, Debug, Default)]
pub struct LexicalMetadata
{
    pub trivia: Vec<Trivia>,
    pub strings: Vec<StringSource>,
}

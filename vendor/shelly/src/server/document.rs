
use std::collections::{ BTreeMap, BTreeSet };

use serde_json::{ Value, json };

use crate::language::{ analysis::{ analyze_semantics_with_limits, SymbolSnapshot,
                                  highlight::{ highlight_spans, HighlightKind } },
                       text::diagnostic::DiagnosticKind };


// The legend's ordering is part of the wire interface. Colors remain the editor's choice.
pub const TOKEN_TYPES: &[&str] = &[
        "comment", "keyword", "variable", "type", "function", "method", "shellyBuiltin",
        "shellyAlias", "shellyCommand", "shellyArgument", "string", "number", "shellyLiteral",
        "operator", "shellyDelimiter", "property", "namespace", "shellyFormat", "shellyEscape",
        "shellyError",
    ];

const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_PARSE_STEPS: usize = 640_000;


#[derive(Clone, Copy, Default)]
struct Position
{
    line: u32,
    character: u32,
}

impl Position
{
    fn value(self) -> Value { json!({ "line": self.line, "character": self.character }) }
}


pub struct Document
{
    pub version: i64,
    pub source: String,
    pub tokens: Vec<u32>,
    pub diagnostics: Vec<Value>,
}


impl Document
{
    pub fn analyze(source: &str, version: i64, snapshot: &SymbolSnapshot) -> Self
    {
        let analysis = analyze_semantics_with_limits(source, snapshot,
                                                    MAX_SOURCE_BYTES, MAX_PARSE_STEPS);
        let spans = highlight_spans(source, &analysis);
        let mut endpoints = BTreeSet::new();
        for span in &spans
        {
            if span.kind == HighlightKind::Plain { continue; }
            endpoints.extend([span.span.start, span.span.end]);
            // LSP tokens are split at line boundaries, including CRLF and lone CR.
            for (offset, character) in source[span.span.start..span.span.end].char_indices()
            {
                if matches!(character, '\r' | '\n')
                {
                    let offset = span.span.start + offset;
                    endpoints.extend([offset, offset + 1]);
                }
            }
        }
        for diagnostic in &analysis.source.diagnostics
        { endpoints.extend([diagnostic.span.start, diagnostic.span.end]); }
        let positions = positions(source, &endpoints);
        let mut tokens = Vec::new();
        let mut previous = Position::default();
        for span in spans
        {
            let Some(kind) = token_kind(span.kind) else { continue; };
            let mut start = span.span.start;
            for (offset, character) in source[span.span.start..span.span.end].char_indices()
            {
                if matches!(character, '\r' | '\n')
                {
                    let end = span.span.start + offset;
                    append_token(&mut tokens, positions[&start], positions[&end], kind,
                                 &mut previous);
                    start = end + 1;
                }
            }
            append_token(&mut tokens, positions[&start], positions[&span.span.end], kind,
                         &mut previous);
        }
        let diagnostics = analysis.source.diagnostics.iter().map(|diagnostic|
            {
                let severity = match diagnostic.kind
                    {
                        DiagnosticKind::Invalid => 1,
                        DiagnosticKind::Limit => 2,
                        DiagnosticKind::Incomplete => 4,
                    };
                json!({ "range": { "start": positions[&diagnostic.span.start].value(),
                                    "end": positions[&diagnostic.span.end].value() },
                        "severity": severity, "source": "shelly-shell",
                        "message": diagnostic.message })
            }).collect();
        Self { version, source: source.into(), tokens, diagnostics }
    }
}


// Convert all requested byte boundaries in one pass, avoiding repeated scans of long lines.
fn positions(source: &str, endpoints: &BTreeSet<usize>) -> BTreeMap<usize, Position>
{
    let mut result = BTreeMap::new();
    let mut characters = source.char_indices().peekable();
    let mut position = Position::default();
    let mut carriage_return = false;
    for &endpoint in endpoints
    {
        while characters.peek().is_some_and(|(offset, _)| *offset < endpoint)
        {
            let (_, character) = characters.next().unwrap();
            match character
            {
                '\r' => { position.line += 1; position.character = 0; }
                '\n' if carriage_return => {},
                '\n' => { position.line += 1; position.character = 0; }
                _ => position.character += character.len_utf16() as u32,
            }
            carriage_return = character == '\r';
        }
        result.insert(endpoint, position);
    }
    result
}


fn append_token(tokens: &mut Vec<u32>, start: Position, end: Position,
                kind: u32, previous: &mut Position)
{
    if start.line != end.line || start.character == end.character { return; }
    tokens.extend([start.line - previous.line,
                   if start.line == previous.line { start.character - previous.character }
                       else { start.character },
                   end.character - start.character, kind, 0]);
    *previous = start;
}


fn token_kind(kind: HighlightKind) -> Option<u32>
{
    if kind == HighlightKind::Plain { None } else { Some(kind as u32 - 1) }
}

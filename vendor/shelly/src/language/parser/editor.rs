
use crate::language::{ ast::{ AstStatement, AstTopLevel },
                       parser::{ results::{ ParserErrorKind, ParseResult },
                                 source::{ SyntaxFact, SyntaxMetadata },
                                 statements::parse_statement },
                       text::{ buffer::SimpleBuffer,
                               diagnostic::{ DiagnosticKind, DiagnosticPhase, SourceDiagnostic },
                               location::Location, span::SourceSpan },
                       tokenizer::{ source::LexicalMetadata, Token, TokenBuffer, TokenKind,
                                    Tokenizer, TokenizerError } };


const MAX_EDITOR_BYTES: usize = 65_536;
const MAX_EDITOR_STEPS: usize = 40_000;
const MAX_EDITOR_DEPTH: usize = 64;


#[derive(Clone, Debug)]
pub struct SyntaxRegion
{
    pub span: SourceSpan,
    pub status: Option<DiagnosticKind>,
}


pub struct EditorAnalysis
{
    pub tokens: Vec<Token>,
    pub lexical: LexicalMetadata,
    pub syntax: SyntaxMetadata,
    pub diagnostics: Vec<SourceDiagnostic>,
    pub regions: Vec<SyntaxRegion>,
    pub unparsed: Option<SourceSpan>,
}


pub(crate) struct FailureCandidate
{
    pub facts: Vec<SyntaxFact>,
    pub span: SourceSpan,
    pub incomplete: bool,
}


pub(crate) struct EditorFrame
{
    pub start: usize,
    pub checkpoint: usize,
    pub candidate: Option<FailureCandidate>,
}


pub(crate) struct EditorParser
{
    pub frames: Vec<EditorFrame>,
    pub diagnostics: Vec<SourceDiagnostic>,
    pub regions: Vec<SyntaxRegion>,
    pub steps: usize,
    pub depth: usize,
    pub blocks: usize,
    pub exhausted: bool,
    pub recovering: bool,
}


impl EditorParser
{
    pub fn new() -> Self
    {
        Self
            {
                frames: Vec::new(), diagnostics: Vec::new(), regions: Vec::new(),
                steps: MAX_EDITOR_STEPS, depth: 0, blocks: 0,
                exhausted: false, recovering: false,
            }
    }
}


/**
 * This entry point returns source information only. The grammar may use temporary
 * placeholders to finish a block, but no recovered AST can reach the compiler.
 */
pub fn analyze_editor(source: &str) -> EditorAnalysis
{
    analyze_editor_tree(source).0
}


// Recovered trees stay inside tooling and are never passed to compilation.
pub(crate) fn analyze_editor_tree(source: &str) -> (EditorAnalysis, AstTopLevel)
{
    analyze_editor_tree_with_limits(source, MAX_EDITOR_BYTES, MAX_EDITOR_STEPS)
}


pub(crate) fn analyze_editor_tree_with_tab_size(source: &str, width: Option<usize>)
    -> (EditorAnalysis, AstTopLevel)
{
    analyze_editor_tree_configured(source, MAX_EDITOR_BYTES, MAX_EDITOR_STEPS, width)
}


pub(crate) fn analyze_editor_tree_with_limits(source: &str, max_bytes: usize, max_steps: usize)
    -> (EditorAnalysis, AstTopLevel)
{
    analyze_editor_tree_configured(source, max_bytes, max_steps, None)
}


pub(crate) fn analyze_editor_tree_configured(source: &str, max_bytes: usize, max_steps: usize,
                                             tab_width: Option<usize>)
    -> (EditorAnalysis, AstTopLevel)
{
    let mut end = source.len().min(max_bytes);
    while !source.is_char_boundary(end) { end -= 1; }
    let mut input = SimpleBuffer::new("<editor>", &source[..end], tab_width);
    let mut tokenizer = Tokenizer::for_editor(&mut input);
    let mut buffer = TokenBuffer::for_editor(&mut tokenizer);
    if let Some(editor) = &mut buffer.editor { editor.steps = max_steps; }
    let mut statements = Vec::new();
    while !buffer.editor_exhausted()
    {
        match parse_statement(&mut buffer)
        {
            Ok(Some(statement)) => statements.push(statement),
            Ok(None) | Err(_) => break,
        }
    }
    let syntax = buffer.syntax_metadata().unwrap_or_default();
    let editor = buffer.take_editor().unwrap();
    drop(buffer);

    // A parser limit still leaves lexical information for the rest of the bounded input.
    while matches!(tokenizer.next_token(), Ok(Some(_))) {}
    let mut diagnostics = editor.diagnostics;
    if editor.exhausted && !diagnostics.iter().any(|issue| issue.kind == DiagnosticKind::Limit)
    {
        diagnostics.push(SourceDiagnostic
            {
                span: SourceSpan::new(end, end), kind: DiagnosticKind::Limit,
                phase: DiagnosticPhase::Parser, message: "Editor parser limit reached.".into(),
            });
    }
    diagnostics.extend_from_slice(tokenizer.editor_diagnostics());
    let unparsed = if end < source.len()
        {
            let span = SourceSpan::new(end, source.len());
            diagnostics.push(SourceDiagnostic
                {
                    span, kind: DiagnosticKind::Limit, phase: DiagnosticPhase::Parser,
                    message: "Editor source size limit reached.".into(),
                });
            Some(span)
        }
        else { None };
    diagnostics.sort_by_key(|diagnostic| (diagnostic.span.start, diagnostic.span.end));
    diagnostics.dedup();
    let mut regions = editor.regions;
    for region in &mut regions
    {
        for diagnostic in &diagnostics
        {
            if    (diagnostic.span.start < region.span.end
                    && diagnostic.span.end > region.span.start)
               || (diagnostic.span.start == diagnostic.span.end
                    && diagnostic.span.start >= region.span.start
                    && diagnostic.span.start <= region.span.end)
            {
                region.status = Some(match (region.status, diagnostic.kind)
                    {
                        (Some(DiagnosticKind::Limit), _) | (_, DiagnosticKind::Limit) =>
                            DiagnosticKind::Limit,
                        (Some(DiagnosticKind::Invalid), _) | (_, DiagnosticKind::Invalid) =>
                            DiagnosticKind::Invalid,
                        _ => DiagnosticKind::Incomplete,
                    });
            }
        }
    }
    regions.sort_by_key(|region| (region.span.start, region.span.end));
    (EditorAnalysis
        {
            tokens: tokenizer.editor_tokens().to_vec(),
            lexical: tokenizer.metadata().cloned().unwrap_or_default(),
            syntax, diagnostics, regions, unparsed,
        }, statements)
}


impl TokenBuffer<'_, '_>
{
    pub(crate) fn editor_exhausted(&self) -> bool
    { self.editor.as_ref().is_some_and(|editor| editor.exhausted) }

    pub(crate) fn take_editor(&mut self) -> Option<EditorParser> { self.editor.take() }

    pub(crate) fn with_editor_depth<T>(&mut self,
                                     parse: impl FnOnce(&mut Self) -> ParseResult<T>)
        -> ParseResult<T>
    {
        if let Some(editor) = &mut self.editor
        {
            if editor.depth >= MAX_EDITOR_DEPTH
            {
                editor.exhausted = true;
                return Err(self.editor_limit_error().into());
            }
            editor.depth += 1;
        }
        let result = parse(self);
        if let Some(editor) = &mut self.editor { editor.depth -= 1; }
        result
    }

    pub(crate) fn with_editor_block<T>(&mut self, parse: impl FnOnce(&mut Self) -> T) -> T
    {
        if let Some(editor) = &mut self.editor { editor.blocks += 1; }
        let result = parse(self);
        if let Some(editor) = &mut self.editor { editor.blocks -= 1; }
        result
    }

    pub(crate) fn editor_limit_error(&self) -> TokenizerError
    {
        TokenizerError { location: Location::new("<editor>", 1, 1),
            message: "Editor parser limit reached.".into() }
    }

    /**
     * Required grammar failures may unwind several token transactions. Keep the
     * furthest failed prefix separately; successful alternatives never publish it.
     */
    pub(crate) fn capture_editor_failure(&mut self)
    {
        let Some(editor) = &mut self.editor else { return; };
        let Some(frame) = editor.frames.last_mut() else { return; };
        let Some(journal) = &self.syntax else { return; };
        let span = self.last_read.unwrap_or(SourceSpan::new(frame.start, frame.start));
        let count = journal.entries.len().saturating_sub(frame.checkpoint);
        if frame.candidate.as_ref().is_none_or(|previous|
            span.end > previous.span.end
                || span.end == previous.span.end && count > previous.facts.len())
        {
            frame.candidate = Some(FailureCandidate
                {
                    facts: journal.entries[frame.checkpoint..].to_vec(),
                    span, incomplete: span.start == span.end,
                });
        }
    }

    pub(crate) fn capture_editor_expected(&mut self)
    {
        if !self.is_editor() { return; }
        self.mark_lookahead();
        let _ = self.next();
        self.capture_editor_failure();
        self.rollback_lookahead();
    }

    pub(crate) fn capture_editor_eof(&mut self)
    {
        if !self.is_editor() { return; }
        let previous = self.last_read;
        let end = self.editor_offset();
        self.last_read = Some(SourceSpan::new(end, end));
        self.capture_editor_failure();
        self.last_read = previous;
    }

    pub(crate) fn parse_editor_statement(&mut self,
        parse: fn(&mut Self) -> ParseResult<Option<AstStatement>>)
        -> ParseResult<Option<AstStatement>>
    {
        let start = self.peek_editor_token()?
            .map_or(self.editor_offset(), |token| token.span.start);
        let checkpoint = self.syntax_checkpoint();
        self.editor.as_mut().unwrap().frames.push(EditorFrame
            { start, checkpoint, candidate: None });
        self.mark_lookahead();
        let result = self.with_editor_depth(parse);
        if let Err(error) = &result
        {
            if matches!(error.kind, ParserErrorKind::ExpectedToken(_, None))
            { self.capture_editor_expected(); }
            self.capture_editor_failure();
        }
        let frame = self.editor.as_mut().unwrap().frames.pop().unwrap();
        if result.is_err() { self.rollback_lookahead(); }
        else { self.commit_lookahead(); }
        match result
        {
            Ok(None) => Ok(None),
            Ok(Some(statement)) =>
                {
                    if !matches!(statement, AstStatement::NullStatement)
                    { self.editor_region(start, None); }
                    Ok(Some(statement))
                },
            Err(error) =>
                {
                    let fallback = self.last_read.unwrap_or(SourceSpan::new(start, start));
                    let span = frame.candidate.as_ref()
                        .map_or(fallback, |candidate| candidate.span);
                    let incomplete = frame.candidate.as_ref().is_some_and(|item| item.incomplete);
                    if let Some(candidate) = frame.candidate && let Some(journal) = &mut self.syntax
                    {
                        journal.entries.truncate(checkpoint);
                        journal.entries.extend(candidate.facts);
                    }
                    let kind = if self.editor_exhausted()
                        {
                            DiagnosticKind::Limit
                        }
                        else if    incomplete
                                || matches!(error.kind, ParserErrorKind::UnexpectedEOF(_))
                        {
                            DiagnosticKind::Incomplete
                        }
                        else
                        {
                            DiagnosticKind::Invalid
                        };
                    self.editor.as_mut().unwrap().diagnostics.push(SourceDiagnostic
                        { span, kind, phase: DiagnosticPhase::Parser, message: error.to_string() });
                    if !self.editor_exhausted()
                    {
                        self.synchronize_editor(span.start, incomplete)?;
                        let end = self.last_read.map_or(start, |span| span.end);
                        if let Some(journal) = &mut self.syntax
                        {
                            let mut tail = journal.entries.split_off(checkpoint);
                            tail.retain(|fact| fact.span.end <= end);
                            journal.entries.extend(tail);
                        }
                    }
                    self.editor_region(start, Some(kind));
                    Ok(Some(AstStatement::NullStatement))
                }
        }
    }

    fn editor_region(&mut self, start: usize, status: Option<DiagnosticKind>)
    {
        let end = self.last_read.map_or(start, |span| span.end).max(start);
        self.editor.as_mut().unwrap().regions.push(SyntaxRegion
            { span: SourceSpan::new(start, end), status });
    }

    fn peek_editor_token(&mut self) -> Result<Option<Token>, TokenizerError>
    {
        self.mark_lookahead();
        let token = self.next();
        self.rollback_lookahead();
        token
    }

    fn synchronize_editor(&mut self, failure_start: usize, incomplete: bool)
        -> Result<(), TokenizerError>
    {
        self.editor.as_mut().unwrap().recovering = true;
        let result = self.synchronize_editor_inner(failure_start, incomplete);
        self.editor.as_mut().unwrap().recovering = false;
        result
    }

    fn synchronize_editor_inner(&mut self, failure_start: usize, incomplete: bool)
        -> Result<(), TokenizerError>
    {
        // Leave an enclosing '}' for its block rule, but always consume stray top-level closers.
        let mut braces = 0;
        while let Some(token) = self.peek_editor_token()?
        {
            if token.span.start >= failure_start { break; }
            self.next()?;
            match token.kind
            {
                TokenKind::BlockOpen => braces += 1,
                TokenKind::BlockClose if braces > 0 => braces -= 1,
                TokenKind::LineBreak | TokenKind::StatementBreak if !incomplete && braces == 0 =>
                    return Ok(()),
                _ => {},
            }
        }
        while let Some(token) = self.peek_editor_token()?
        {
            if    token.kind == TokenKind::BlockClose
               && braces == 0
               && self.editor.as_ref().unwrap().blocks > 0
            { return Ok(()); }
            self.next()?;
            match token.kind
            {
                TokenKind::BlockOpen => braces += 1,
                TokenKind::BlockClose if braces > 0 => braces -= 1,
                TokenKind::LineBreak | TokenKind::StatementBreak if braces == 0 => break,
                _ => {},
            }
        }
        Ok(())
    }
}

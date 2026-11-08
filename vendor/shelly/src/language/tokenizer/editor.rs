
use crate::language::{ text::{ buffer::Buffer,
                               diagnostic::{ DiagnosticKind, DiagnosticPhase, SourceDiagnostic },
                               location::Location, span::SourceSpan },
                       tokenizer::{ Token, TokenKind, TokenValue, Tokenizer, TokenizerError } };


pub(super) struct EditorLexer
{
    start: (Location, usize, TokenKind),
    pub tokens: Vec<Token>,
    pub diagnostics: Vec<SourceDiagnostic>,
    stopped: bool,
}


impl<'a> Tokenizer<'a>
{
    pub(crate) fn for_editor(input: &'a mut dyn Buffer) -> Self
    {
        let mut tokenizer = Self::with_metadata(input);
        tokenizer.editor = Some(EditorLexer
            {
                start: (Location::default(), 0, TokenKind::Invalid),
                tokens: Vec::new(),
                diagnostics: Vec::new(),
                stopped: false,
            });
        tokenizer
    }

    pub(crate) fn editor_tokens(&self) -> &[Token]
    { self.editor.as_ref().map_or(&[], |editor| editor.tokens.as_slice()) }

    pub(crate) fn editor_diagnostics(&self) -> &[SourceDiagnostic]
    { self.editor.as_ref().map_or(&[], |editor| editor.diagnostics.as_slice()) }

    pub(super) fn editor_start(&mut self, kind: TokenKind)
    {
        if self.editor.is_some()
        { self.editor_start_at(self.input.location().clone(), self.input.byte_offset(), kind); }
    }

    pub(super) fn editor_start_at(&mut self, location: Location, start: usize, kind: TokenKind)
    {
        if let Some(editor) = &mut self.editor { editor.start = (location, start, kind); }
    }

    pub(super) fn editor_issue(&mut self, span: SourceSpan, kind: DiagnosticKind,
                              message: impl Into<String>)
    {
        if let Some(editor) = &mut self.editor
        {
            editor.diagnostics.push(SourceDiagnostic
                { span, kind, phase: DiagnosticPhase::Tokenizer, message: message.into() });
        }
    }

    /**
     * Reuse the normal token rules. A damaged word becomes a bounded source token;
     * a permanent read error stops recovery rather than being retried forever.
     */
    pub(super) fn next_editor_token(&mut self) -> Result<Option<Token>, TokenizerError>
    {
        if self.editor.as_ref().is_some_and(|editor| editor.stopped) { return Ok(None); }
        self.editor_start(TokenKind::Invalid);
        let result = match self.next_strict_token()
        {
            Ok(result) => result,
            Err(error) =>
                {
                    let (location, start, mut kind) = self.editor.as_ref().unwrap().start.clone();
                    let end = self.word_span(start).end;
                    let read_error = self.input.read_error().is_some();
                    let incomplete = !read_error && self.input.peek_next().is_none()
                        && matches!(kind, TokenKind::Identifier | TokenKind::Member
                            | TokenKind::LineContinue);
                    let severity = if incomplete { DiagnosticKind::Incomplete }
                        else { DiagnosticKind::Invalid };
                    self.editor_issue(SourceSpan::new(start, end), severity, error.message);
                    if read_error { self.editor.as_mut().unwrap().stopped = true; }
                    if start == end { return Ok(None); }
                    if !incomplete { kind = TokenKind::Invalid; }
                    let value = match kind
                    {
                        TokenKind::Identifier => TokenValue::Identifier("$".into()),
                        TokenKind::Member => TokenValue::Symbol(String::new()),
                        _ => TokenValue::None,
                    };
                    Some(Token { location, span: SourceSpan::new(start, end), kind, value })
                }
        };
        if let Some(token) = &result { self.editor.as_mut().unwrap().tokens.push(token.clone()); }
        Ok(result)
    }
}

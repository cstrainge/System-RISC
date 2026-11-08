use crate::language::{parser::{editor::EditorParser,
                               source::{SyntaxContext, SyntaxFact, SyntaxJournal,
                                        SyntaxMetadata, SyntaxRole}},
                      text::{diagnostic::{DiagnosticKind, DiagnosticPhase, SourceDiagnostic},
                             layout::LayoutPosition,
                             location::Location,
                             span::SourceSpan},
                      tokenizer::tokenizer::{Token, TokenKind, Tokenizer, TokenizerError}};


#[derive(Clone, Copy)]
struct Transaction
{
    position:          usize,
    syntax_checkpoint: usize,
    last_read:         Option<SourceSpan>
}

/**
 * Simple buffer that provides lookahead functionality for a parser. The tokenizer just focuses
 * on producing tokens, while this buffer allows the parser to peek ahead without consuming them
 * immediately.
 *
 * It also features transactional capabilities, allowing the parser to start a transaction,
 * make speculative reads, and either commit or rollback based on the parsing outcome.
 *
 * This allows the parser to try out different parsing strategies without permanently consuming
 * tokens, providing flexibility in handling complex grammar rules. If a rule doesn't work out
 * as expected, the parser can rollback to a previous state and try an alternative approach.
 *
 * Otherwise it can commit the current transaction, making all speculative reads permanent.
 */
pub struct TokenBuffer<'tokenizer, 'input>
{
    /**
     * The underlying tokenizer that produces tokens for this buffer.
     */
    tokenizer: &'tokenizer mut Tokenizer<'input>,

    /**
     * Buffer that holds tokens for lookahead purposes. Tokens are read from the underlying
     * tokenizer as needed for lookahead operations.
     */
    lookahead_buffer: Vec<Option<Token>>,

    /**
     * The position of the current cursor within the lookahead buffer. It represents the
     * current toplevel transaction within the lookahead stack.
     */
    position: usize,

    /**
     * Stack of cursors representing nested lookahead transactions. Each entry corresponds to a
     * saved position in the lookahead buffer, allowing the parser to rollback to previous
     * states if needed.
     */
    lookahead_stack:      Vec<Transaction>,
    pub(crate) syntax:    Option<SyntaxJournal>,
    context:              SyntaxContext,
    pub(crate) editor:    Option<EditorParser>,
    pub(crate) last_read: Option<SourceSpan>
}


/**
 * An estimate of the initial lookahead buffer capacity. We're hoping we don't need to resize it
 * frequently during parsing. If we do end up resizing it it isn't the end of the world.
 */
const LOOKAHEAD_BUFFER_CAPACITY: usize = 50;


impl<'tokenizer, 'input> TokenBuffer<'tokenizer, 'input>
{
    /**
     * Initialize a `TokenBuffer` with the given tokenizer. Ready to read from.
     */
    pub fn new(tokenizer: &'tokenizer mut Tokenizer<'input>) -> Self
    {
        Self { tokenizer,
               lookahead_buffer: Vec::with_capacity(LOOKAHEAD_BUFFER_CAPACITY),
               position: 0,
               lookahead_stack: Vec::with_capacity(LOOKAHEAD_BUFFER_CAPACITY),
               syntax: None,
               context: SyntaxContext::Expression,
               editor: None,
               last_read: None }
    }

    pub fn with_syntax(tokenizer: &'tokenizer mut Tokenizer<'input>) -> Self
    {
        let mut buffer = Self::new(tokenizer);
        buffer.syntax = Some(SyntaxJournal::default());
        buffer
    }

    pub(crate) fn for_editor(tokenizer: &'tokenizer mut Tokenizer<'input>) -> Self
    {
        let mut buffer = Self::with_syntax(tokenizer);
        buffer.editor = Some(EditorParser::new());
        buffer
    }

    pub(crate) fn is_editor(&self) -> bool
    {
        self.editor.is_some()
    }

    pub(crate) fn layout_diagnostic(&mut self, location: &Location, message: &str)
    {
        let Some(editor) = &mut self.editor
        else
        {
            return;
        };
        let span = self.tokenizer
                       .editor_tokens()
                       .iter()
                       .find(|token| token.location == *location)
                       .map(|token| token.span)
                       .unwrap_or_default();
        editor.diagnostics.push(SourceDiagnostic { span,
                                                   kind: DiagnosticKind::Invalid,
                                                   phase: DiagnosticPhase::Parser,
                                                   message: message.into() });
    }

    pub(crate) fn anchor(&self, token: &Token) -> LayoutPosition
    {
        self.tokenizer
            .anchor(token.span)
            .unwrap_or(LayoutPosition { column:    token.location.column,
                                        tab_width: 4,
                                        hard_tabs: false })
    }

    pub(crate) fn editor_offset(&self) -> usize
    {
        self.tokenizer.byte_offset()
    }

    pub fn syntax_metadata(&self) -> Option<SyntaxMetadata>
    {
        self.syntax.as_ref().map(SyntaxJournal::snapshot)
    }

    pub(crate) fn syntax_checkpoint(&self) -> usize
    {
        self.syntax.as_ref().map_or(0, |journal| journal.entries.len())
    }

    pub(crate) fn syntax_context(&self) -> SyntaxContext
    {
        self.context
    }

    pub(crate) fn with_context<T>(&mut self, context: SyntaxContext,
                                  parse: impl FnOnce(&mut Self) -> T)
                                  -> T
    {
        let previous = self.context;
        self.context = context;
        let result = parse(self);
        self.context = previous;
        result
    }

    pub(crate) fn annotate(&mut self, span: SourceSpan, role: SyntaxRole)
    {
        if let Some(journal) = &mut self.syntax
        {
            journal.entries.push(SyntaxFact { span,
                                              role,
                                              context: self.context });
        }
    }

    pub(crate) fn annotate_word(&mut self, span: SourceSpan)
    {
        self.annotate(span, self.word_role());
    }

    fn word_role(&self) -> SyntaxRole
    {
        match self.context
        {
            SyntaxContext::Argument => SyntaxRole::Argument,
            SyntaxContext::Command => SyntaxRole::CommandName,
            SyntaxContext::Type => SyntaxRole::TypeName,
            SyntaxContext::Expression => SyntaxRole::Name
        }
    }

    pub(crate) fn annotate_names_since(&mut self, checkpoint: usize, role: SyntaxRole)
    {
        if let Some(journal) = &mut self.syntax
        {
            let end = journal.entries.len();
            for index in checkpoint..end
            {
                let fact = journal.entries[index];
                if matches!(fact.role,
                            SyntaxRole::Name
                            | SyntaxRole::CommandName
                            | SyntaxRole::Argument
                            | SyntaxRole::TypeName)
                {
                    journal.entries.push(SyntaxFact { role,
                                                      ..fact });
                }
            }
        }
    }

    fn record_token(&mut self, token: &Token)
    {
        if self.syntax.is_none()
        {
            return;
        }
        if self.editor.as_ref().is_some_and(|editor| editor.recovering)
        {
            return;
        }
        let role = match token.kind
        {
            TokenKind::Invalid => SyntaxRole::Invalid,
            TokenKind::Symbol => self.word_role(),
            TokenKind::Identifier | TokenKind::AutoIdentifier => SyntaxRole::Variable,
            TokenKind::Member => SyntaxRole::Member,
            TokenKind::Literal => SyntaxRole::Literal,
            TokenKind::Let
            | TokenKind::Export
            | TokenKind::Public
            | TokenKind::Alias
            | TokenKind::Sub
            | TokenKind::If
            | TokenKind::Else
            | TokenKind::While
            | TokenKind::Until
            | TokenKind::For
            | TokenKind::In
            | TokenKind::Break
            | TokenKind::Continue
            | TokenKind::Loop
            | TokenKind::Match
            | TokenKind::Return
            | TokenKind::Function
            | TokenKind::Struct
            | TokenKind::Enum
            | TokenKind::Type
            | TokenKind::Import => SyntaxRole::Keyword,
            TokenKind::Comma
            | TokenKind::BlockOpen
            | TokenKind::BlockClose
            | TokenKind::EmptyBlock
            | TokenKind::SquareOpen
            | TokenKind::SquareClose
            | TokenKind::IndexOpen
            | TokenKind::ParenOpen
            | TokenKind::ParenClose
            | TokenKind::StatementBreak
            | TokenKind::LineBreak
            | TokenKind::LineContinue => SyntaxRole::Delimiter,
            _ => SyntaxRole::Operator
        };
        self.annotate(token.span, role);
    }

    /**
     * Mark the current position in the lookahead buffer, starting a new lookahead transaction.
     */
    pub fn mark_lookahead(&mut self)
    {
        // Do we currently have a lookahead transaction active? If so, use the front of the stack
        // as the current lookahead position. Otherwise, use the current position.
        let lookahead = if self.is_peeking()
        {
            self.lookahead_stack.last().map(|frame| frame.position)
        }
        else
        {
            Some(self.position)
        };

        if let Some(lookahead) = lookahead
        {
            self.lookahead_stack.push(Transaction { position:          lookahead,
                                                    syntax_checkpoint: self.syntax_checkpoint(),
                                                    last_read:         self.last_read });
        }
    }

    /**
     * Commit the current lookahead transaction, making its changes permanent.
     */
    pub fn commit_lookahead(&mut self)
    {
        if let Some(lookahead) = self.lookahead_stack.pop()
        {
            if let Some(parent) = self.lookahead_stack.last_mut()
            {
                parent.position = lookahead.position;
            }
            else
            {
                self.position = lookahead.position;
                self.clear_finished_buffer();
            }
        }
    }

    /**
     * Rollback the current lookahead transaction, reverting any changes made since it was
     * marked.
     */
    pub fn rollback_lookahead(&mut self)
    {
        if let Some(frame) = self.lookahead_stack.pop()
        {
            self.last_read = frame.last_read;
            if let Some(journal) = &mut self.syntax
            {
                journal.entries.truncate(frame.syntax_checkpoint);
            }
            self.clear_finished_buffer();
        }
    }

    /**
     * Extract the next token from the buffer or tokenizer. if there is a lookahead transaction
     * active then this isn't a permanent change.
     */
    pub fn next(&mut self) -> Result<Option<Token>, TokenizerError>
    {
        self.step()?;
        let index = self.position();

        let token = if index >= self.lookahead_buffer.len()
        {
            match self.tokenizer.next_token()?
            {
                Some(token) if !self.is_peeking() => token,
                Some(token) =>
                {
                    self.lookahead_buffer.push(Some(token));
                    self.cached_token(index)?
                }
                None =>
                {
                    if self.is_editor()
                    {
                        let end = self.tokenizer.byte_offset();
                        self.last_read = Some(SourceSpan::new(end, end));
                    }
                    return Ok(None);
                }
            }
        }
        else
        {
            self.cached_token(index)?
        };
        if self.is_editor()
        {
            self.last_read = Some(token.span);
        }

        if let Some(cursor) = self.lookahead_stack.last_mut()
        {
            cursor.position += 1;
        }
        else
        {
            self.position += 1;
            self.clear_finished_buffer();
        }

        self.record_token(&token);
        Ok(Some(token))
    }

    // Peek without copying a payload, advancing a transaction or creating a
    // mismatch error. Lexed tokens remain available to ordinary consumption.
    pub(crate) fn peek_kind(&mut self) -> Result<Option<TokenKind>, TokenizerError>
    {
        self.step()?;
        let index = self.position();
        if index >= self.lookahead_buffer.len()
        {
            let Some(token) = self.tokenizer.next_token()?
            else
            {
                return Ok(None);
            };
            self.lookahead_buffer.push(Some(token));
        }
        self.lookahead_buffer
            .get(index)
            .and_then(Option::as_ref)
            .map(|token| Some(token.kind))
            .ok_or_else(|| self.missing_cached_token())
    }

    fn cached_token(&mut self, index: usize) -> Result<Token, TokenizerError>
    {
        let token = if self.is_peeking()
        {
            self.lookahead_buffer.get(index).and_then(Option::as_ref).cloned()
        }
        else
        {
            // No live transaction can revisit a committed token. Move its
            // original payload instead of cloning and later discarding it.
            self.lookahead_buffer.get_mut(index).and_then(Option::take)
        };
        token.ok_or_else(|| self.missing_cached_token())
    }

    fn missing_cached_token(&self) -> TokenizerError
    {
        TokenizerError { location: self.tokenizer.input.location().clone(),
                         message:
                             "Internal tokenizer cache has no token at the active cursor.".into() }
    }

    fn step(&mut self) -> Result<(), TokenizerError>
    {
        if let Some(editor) = &mut self.editor
        {
            if editor.steps == 0
            {
                editor.exhausted = true;
                return Err(self.editor_limit_error());
            }
            editor.steps -= 1;
        }
        Ok(())
    }

    /**
     * Current cursor within the token cache, including lookahead. Positions can
     * be compared while a lookahead transaction remains active; cache cleanup
     * outside a transaction may reset the cursor.
     */
    pub(crate) fn position(&self) -> usize
    {
        self.lookahead_stack.last().map_or(self.position, |frame| frame.position)
    }

    /**
     * Check if there is an active lookahead transaction. Returns `true` if there is, `false`
     * otherwise.
     */
    fn is_peeking(&self) -> bool
    {
        !self.lookahead_stack.is_empty()
    }

    /**
     * Clear out the finished portion of the lookahead buffer.
     */
    fn clear_finished_buffer(&mut self)
    {
        if self.position >= self.lookahead_buffer.len() && !self.is_peeking()
        {
            self.lookahead_buffer.clear();
            self.position = 0;
        }
    }
}

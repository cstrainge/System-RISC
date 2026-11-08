
use std::{ collections::HashMap, fmt::{ self, Display, Formatter } };

use crate::language::{ text::{ buffer::Buffer, diagnostic::DiagnosticKind,
                               location::Location, layout::LayoutPosition, span::SourceSpan },
                       tokenizer::{ editor::EditorLexer,
                                    source::{ LexicalMetadata, Trivia, TriviaKind,
                                              StringSource, StringPart, StringPartKind } } };

/**
 * Represents all the different types of tokens that can be recognized in the Shelly script
 * language.
 */
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind
{
    // An editor-only damaged lexeme. Execution never produces this token.
    Invalid,
    Member,

    /**
     * A simple literal value, such as a number or string.
     */
    Literal,

    /**
     * A variable identifier and always starts with a $ character.
     */
    Identifier,

    /**
     * A symbol text representing blocks of text as found during parsing. Can be a name of an
     * executable or a command line parameter being passed to something. These can end up resolving
     * to simple strings.
     */
    Symbol,

    /**
     * The auto variable that is automatically supplied by the language in certain contexts.
     */
    AutoIdentifier,

    /**
     * The `let` keyword used for variable declaration.
     */
    Let,

    /**
     * The `export` keyword used for marking variables as exported.
     */
    Export,

    /** The `pub` keyword marks a module declaration as public. */
    Public,

    /**
     * The `alias` keyword used for defining an alias.
     */
    Alias,

    /**
     * The `,` character used to separate items in a list or function parameters.
     */
    Comma,

    /**
     * The `sub` keyword used for defining a sub-process block.
     */
    Sub,

    /**
     * The `if` keyword used for conditional branching.
     */
    If,

    /**
     * The `else` keyword used for conditional branching when the `if` condition is not met.
     */
    Else,

    /**
     * The `while` keyword used for looping while a condition is true.
     */
    While,
    Until,
    For,
    In,
    Break,
    Continue,

    /**
     * A unbound loop construct used for indefinite looping until explicitly broken.
     */
    Loop,

    /**
     * The `match` keyword used for pattern matching against values.
     */
    Match,

    /**
     * The `return` keyword used for returning a value from a function.
     */
    Return,
    Error,
    Try,
    Catch,
    Finally,
    Throw,

    /**
     * Keyword used for defining a function.
     */
    Function,

    /**
     * The `struct` keyword used for defining a new structure.
     */
    Struct,

    /**
     * The `enum` keyword used for defining a new enumeration.
     */
    Enum,

    /**
     * The `type` keyword used for defining a new type alias.
     */
    Type,

    /**
     * Keyword used for importing modules or other resources into the current scope.
     */
    Import,

    /**
     * The `:` character used for type annotations or other type-related syntax.
     */
    TypeDelimiter,

    /**
     * The `=` character used for assignment of values to variables.
     */
    Assign,
    MatchArrow,

    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    And,
    Or,
    Not,
    Range,
    RangeInclusive,

    /**
     * The `-` character used for subtraction or negation.
     */
    Minus,

    /**
     * The `+` character used for addition.
     */
    Plus,

    /**
     * The `*` character used for multiplication or file gathering operations.
     */
    Asterisk,

    /**
     * The `**` character used for globbing or file pattern matching.
     */
    Glob,

    /**
     * The `/` character used for division or file path operations.
     */
    Slash,

    /**
     * The `%` character used for modulo operations or other percentage-related syntax.
     */
    Percent,

    /**
     * The `;` character used to indicate the end of a statement, without requiring a newline.
     */
    StatementBreak,

    /**
     * The `\n` character used to indicate a line break in the source code. If the file includes any
     * `\r` characters they are ignored.
     */
    LineBreak,

    /**
     * The `\` character used to indicate that the current line continues onto the next line. It is
     * illegal for anything other than whitespace and comments to follow it.
     */
    LineContinue,

    /**
     * The scope symbol, `::` used to indicate the a scope delimiter for a keyword or to indicate
     * the executable should be taken from an outer scope.
     */
    Scope,

    /**
     * The `\`` character used to indicate that we're taking the value of an executable instead of
     * actually executing it.
     *
     * Used for assigning functions and executable references to variables for later use.
     */
    ExecEscape,

    /**
     * Open a block of code with `{`.
     */
    BlockOpen,

    /**
     * Close a block of code with `}`.
     */
    BlockClose,

    /**
     * Represents an empty block of code, `{}`.
     */
    EmptyBlock,

    /**
     * Open a square bracket with `[`. Used for array indexing or defining array literals.
     */
    SquareOpen,

    /**
     * An opening bracket immediately following a value, without whitespace.
     */
    IndexOpen,

    /**
     * Close a square bracket with `]`. Used for array indexing or defining array literals.
     */
    SquareClose,

    /**
     * Open a parenthesis with `(`. Used for grouping expressions, defining function parameters, and
     * grouping expressions/executable calls.
     */
    ParenOpen,

    /**
     * Close a parenthesis closing a group of expressions or an executable call.
     */
    ParenClose,

    /**
     * Symbol for piping IO between commands, represented by the `|` character.
     */
    Pipe,
    Branch,

    /**
     * Represents stderr in the source code, represented by the `~` character.
     */
    ErrorSource,

    /**
     * Represents redirection in the source code to a file or variable, the `->` symbol.
     */
    RedirectTo,

    /**
     * Represents redirection of stderr to a file or variable, the `~->` symbol.
     */
    RedirectErrorTo,

    /**
     * Represents redirection of both stdout and stderr to a file or variable, the `~+->` symbol.
     */
    RedirectBothTo,

    /**
     * Represents the splat operator, `...`, used for argument expansion.
     */
    Splat,
}


impl Display for TokenKind
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result
    {
        write!(f, "{:?}", self)
    }
}


/**
 * Indicate if the string should participate in interpolation or not.
 */
#[derive(Clone, PartialEq, Eq)]
pub enum StringFlag
{
    Interpolated,
    NonInterpolated
}


/**
 * Represents the different types of literals that can be associated with a token.
 */
#[derive(Clone)]
pub enum TokenLiteral
{
    /**
     * An integer literal value.
     */
    Integer(i64, String),

    /**
     * A floating-point literal value.
     */
    Float(f64, String),

    /**
     * A boolean literal value.
     */
    Boolean(bool),

    /**
     * A string literal value.
     */
    // Byte offsets of escaped dollars retain literal/interpolated boundaries after decoding.
    String(String, StringFlag, Vec<usize>)
}


impl Display for TokenLiteral
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result
    {
        match self
        {
            TokenLiteral::Integer(value, text) => write!(f, "{}:{}", value, text),
            TokenLiteral::Float(value, text)   => write!(f, "{}:{}", value, text),
            TokenLiteral::Boolean(value)       => write!(f, "{}", value),
            TokenLiteral::String(text, _, _)   => write!(f, "{:?}", text)
        }
    }
}


/**
 * Represents a value that can be associated with a token, such as a literal, identifier, or none.
 */
#[derive(Clone)]
pub enum TokenValue
{
    None,
    Literal(TokenLiteral),
    Identifier(String),
    Symbol(String)
}


impl Display for TokenValue
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result
    {
        match self
        {
            TokenValue::None                   => write!(f, "_"),
            TokenValue::Literal(literal)       => write!(f, "{}", literal),
            TokenValue::Identifier(identifier) => write!(f, "{}", identifier),
            TokenValue::Symbol(symbol)         => write!(f, "{}", symbol)
        }
    }
}


/**
 * Represents a single token of language as extracted from the input source code.
 */
#[derive(Clone)]
pub struct Token
{
    /**
     * The logical location in the source code this token was extracted from.
     */
    pub location: Location,

    /**
     * Exact half-open byte range, including original quotes and escapes for strings.
     */
    pub span: SourceSpan,

    /**
     * The type of the token, indicating its role in the language syntax.
     */
    pub kind: TokenKind,

    /**
     * The value of the token, which may be a literal, identifier, or none depending on the token
     * type.
     */
    pub value: TokenValue
}


impl Token
{
    /**
     * Returns the textual representation of the token's original value.
     */
    pub fn token_value_text(&self) -> String
    {
        match &self.kind
        {
            TokenKind::Literal =>
                {
                    if let TokenValue::Literal(literal) = &self.value
                    {
                        match literal
                        {
                            TokenLiteral::Integer(_, text) => return text.clone(),
                            TokenLiteral::Float(_, text)   => return text.clone(),
                            TokenLiteral::Boolean(value)   => return value.to_string(),
                            TokenLiteral::String(text, _, _) => return text.clone()
                        }
                    }
                    else
                    {
                        panic!("A literal token does not have a literal value.");
                    }
                },

            TokenKind::Identifier =>
                {
                    if let TokenValue::Identifier(text) = &self.value
                    {
                        text.clone()
                    }
                    else
                    {
                        panic!("An identifier token does not have an identifier value.");
                    }
                },

            TokenKind::Symbol | TokenKind::Member =>
                {
                    if let TokenValue::Symbol(text) = &self.value
                    {
                        text.clone()
                    }
                    else
                    {
                        panic!("A symbol token does not have a symbol value.");
                    }
                },

            TokenKind::Invalid           => String::new(),
            TokenKind::AutoIdentifier    => "$".to_string(),
            TokenKind::Let               => "let".to_string(),
            TokenKind::Export            => "export".to_string(),
            TokenKind::Public            => "pub".to_string(),
            TokenKind::Alias             => "alias".to_string(),
            TokenKind::Comma             => ",".to_string(),
            TokenKind::Sub               => "sub".to_string(),
            TokenKind::If                => "if".to_string(),
            TokenKind::Else              => "else".to_string(),
            TokenKind::While             => "while".to_string(),
            TokenKind::Until             => "until".to_string(),
            TokenKind::For               => "for".to_string(),
            TokenKind::In                => "in".to_string(),
            TokenKind::Break             => "break".to_string(),
            TokenKind::Continue          => "continue".to_string(),
            TokenKind::Loop              => "loop".to_string(),
            TokenKind::Match             => "match".to_string(),
            TokenKind::Return            => "return".to_string(),
            TokenKind::Error             => "error".to_string(),
            TokenKind::Try               => "try".to_string(),
            TokenKind::Catch             => "catch".to_string(),
            TokenKind::Finally           => "finally".to_string(),
            TokenKind::Throw             => "throw".to_string(),
            TokenKind::Function          => "fn".to_string(),
            TokenKind::Struct            => "struct".to_string(),
            TokenKind::Enum              => "enum".to_string(),
            TokenKind::Type              => "type".to_string(),
            TokenKind::Import            => "import".to_string(),
            TokenKind::TypeDelimiter     => ":".to_string(),
            TokenKind::Assign            => "=".to_string(),
            TokenKind::MatchArrow        => "=>".to_string(),
            TokenKind::Equal             => "==".to_string(),
            TokenKind::NotEqual          => "!=".to_string(),
            TokenKind::Less              => "<".to_string(),
            TokenKind::LessEqual         => "<=".to_string(),
            TokenKind::Greater           => ">".to_string(),
            TokenKind::GreaterEqual      => ">=".to_string(),
            TokenKind::And               => "&&".to_string(),
            TokenKind::Or                => "||".to_string(),
            TokenKind::Not               => "!".to_string(),
            TokenKind::Minus             => "-".to_string(),
            TokenKind::Plus              => "+".to_string(),
            TokenKind::Asterisk          => "*".to_string(),
            TokenKind::Glob              => "**".to_string(),
            TokenKind::Slash             => "/".to_string(),
            TokenKind::Percent           => "%".to_string(),
            TokenKind::StatementBreak    => ";".to_string(),
            TokenKind::LineBreak         => "\n".to_string(),
            TokenKind::LineContinue      => "\\".to_string(),
            TokenKind::Scope             => "::".to_string(),
            TokenKind::ExecEscape        => "`".to_string(),
            TokenKind::BlockOpen         => "{".to_string(),
            TokenKind::BlockClose        => "}".to_string(),
            TokenKind::EmptyBlock        => "{}".to_string(),
            TokenKind::SquareOpen        => "[".to_string(),
            TokenKind::IndexOpen         => "[".to_string(),
            TokenKind::SquareClose       => "]".to_string(),
            TokenKind::ParenOpen         => "(".to_string(),
            TokenKind::ParenClose        => ")".to_string(),
            TokenKind::Pipe              => "|".to_string(),
            TokenKind::Branch            => "\\-".to_string(),
            TokenKind::ErrorSource       => "~".to_string(),
            TokenKind::RedirectTo        => "->".to_string(),
            TokenKind::RedirectErrorTo   => "~->".to_string(),
            TokenKind::RedirectBothTo    => "~+->".to_string(),
            TokenKind::Splat             => "...".to_string(),
            TokenKind::Range             => "..".to_string(),
            TokenKind::RangeInclusive    => "..=".to_string()
        }
    }
}


impl Display for Token
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result
    {
        write!(f, "{}: ({}, {})", self.location, self.kind, self.value)
    }
}


/**
 * Represents an error encountered during tokenization.
 */
#[derive(Clone)]
pub struct TokenizerError
{
    /**
     * The logical location in the source code the error occurred at.
     */
    pub location: Location,

    /**
     * A descriptive message explaining the error.
     */
    pub message: String
}


impl Display for TokenizerError
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result
    {
        write!(f, "TokenizerError at {}: {}", self.location, self.message)
    }
}


/**
 * Indicates whether comments should be skipped during tokenization.
 */
#[derive(PartialEq, Eq, Clone, Copy)]
enum SkipComments
{
    /**
     * Skip comments during tokenization.
     */
    Yes,

    /**
     * Do not skip comments during tokenization.
     */
    No
}


#[derive(PartialEq, Eq, Clone, Copy)]
enum SkipNewlines
{
    /**
     * Skip newlines during tokenization.
     */
    Yes,

    /**
     * Do not skip newlines during tokenization.
     */
    No
}


/**
 * A Shelly script tokenizer that lazily extracts tokens from the input source code buffer as
 * needed.
 */
pub struct Tokenizer<'a>
{
    /**
     * The source code input buffer to read our tokens from.
     */
    pub(super) input: &'a mut dyn Buffer,
    // Reading a word can consume the following operator.
    pending_operator: Option<Token>,
    anchors: HashMap<usize, LayoutPosition>,
    header: bool,
    tab_directive: bool,
    // A prefetched negative operand distinguishes `a<-2` from removed `<-` arrows.
    pending_operand: Option<Token>,
    index_follows: bool,
    member_follows: bool,
    enum_variant_follows: bool,
    metadata: Option<LexicalMetadata>,
    active_string: Option<usize>,
    pub(super) editor: Option<EditorLexer>,
}


impl<'a> Tokenizer<'a>
{
    /// Opt into lexical facts for analysis. Normal execution allocates no sidecar metadata.
    pub fn with_metadata(input: &'a mut dyn Buffer) -> Self
    {
        let mut tokenizer = Self::new(input);
        tokenizer.metadata = Some(LexicalMetadata::default());
        tokenizer
    }

    pub fn metadata(&self) -> Option<&LexicalMetadata> { self.metadata.as_ref() }

    pub fn byte_offset(&self) -> usize { self.input.byte_offset() }

    pub(crate) fn anchor(&self, span: SourceSpan) -> Option<LayoutPosition>
    { self.anchors.get(&span.start).copied() }

    pub(super) fn word_span(&self, start: usize) -> SourceSpan
    {
        // Word extraction may have already consumed the following operator or member.
        // Its pending token owns those bytes, not the word being returned now.
        SourceSpan::new(start, self.pending_operator.as_ref()
            .map_or(self.input.byte_offset(), |token| token.span.start))
    }

    fn record_trivia(&mut self, kind: TriviaKind, start: usize)
    {
        if let Some(metadata) = &mut self.metadata && self.active_string.is_none()
        {
            let span = SourceSpan::new(start, self.input.byte_offset());
            if    let Some(previous) = metadata.trivia.last_mut()
               && previous.kind == kind
               && previous.span.end == span.start
            { previous.span.end = span.end; }
            else { metadata.trivia.push(Trivia { kind, span }); }
        }
    }

    fn begin_string(&mut self, start: usize, flag: &StringFlag)
    {
        if let Some(metadata) = &mut self.metadata
        {
            self.active_string = Some(metadata.strings.len());
            let opening = SourceSpan::new(start, self.input.byte_offset());
            metadata.strings.push(StringSource
                {
                    span: opening,
                    opening,
                    closing: None,
                    interpolated: *flag == StringFlag::Interpolated,
                    decoded: String::new(),
                    parts: Vec::new(),
                });
        }
    }

    fn push_string(&mut self, text: &mut String, character: char,
                   source: SourceSpan, kind: StringPartKind)
    {
        let decoded = SourceSpan::new(text.len(), text.len() + character.len_utf8());
        text.push(character);
        if let Some(index) = self.active_string && let Some(metadata) = &mut self.metadata
        {
            let string = &mut metadata.strings[index];
            string.decoded.push(character);
            if    let Some(previous) = string.parts.last_mut()
               && kind == StringPartKind::Text
               && previous.kind == kind
               && previous.source.end == source.start
               && previous.decoded.end == decoded.start
            {
                previous.source.end = source.end;
                previous.decoded.end = decoded.end;
            }
            else { string.parts.push(StringPart { source, decoded, kind }); }
        }
    }

    fn close_string(&mut self, start: usize)
    {
        if let Some(index) = self.active_string.take() && let Some(metadata) = &mut self.metadata
        {
            let string = &mut metadata.strings[index];
            string.span.end = self.input.byte_offset();
            string.closing = Some(SourceSpan::new(start, string.span.end));
        }
    }

    /**
     * Construct a new tokenizer with the given input buffer.
     */
    pub fn new(input: &'a mut dyn Buffer) -> Self
    {
        Self { input, pending_operator: None, pending_operand: None, index_follows: false,
            member_follows: false, enum_variant_follows: false,
            metadata: None, active_string: None, editor: None,
            anchors: HashMap::new(), header: true, tab_directive: false }
    }

    pub fn has_tab_directive(&self) -> bool { self.tab_directive }


    /**
     * Attempt to extract the next token from the input stream. Returns `Ok(Some(Token))` if a token
     * is successfully parsed, `Ok(None)` if the end of the input stream is reached, or
     * `Err(TokenizerError)` if an error is encountered during tokenization.
     */
    pub fn next_token(&mut self) -> Result<Option<Token>, TokenizerError>
    {
        if self.editor.is_some() { return self.next_editor_token(); }
        self.next_strict_token()
    }

    pub(super) fn next_strict_token(&mut self) -> Result<Option<Token>, TokenizerError>
    {
        let mut result = self.read_token();
        if let Some(index) = self.active_string && let Some(metadata) = &mut self.metadata
        { metadata.strings[index].span.end = self.input.byte_offset(); }
        if let Ok(Some(token)) = &mut result
        {
            if !matches!(token.kind, TokenKind::LineBreak) { self.header = false; }
            if    token.kind == TokenKind::Less
               && self.input.peek_next() == Some('-')
               && let Some(operand) = self.read_token()?
            {
                if    operand.kind == TokenKind::Minus
                   || matches!(&operand.value, TokenValue::Symbol(word)
                        if matches!(word.as_str(), "-~" | "-+~"))
                {
                    self.editor_start_at(token.location.clone(), token.span.start,
                        TokenKind::Invalid);
                    return Err(TokenizerError
                        {
                            location: token.location.clone(),
                            message: "Left-facing redirection is not supported; \
                                data flows left to right.".to_string(),
                        });
                }
                self.pending_operand = Some(operand);
            }
            if token.kind == TokenKind::SquareOpen && self.index_follows
            {
                token.kind = TokenKind::IndexOpen;
            }
            let enum_variant = self.enum_variant_follows && token.kind == TokenKind::Symbol;
            self.index_follows = self.pending_operator.is_none()
                && self.input.peek_next() == Some('[')
                && (enum_variant || matches!(token.kind,
                    TokenKind::Identifier | TokenKind::AutoIdentifier | TokenKind::Literal
                    | TokenKind::SquareClose | TokenKind::ParenClose | TokenKind::Member));
            self.member_follows = self.pending_operator.is_none()
                && self.input.peek_next() == Some('.')
                && (enum_variant || matches!(token.kind,
                    TokenKind::Identifier | TokenKind::AutoIdentifier | TokenKind::Literal
                    | TokenKind::SquareClose | TokenKind::ParenClose | TokenKind::Member));
            self.enum_variant_follows = token.kind == TokenKind::Scope;
        }
        if let Some(message) = self.input.read_error()
        {
            return Err(TokenizerError
                {
                    location: self.input.location().clone(),
                    message: message.to_string()
                });
        }
        result
    }

    fn read_token(&mut self) -> Result<Option<Token>, TokenizerError>
    {
        if let Some(operand) = self.pending_operand.take() { return Ok(Some(operand)); }
        if let Some(operator) = self.pending_operator.take()
        {
            return Ok(Some(operator));
        }

        if self.member_follows && self.input.peek_next() == Some('.')
        {
            self.editor_start(TokenKind::Member);
            self.member_follows = false;
            let dot = self.parse_dot();
            if dot.kind != TokenKind::Symbol || dot.token_value_text() != "."
            {
                return Ok(Some(dot));
            }
            let raw = self.input.peek_next() == Some(':');
            if raw
            {
                self.input.next();
                if self.input.next() != Some(':')
                { return Err(TokenizerError { location: dot.location,
                    message: "Backing access requires '.::field'".into() }); }
            }
            let dynamic = self.input.peek_next() == Some('$');
            if dynamic { self.input.next(); }
            let mut name = String::new();
            while    let Some(c) = self.input.peek_next()
                  && (c.is_alphanumeric() || c == '_')
            {
                name.push(c);
                self.input.next();
            }
            if name.is_empty() || name.chars().next().is_some_and(|c| c.is_numeric())
            {
                if !name.is_empty()
                { self.editor_start_at(dot.location.clone(), dot.span.start, TokenKind::Invalid); }
                return Err(TokenizerError { location: dot.location,
                    message: "Expected a field name after '.'.".to_string() });
            }
            if dynamic
            {
                name.insert(0, '$');
                self.qualified_variable_tail(&mut name)?;
            }
            if raw { name.insert_str(0, "::"); }
            return Ok(Some(Token
                {
                    location: dot.location,
                    span: SourceSpan::new(dot.span.start, self.input.byte_offset()),
                    kind: TokenKind::Member,
                    value: TokenValue::Symbol(name),
                }));
        }

        // Skip past any whitespace and comments.
        loop
        {
            self.skip_whitespace(SkipComments::Yes, SkipNewlines::No)?;
            if self.input.peek_next() != Some('\\') { break; }
            let location = self.input.location().clone();
            let start = self.input.byte_offset();
            self.editor_start(TokenKind::LineContinue);
            let layout = self.input.layout_position();
            self.input.next();
            if self.input.peek_next() == Some('-')
            {
                self.input.next();
                self.anchors.insert(start, layout);
                return Ok(Some(Token { location, span: SourceSpan::new(start,
                    self.input.byte_offset()), kind: TokenKind::Branch, value: TokenValue::None }));
            }
            if self.input.peek_next() == Some('\r') { self.input.next(); }
            if self.input.peek_next() != Some('\n')
            {
                return Err(TokenizerError
                    {
                        location,
                        message: "Expected a newline after line continuation.".to_string()
                    });
            }
            self.input.next();
            self.record_trivia(TriviaKind::LineContinuation, start);
        }

        // Check to see what the next character is, if any.
        let next = self.input.peek_next();

        if let None = next
        {
            return Ok(None);
        }

        // Unwrap the next character since we know it exists then proceed with matching it. Attempt
        // to return the extracted token to the caller, if any.
        let next = next.unwrap();
        self.editor_start(if next == '$' { TokenKind::Identifier } else { TokenKind::Invalid });

        match next
        {
            '_' | 'a'..='z' | 'A'..='Z' => Ok(Some(self.parse_symbol())),
            '$'                         => self.parse_identifier().map(Some),
            '.'                         =>
                {
                    let mut token = self.parse_dot();
                    if    token.kind == TokenKind::Splat
                       && self.input.peek_next().is_some_and(|next|
                            !Self::is_separator_char(&next)
                                && !matches!(next, ']' | '=' | '!' | '&'))
                    {
                        let word = "...".to_string() + &self.extract_to_separator(None);
                        token = Self::symbol_str_to_token(token.location, word,
                            self.word_span(token.span.start));
                    }
                    if    let TokenValue::Symbol(word) = &mut token.value
                       && word == "."
                    {
                        *word += &self.extract_to_separator(None);
                        token.span = self.word_span(token.span.start);
                    }
                    Ok(Some(token))
                },
            '0'..='9'                   => Ok(Some(self.try_parse_number())),
            '"'                         => self.parse_string(StringFlag::Interpolated),
            '\''                        => self.parse_string(StringFlag::NonInterpolated),
            _                           => self.try_parse_operator()
        }
    }

    /**
     * Extract a text symbol from the input stream. A symbol typically consists of alphabetic
     * characters, digits, and underscores, and is used to represent identifiers, keywords, or
     * other executables in the system where an executable can be an external program or function.
     */
    fn parse_symbol(&mut self) -> Token
    {
        let location = self.input.location().clone();
        let start = self.input.byte_offset();
        let symbol = self.extract_to_separator(
            if self.enum_variant_follows { Some(&['.', '[']) } else { None });

        Self::symbol_str_to_token(location, symbol, self.word_span(start))
    }

    /**
     * Internal method to convert the symbol string into a corresponding token. This is used to
     * differentiate between keywords and general symbols.
     */
    fn symbol_str_to_token(location: Location, symbol: String, span: SourceSpan) -> Token
    {
        let kind = match symbol.as_str()
            {
                "let"    => TokenKind::Let,
                "export" => TokenKind::Export,
                "pub"    => TokenKind::Public,
                "alias"  => TokenKind::Alias,
                "sub"    => TokenKind::Sub,
                "if"     => TokenKind::If,
                "else"   => TokenKind::Else,
                "while"  => TokenKind::While,
                "until"  => TokenKind::Until,
                "for"    => TokenKind::For,
                "in"     => TokenKind::In,
                "break"  => TokenKind::Break,
                "continue" => TokenKind::Continue,
                "loop"   => TokenKind::Loop,
                "match"  => TokenKind::Match,
                "return" => TokenKind::Return,
                "error" => TokenKind::Error,
                "try" => TokenKind::Try,
                "catch" => TokenKind::Catch,
                "finally" => TokenKind::Finally,
                "throw" => TokenKind::Throw,
                "fn"     => TokenKind::Function,
                "struct" => TokenKind::Struct,
                "enum"   => TokenKind::Enum,
                "type"   => TokenKind::Type,
                "import" => TokenKind::Import,
                "true"   => TokenKind::Literal,
                "false"  => TokenKind::Literal,
                _        => TokenKind::Symbol
            };

        let value = match &kind
            {
                TokenKind::Symbol  => TokenValue::Symbol(symbol),
                TokenKind::Literal => TokenValue::Literal(TokenLiteral::Boolean(symbol == "true")),
                _                  => TokenValue::None
            };

         Token { location, span, kind, value }
    }

    /**
     * Parse a variable identifier from the input stream. A variable identifier starts with a `$`
     * sign followed by a name, optionally enclosed in braces.
     */
    fn qualified_variable_tail(&mut self, identifier: &mut String) -> Result<(), TokenizerError>
    {
        while self.input.peek_next() == Some(':')
        {
            let location = self.input.location().clone();
            let start = self.input.byte_offset();
            self.input.next();
            if self.input.peek_next() != Some(':')
            {
                self.pending_operator = Some(Token
                    { location, span: self.word_span(start), kind: TokenKind::TypeDelimiter,
                      value: TokenValue::None });
                break;
            }
            self.input.next();
            let member = self.extract_to_separator(Some(&['{', '}', '.', '/', '[', ']']));
            if member.is_empty()
            {
                return Err(TokenizerError { location,
                    message: "Expected a variable name after '::'".into() });
            }
            identifier.push_str("::");
            identifier.push_str(&member);
        }
        Ok(())
    }

    fn parse_identifier(&mut self) -> Result<Token, TokenizerError>
    {
        let location = self.input.location().clone();
        let start = self.input.byte_offset();
        let mut identifier = "$".to_string();

        // Ensure that the next character is a '$' before proceeding with parsing the identifier.
        assert!(self.input.next() == Some('$'),
                "Expected '$' at the beginning of an identifier");

        let braced = self.input.peek_next() == Some('{');
        if braced
        {
            self.input.next();
            identifier += &self.extract_to_separator(Some(&['{', '}', '.', '/', '[', ']']));

            self.qualified_variable_tail(&mut identifier)?;
            if    identifier.len() == 1
               || self.pending_operator.is_some()
               || (self.editor.is_some() && self.input.peek_next() != Some('}'))
               || self.input.next() != Some('}')
            {
                return Err(TokenizerError
                    {
                        location,
                        message: "Expected a variable name and closing '}' in variable reference."
                            .to_string()
                    });
            }
        }
        else
        {
            identifier += &self.extract_to_separator(Some(&['.', '/', '[', ']']));
            self.qualified_variable_tail(&mut identifier)?;
        }

        if identifier.contains('=')
        {
            self.editor_start_at(location.clone(), start, TokenKind::Invalid);
            return Err(TokenizerError
                {
                    location,
                    message: "Invalid variable name: '=' is not allowed.".to_string()
                });
        }

        // A braced variable may start an interpolated word. Keep range and splat
        // operators separate, but include a single dot (e.g. ${name}.txt).
        let mut suffix = String::new();
        if braced && self.pending_operator.is_none() && self.input.peek_next() == Some('.')
        {
            let dot = self.parse_dot();
            if matches!(dot.kind, TokenKind::Range | TokenKind::RangeInclusive | TokenKind::Splat)
            {
                self.pending_operator = Some(dot);
            }
            else
            {
                suffix = dot.token_value_text();
            }
        }

        // Variable-prefixed paths and braced-variable suffixes are one word.
        // Indexing, assignment, quotes, and statement delimiters remain separate.
        if    self.pending_operator.is_none()
           && (!suffix.is_empty() || self.input.peek_next().is_some_and(|next|
                next == '/' || (braced && !Self::is_separator_char(&next)
                    && !matches!(next, '[' | ']' | '=' | '<' | '>' | '\'' | '"'))))
        {
            suffix += &self.extract_to_separator(None);
        }
        if !suffix.is_empty()
        {
            // Preserve the variable boundary when the resulting word is interpolated.
            if braced { identifier = format!("${{{}}}", &identifier[1..]); }
            identifier += &suffix;
            return Ok(Self::symbol_str_to_token(location, identifier, self.word_span(start)));
        }

        if identifier.len() == 1
        {
            return Ok(Token
                {
                    location,
                    span: self.word_span(start),
                    kind: TokenKind::AutoIdentifier,
                    value: TokenValue::None
                });
        }

        Ok(Token
            {
                location,
                span: self.word_span(start),
                kind: TokenKind::Identifier,
                value: TokenValue::Identifier(identifier)
            })
    }

    /**
     * Attempt to parse a literal number from the input stream. It will handle integers and
     * floating-point numbers, and other numeric formats as defined by the language's syntax.
     */
    fn try_parse_number(&mut self) -> Token
    {
        let location = self.input.location().clone();
        let start = self.input.byte_offset();
        let number_str = self.extract_word(Some(&['[']), true);

        // Attempt to parse the collected string as an integer first. If that fails, try parsing it
        // as a float. If both fail, treat it as a symbol.
        if let Ok(number) = number_str.parse::<i64>()
        {
            Token
                {
                    location,
                    span: self.word_span(start),
                    kind: TokenKind::Literal,
                    value: TokenValue::Literal(TokenLiteral::Integer(number, number_str))
                }
        }
        else if let Ok(number) = number_str.parse::<f64>()
        {
            Token
                {
                    location,
                    span: self.word_span(start),
                    kind: TokenKind::Literal,
                    value: TokenValue::Literal(TokenLiteral::Float(number, number_str))
                }
        }
        else
        {
            Self::symbol_str_to_token(location, number_str, self.word_span(start))
        }
    }

    /**
     * Extract characters from the input stream until a separator or an optional additional
     * separator is encountered.
     */
    fn extract_to_separator(&mut self, additional_separators: Option<&[char]>) -> String
    {
        self.extract_word(additional_separators, false)
    }

    fn extract_word(&mut self, additional_separators: Option<&[char]>, ranges: bool) -> String
    {
        let mut result = String::new();
        let mut in_glob_class = false;

        while let Some(next) = self.input.peek_next()
        {
            if ranges && next == '.'
            {
                let token = self.parse_dot();
                if matches!(
                    token.kind,
                    TokenKind::Range | TokenKind::RangeInclusive | TokenKind::Splat
                )
                {
                    self.pending_operator = Some(token);
                    break;
                }
                // Keep decimal points and scientific notation inside numbers,
                // but separate a numeric receiver from its first method name.
                if    result.parse::<f64>().is_ok()
                   && self.input.peek_next().is_some_and(|next|
                        next.is_alphabetic() || next == '_')
                {
                    let mut member = String::new();
                    while    let Some(next) = self.input.peek_next()
                          && (next.is_alphanumeric() || next == '_')
                    {
                        member.push(self.input.next().unwrap());
                    }
                    if    matches!(member.as_str(), "e" | "E")
                       && self.input.peek_next().is_some_and(|next| matches!(next, '+' | '-'))
                    {
                        member.push(self.input.next().unwrap());
                        while    let Some(next) = self.input.peek_next()
                              && next.is_ascii_digit()
                        {
                            member.push(self.input.next().unwrap());
                        }
                    }
                    let candidate = format!("{}.{}", result, member);
                    if    candidate.parse::<f64>().is_ok()
                       || self.input.peek_next() == Some('/')
                    {
                        result = candidate;
                        continue;
                    }
                    self.pending_operator = Some(Token
                        {
                            location: token.location,
                            span: SourceSpan::new(token.span.start, self.input.byte_offset()),
                            kind: TokenKind::Member,
                            value: TokenValue::Symbol(member),
                        });
                    break;
                }
                result += &token.token_value_text();
                continue;
            }
            // A closing array delimiter ends a word, except within a path glob class.
            if next == ']' && !in_glob_class { break; }
            // Braces delimit blocks, except inside an interpolated word's ${name} reference.
            if next == '{' && result.ends_with('$') && additional_separators.is_none()
            {
                result.push(self.input.next().unwrap());
                while let Some(character) = self.input.next()
                {
                    result.push(character);
                    if character == '}' { break; }
                }
                continue;
            }
            if    !in_glob_class && matches!(next, '<' | '>')
               && !(next == '>' && result.ends_with('-'))
            {
                let location = self.input.location().clone();
                let start = self.input.byte_offset();
                self.input.next();
                self.pending_operator = Some(self.ordering_token(location, next, start));
                break;
            }
            // Preserve single '=' and trailing '!' in shell words, and glob classes.
            // Paired operators delimit operands even without surrounding whitespace.
            if !in_glob_class && matches!(next, '=' | '!' | '&')
            {
                let location = self.input.location().clone();
                let start = self.input.byte_offset();
                self.input.next();
                let second = if next == '&' { '&' } else { '=' };
                if next == '=' && self.input.peek_next() == Some('>')
                {
                    self.input.next();
                    self.pending_operator = Some(Token
                        { location, span: self.word_span(start), kind: TokenKind::MatchArrow,
                          value: TokenValue::None });
                    break;
                }
                if self.input.peek_next() == Some(second)
                {
                    self.input.next();
                    let kind = match next
                        {
                            '=' => TokenKind::Equal,
                            '!' => TokenKind::NotEqual,
                            _ => TokenKind::And
                        };
                    self.pending_operator = Some(Token
                        { location, span: self.word_span(start), kind, value: TokenValue::None });
                    break;
                }
                result.push(next);
                continue;
            }
            if    !Self::is_separator_char(&next)
               && !additional_separators.map_or(false, |separators| separators.contains(&next))
            {
                let _ = &self.input.next();
                result.push(next);
                if next == '[' { in_glob_class = true; }
                if next == ']' { in_glob_class = false; }
            }
            else
            {
                break;
            }
        }

        result
    }

    fn ordering_token(&mut self, location: Location, first: char, start: usize) -> Token
    {
        let equal = self.input.peek_next() == Some('=');
        if equal { self.input.next(); }
        let kind = match (first, equal)
            {
                ('<', false) => TokenKind::Less,
                ('<', true) => TokenKind::LessEqual,
                ('>', false) => TokenKind::Greater,
                _ => TokenKind::GreaterEqual,
            };
        Token { location, span: self.word_span(start), kind, value: TokenValue::None }
    }

    fn parse_dot(&mut self) -> Token
    {
        let location = self.input.location().clone();
        let start = self.input.byte_offset();
        self.input.next();
        if self.input.peek_next() != Some('.')
        {
            // A single dot remains part of a float or path. Leave following
            // characters for the numeric extractor when it is reading a float.
            return Self::symbol_str_to_token(location, ".".to_string(), self.word_span(start));
        }
        self.input.next();
        if self.input.peek_next() == Some('/')
        {
            let word = "..".to_string() + &self.extract_to_separator(None);
            return Self::symbol_str_to_token(location, word, self.word_span(start));
        }
        let kind = match self.input.peek_next()
            {
                Some('.') => { self.input.next(); TokenKind::Splat },
                Some('=') => { self.input.next(); TokenKind::RangeInclusive },
                _ => TokenKind::Range
            };
        Token { location, span: self.word_span(start), kind, value: TokenValue::None }
    }

    /**
     * Parse a string literal from the input stream. It will attempt to extract the string as a
     * single line string. If it is a multi-line string, it will call out for multi-line handling.
     *
     * In single line strings `\n`, new lines are illegal and will result in a parsing error. An
     * error is also generated if the closing quote is never encountered in the source text.
     */
    fn parse_string(&mut self, flag: StringFlag) -> Result<Option<Token>, TokenizerError>
    {
        let location = self.input.location().clone();
        let start = self.input.byte_offset();
        let next = self.input.next().unwrap();
        self.begin_string(start, &flag);

        assert!(matches!(next, '"' | '\''),
                "Internal error, expected a quote to start a string literal.");

        if    let Some(next) = self.input.peek_next()
           && next == '*'
        {
            return self.parse_multiline_string(location, flag, start);
        }
        else
        {
            let mut literal_string = String::new();
            let mut escaped_dollars = Vec::new();
            let mut closed = false;

            loop
            {
                let before = self.editor.as_ref().map(|_| self.input.location().clone());
                let Some(next) = self.input.next() else { break; };
                let char_start = self.input.byte_offset() - next.len_utf8();
                match next
                {
                    '"' if flag == StringFlag::Interpolated =>
                        {
                            self.close_string(char_start);
                            closed = true;
                            break;
                        },

                    '\'' if flag == StringFlag::NonInterpolated =>
                        {
                            self.close_string(char_start);
                            closed = true;
                            break;
                        },

                    '\n' =>
                        {
                            if self.editor.is_some()
                            {
                                self.editor_issue(SourceSpan::new(start, char_start),
                                    DiagnosticKind::Invalid,
                                    "Newline encountered in single-line string literal.");
                                self.end_partial_string(char_start);
                                self.pending_operator = Some(Token
                                    {
                                        location: before.unwrap(),
                                        span: SourceSpan::new(char_start, self.input.byte_offset()),
                                        kind: TokenKind::LineBreak,
                                        value: TokenValue::None,
                                    });
                                closed = true;
                                break;
                            }
                            return Err(TokenizerError
                                {
                                    location,
                                    message: "Newline encountered in single-line string literal."
                                        .to_string()
                                });
                        },

                    '\\' =>
                        {
                            let escaped = self.process_editor_escape(char_start)?;
                            if escaped == '$' { escaped_dollars.push(literal_string.len()); }
                            let source = SourceSpan::new(char_start, self.input.byte_offset());
                            self.push_string(&mut literal_string, escaped, source,
                                StringPartKind::Escape);
                        },

                    _ =>
                        {
                            let source = SourceSpan::new(char_start, self.input.byte_offset());
                            self.push_string(&mut literal_string, next, source,
                                StringPartKind::Text);
                        }
                }
            }

            if !closed && self.editor.is_some()
            {
                self.editor_issue(SourceSpan::new(start, self.input.byte_offset()),
                    DiagnosticKind::Incomplete, "Unterminated string literal.");
                self.end_partial_string(self.input.byte_offset());
                closed = true;
            }
            if !closed
            {
                return Err(TokenizerError
                    {
                        location,
                        message: "Missing ending quote for string literal.".to_string()
                    })
            }

            Ok(Some(Token
                {
                    location,
                    span: self.word_span(start),
                    kind: TokenKind::Literal,
                    value: TokenValue::Literal(TokenLiteral::String(
                        literal_string,
                        flag,
                        escaped_dollars,
                    )),
                }))
        }
    }

    /**
     * Called from parse string when a multi-line string is detected. The string parsed will have
     * leading whitespace removed while preserving the relative indentation of the subsequent lines.
     */
    fn parse_multiline_string(&mut self,
                              location: Location,
                              flag: StringFlag, start: usize)
                              -> Result<Option<Token>, TokenizerError>
    {
        // Helper for skipping extra whitespace at the beginning of each line.  If there is no text
        // on a given line it is skipped entirely.
        fn skip_whitespace_until_column<'a>(location: &Location, buffer: &mut dyn Buffer,
                                            target_column: usize, capture: bool, editor: bool)
                                            -> Result<Vec<SourceSpan>, TokenizerError>
        {
            let mut newlines = Vec::new();
            while    let Some(next) = buffer.peek_next()
                  && Tokenizer::<'a>::is_whitespace_char(&next)
                  && buffer.location().column < target_column
            {
                let start = buffer.byte_offset();
                let _ = buffer.next();
                if capture && next == '\n'
                { newlines.push(SourceSpan::new(start, buffer.byte_offset())); }
            }

            if buffer.peek_next().is_none() && !editor
            {
                return Err(TokenizerError
                    {
                        location: location.clone(),
                        message: "Unexpected end of file in string literal.".to_string()
                    });
            }

            Ok(newlines)
        }

        // Append newlines for skipped empty lines.
        fn append_newlines(text: &mut String, count: usize)
        {
            for _ in 0..count
            {
                text.push('\n');
            }
        }

        // We expect that the " has already be processed and that we need to consume the following
        // *.
        let next = self.input.next().unwrap();
        assert!(next == '*');
        if let Some(index) = self.active_string && let Some(metadata) = &mut self.metadata
        { metadata.strings[index].opening.end = self.input.byte_offset(); }

        // Skip over any whitespace at the beginning of the string.  Using the location of the first
        // textual character to calibrate what we will consider the beginning of the actual line of
        // text.  This way we can remove any extra whitespace at the beginning of each line while
        // allowing for any extra indentation the user may want to add.
        self.skip_whitespace(SkipComments::No, SkipNewlines::Yes)?;

        let target_column = self.input.location().column;

        let mut closed = false;
        let mut text = String::new();
        let mut escaped_dollars = Vec::new();

        // Keep going until we either hit the end of the buffer or the closing *" pair.
        while let Some(next) = self.input.next()
        {
            let char_start = self.input.byte_offset() - next.len_utf8();
            match next
            {
                // We found the * but did we find the "?
                '*' =>
                {
                    if let Some(quote) = self.input.peek_next()
                    {
                        // We're at the end of the string.
                        if    (quote == '"' && flag == StringFlag::Interpolated)
                           || (quote == '\'' && flag == StringFlag::NonInterpolated)
                        {
                            let _ = self.input.next();
                            self.close_string(char_start);
                            closed = true;
                            break;
                        }
                        else
                        {
                            // Looks like a stray * so we'll just add it to the text.
                            let source = SourceSpan::new(char_start, self.input.byte_offset());
                            self.push_string(&mut text, '*', source, StringPartKind::Text);
                        }
                    }
                    else
                    {
                        if self.editor.is_some()
                        {
                            let source = SourceSpan::new(char_start, self.input.byte_offset());
                            self.push_string(&mut text, '*', source, StringPartKind::Text);
                            break;
                        }
                        return Err(TokenizerError
                            {
                                location: location.clone(),
                                message: "Unexpected end of file in string literal.".to_string()
                            });
                    }
                }

                // Process the escape sequence.
                '\\' =>
                    {
                        let escaped = self.process_editor_escape(char_start)?;
                        if escaped == '$' { escaped_dollars.push(text.len()); }
                        let source = SourceSpan::new(char_start, self.input.byte_offset());
                        self.push_string(&mut text, escaped, source, StringPartKind::Escape);
                    },

                // Process the new line skipping any extra whitespace until we hit the target
                // column.
                '\n' =>
                {
                    let source = SourceSpan::new(char_start, self.input.byte_offset());
                    self.push_string(&mut text, '\n', source, StringPartKind::Text);

                    // Keep track of the starting line so that we can add newlines for skipped empty
                    // lines.  Then start skipping until we find something useful or we hit the
                    // target column.
                    let start_line = self.input.location().line;

                    let newlines = skip_whitespace_until_column(&location, self.input,
                        target_column, self.metadata.is_some(), self.editor.is_some())?;

                    // If we skipped any empty lines then we need to backfill the newlines.
                    let current_line = self.input.location().line;

                    if current_line > start_line
                    {
                        if self.metadata.is_some()
                        {
                            for source in newlines
                            { self.push_string(&mut text, '\n', source, StringPartKind::Text); }
                        }
                        else { append_newlines(&mut text, current_line - start_line); }
                    }
                }

                // Just add the character to the text.
                _ =>
                {
                    let source = SourceSpan::new(char_start, self.input.byte_offset());
                    self.push_string(&mut text, next, source, StringPartKind::Text);
                }
            }
        }

        if !closed && self.editor.is_some()
        {
            self.editor_issue(SourceSpan::new(start, self.input.byte_offset()),
                DiagnosticKind::Incomplete, "Unterminated string literal.");
            self.end_partial_string(self.input.byte_offset());
            closed = true;
        }
        if !closed
        {
            return Err(TokenizerError
                {
                    location: location.clone(),
                    message: "Unterminated string literal.".to_string()
                });
        }

        Ok(Some(Token
            {
                location,
                span: self.word_span(start),
                kind: TokenKind::Literal,
                value: TokenValue::Literal(TokenLiteral::String(text, flag, escaped_dollars))
            }))
    }

    /**
     * Keep the escape decoder strict. Editor recovery records the damaged source
     * and substitutes a character only in the decoded analysis value.
     */
    fn process_editor_escape(&mut self, start: usize) -> Result<char, TokenizerError>
    {
        match self.process_string_escape()
        {
            Err(error) if self.editor.is_some() =>
                {
                    let end = self.input.byte_offset();
                    let kind = if end <= start + 2 && self.input.peek_next().is_none()
                        { DiagnosticKind::Incomplete } else { DiagnosticKind::Invalid };
                    self.editor_issue(SourceSpan::new(start, end), kind, error.message);
                    Ok('\u{fffd}')
                },
            result => result,
        }
    }

    fn end_partial_string(&mut self, end: usize)
    {
        if let Some(index) = self.active_string.take() && let Some(metadata) = &mut self.metadata
        { metadata.strings[index].span.end = end; }
    }

    /**
     * Processes an escape sequence within a string literal and returns the corresponding character.
     *
     * The sequence can be a single known character like \n or \\ or a numeric escape sequence like
     * \xNN or \o{NNNN}.
     */
    fn process_string_escape(&mut self) -> Result<char, TokenizerError>
    {
        // Attempt to parse the numeric value from the escape sequence. If not parsed correctly, an
        // error will be returned.
        fn parse_number(location: &Location, input: &mut dyn Buffer,
                        is_digit: fn(char) -> bool, radix: u32)
                        -> Result<char, TokenizerError>
        {
            let number_str = read_digits(input, is_digit);
            let number = u32::from_str_radix(&number_str, radix)
                .map_err(|_| TokenizerError
                    {
                        location: location.clone(),
                        message: format!("Failed to parse numeric literal from '{}'.",
                                         number_str)
                    })?;

            char::from_u32(number)
                .ok_or_else(|| TokenizerError
                    {
                        location: location.clone(),
                        message: format!("Numeric literal '{}' is not a valid character.",
                                         number_str)
                    })
        }

        // Read a sequence of digits from the input buffer that satisfy the `is_digit` predicate
        // breaking when a non-digit character is encountered.
        fn read_digits(input: &mut dyn Buffer, is_digit: fn(char) -> bool) -> String
        {
            let mut number_str = String::new();

            while    let Some(next) = input.peek_next()
                  && is_digit(next)
            {
                number_str.push(input.next().unwrap());
            }

            number_str
        }

        let location = self.input.location().clone();

        match self.input.next()
        {
            // Perform a simple translation of the escape sequence.
            Some('n') => Ok('\n'),
            Some('r') => Ok('\r'),
            Some('t') => Ok('\t'),

            // Parse a decimal numeric literal for the character.
            Some('0') =>
                {
                    parse_number(&location, self.input, |next| next.is_ascii_digit(), 10)
                }

            // Parse a hexadecimal numeric literal for the character.
            Some('x') | Some('X') =>
                {
                    parse_number(&location, self.input, |next| next.is_ascii_hexdigit(), 16)
                }

            // Parse an octal numeric literal for the character.
            Some('o') | Some('O') =>
                {
                    parse_number(&location, self.input, |next| ('0'..='7').contains(&next), 8)
                }

            // The escape was on a non-special character so just pass it through without
            // translation.
            Some(next) => Ok(next),

            // Looks like we hit the end of the buffer while processing a string.
            None =>
                {
                    Err(TokenizerError
                        {
                            location,
                            message: "Unexpected end of file in string literal.".to_string()
                        })
                }
        }
    }

    /**
     * Attempt to extract a single or multiple character operator from the input stream. Returns
     * `Ok(Some(Token))` if an operator is successfully parsed, `Ok(None)` if no operator is found,
     * or `Err(TokenizerError)` if an error is encountered during parsing.
     */
    fn try_parse_operator(&mut self) -> Result<Option<Token>, TokenizerError>
    {
        fn operator_token(location: Location, span: SourceSpan, kind: TokenKind)
            -> Result<Option<Token>, TokenizerError>
        {
            Ok(Some(Token { location, span, kind, value: TokenValue::None }))
        }

        // First, check to see if the next character is a separator operator that can be immediately
        // returned. If it isn't a separator operator, we will need to parse it as a multi-character
        // operator. This disambiguates "-" from "-f" or "--foo" where the latter two can be treated
        // as text to be passed through.
        let location = self.input.location().clone();
        let start = self.input.byte_offset();
        if self.input.peek_next() == Some('|')
        { self.anchors.insert(start, self.input.layout_position()); }
        let next = self.input.next().unwrap();

        match next
        {
            '<' | '>' => return Ok(Some(self.ordering_token(location, next, start))),
            '-' if self.input.peek_next() == Some('>') =>
                {
                    self.input.next();
                    return operator_token(location, self.word_span(start), TokenKind::RedirectTo);
                },
            '|' =>
                {
                    if self.input.peek_next() == Some('|')
                    {
                        self.input.next();
                        return operator_token(location, self.word_span(start), TokenKind::Or);
                    }
                    return operator_token(location, self.word_span(start), TokenKind::Pipe);
                },
            '!' =>
                {
                    if self.input.peek_next() == Some('=')
                    {
                        self.input.next();
                        return operator_token(location, self.word_span(start), TokenKind::NotEqual);
                    }
                    return operator_token(location, self.word_span(start), TokenKind::Not);
                },
            '=' if self.input.peek_next() == Some('>') =>
                {
                    self.input.next();
                    return operator_token(location, self.word_span(start), TokenKind::MatchArrow);
                },
            '=' | '&' if self.input.peek_next() == Some(next) =>
                {
                    self.input.next();
                    return operator_token(location, self.word_span(start),
                        if next == '=' { TokenKind::Equal } else { TokenKind::And });
                },
            ';'  => return operator_token(location, self.word_span(start),
                TokenKind::StatementBreak),
            '('  => return operator_token(location, self.word_span(start), TokenKind::ParenOpen),
            ')'  => return operator_token(location, self.word_span(start), TokenKind::ParenClose),
            '{'  => return operator_token(location, self.word_span(start), TokenKind::BlockOpen),
            '}'  => return operator_token(location, self.word_span(start), TokenKind::BlockClose),
            '['  => return operator_token(location, self.word_span(start), TokenKind::SquareOpen),
            ']'  => return operator_token(location, self.word_span(start), TokenKind::SquareClose),
            '`'  => return operator_token(location, self.word_span(start), TokenKind::ExecEscape),
            '\n' => return operator_token(location, self.word_span(start), TokenKind::LineBreak),
            '\\' => return operator_token(location, self.word_span(start), TokenKind::LineContinue),
            ','  => return operator_token(location, self.word_span(start), TokenKind::Comma),
            '-' if self.input.peek_next() == Some('[') =>
                return operator_token(location, self.word_span(start), TokenKind::Minus),
            ':'  =>
                {
                    if let Some(':') = self.input.peek_next()
                    {
                        let _ = self.input.next();
                        return operator_token(location, self.word_span(start), TokenKind::Scope);
                    }

                    return operator_token(location, self.word_span(start),
                        TokenKind::TypeDelimiter);
                }

            // Nothing to do here, pass through to non-separator operator parsing.
            _ => {}
        }

        // Ok we know it isn't a separator operator, so we will attempt to parse it as a
        // non-separator operator, some of which can be multi-character.
        let mut operator_str = String::new();

        operator_str.push(next);
        if next == '-' && self.input.peek_next().is_some_and(|character| character.is_ascii_digit())
        {
            operator_str += &self.extract_word(Some(&['[']), true);
        }
        else
        {
            operator_str += &self.extract_to_separator(None);
        }

        // Negative numbers are values; options such as -f and --flag remain words.
        if operator_str.starts_with('-') && operator_str.len() > 1
        {
            let number = operator_str.parse::<i64>().ok()
                .map(|value| TokenLiteral::Integer(value, operator_str.clone()))
                .or_else(|| operator_str.parse::<f64>().ok()
                    .map(|value| TokenLiteral::Float(value, operator_str.clone())));
            if let Some(number) = number
            {
                return Ok(Some(Token
                    {
                        location,
                        span: self.word_span(start),
                        kind: TokenKind::Literal,
                        value: TokenValue::Literal(number)
                    }));
            }
        }

        match operator_str.as_str()
        {
            "="    => return operator_token(location, self.word_span(start), TokenKind::Assign),
            "{"    => return operator_token(location, self.word_span(start), TokenKind::BlockOpen),
            "}"    => return operator_token(location, self.word_span(start), TokenKind::BlockClose),
            "-"    => return operator_token(location, self.word_span(start), TokenKind::Minus),
            "~"    => return operator_token(location, self.word_span(start),
                TokenKind::ErrorSource),
            "+"    => return operator_token(location, self.word_span(start), TokenKind::Plus),
            "*"    => return operator_token(location, self.word_span(start), TokenKind::Asterisk),
            "/"    => return operator_token(location, self.word_span(start), TokenKind::Slash),
            "%"    => return operator_token(location, self.word_span(start), TokenKind::Percent),
            "->"   => return operator_token(location, self.word_span(start), TokenKind::RedirectTo),
            "~->"  => return operator_token(location, self.word_span(start),
                TokenKind::RedirectErrorTo),
            "~+->" => return operator_token(location, self.word_span(start),
                TokenKind::RedirectBothTo),
            "<-" | "<-~" | "<-+~" => return Err(TokenizerError
                {
                    location,
                    message: "Left-facing redirection is not supported; data flows left to right."
                        .to_string(),
                }),
            "{}"   => return operator_token(location, self.word_span(start), TokenKind::EmptyBlock),
            "**"   => return operator_token(location, self.word_span(start), TokenKind::Glob),
            "..."  => return operator_token(location, self.word_span(start), TokenKind::Splat),
            _      => Ok(Some(Self::symbol_str_to_token(location, operator_str,
                self.word_span(start))))
        }
    }

    /**
     * Skip all text that is considered whitespace, including comments, until a non-whitespace
     * character is encountered. In this syntax new lines are not specifically considered
     * whitespace. Because newlines are significant in terminating certain statements like
     * executable object calls.
     */
    fn skip_whitespace(&mut self, skip_comments: SkipComments, skip_newlines: SkipNewlines)
        -> Result<(), TokenizerError>
    {
        // Peek at the next character and skip it if it's whitespace or part of a comment.
        while let Some(next) = self.input.peek_next()
        {
            // Check to see if the next character is not whitespace. If it isn't, break out of the
            // loop, without consuming the non-whitespace character.
            if    !Self::is_whitespace_char(&next)
               && !(next == '#' && skip_comments == SkipComments::Yes)
               && !(next == '\n' && skip_newlines == SkipNewlines::Yes)
            {
                break;
            }

            // It's a whitespace character or part of a comment, so consume it.
            let start = self.input.byte_offset();
            let location = self.input.location().clone();
            let standalone = next == '#' && self.input.indentation();
            let _ = self.input.next();

            // If the next character is a comment indicator and we are supposed to skip comments,
            // skip the entire comment up until the next newline character. We don't consume the
            // newline itself so that it can be handled separately as a significant token.
            if next == '#' && skip_comments == SkipComments::Yes
            {
                // Consume any characters in the comment until a newline is encountered.
                let mut comment = String::new();
                while let Some(next) = self.input.peek_next()
                {
                    if next == '\n' { break; }
                    comment.push(self.input.next().unwrap());
                }
                if comment == "tab-size" || comment.starts_with("tab-size ")
                    || comment.starts_with("tab-size\t")
                {
                    let invalid = |message: &str| TokenizerError
                        { location: location.clone(), message: message.into() };
                    if !standalone || !self.header
                    { return Err(invalid("#tab-size must occur in the header before code")); }
                    if self.tab_directive
                    { return Err(invalid("Duplicate #tab-size directive")); }
                    let value = comment[8..].trim();
                    let width = value.parse::<usize>().ok().filter(|width|
                        (1..=32).contains(width) && value.bytes().all(|c| c.is_ascii_digit()))
                        .ok_or_else(|| invalid("#tab-size requires an integer from 1 through 32"))?;
                    self.input.set_tab_width(width);
                    self.tab_directive = true;
                }
                self.record_trivia(TriviaKind::Comment, start);
            }
            else { self.record_trivia(TriviaKind::Whitespace, start); }
        }
        Ok(())
    }

    /**
     * Is the given character a separator character? Separator characters are those that are used as
     * operators or punctuation in the language's syntax. They also act as separator characters
     * between tokens. So `$var|$var` appear as separate tokens in the stream despite the lack of
     * whitespace.
     */
    fn is_separator_char(next: &char) -> bool
    {
           *next == '|'
        || *next == ';'
        || *next == '('
        || *next == ')'
        || *next == '{'
        || *next == '}'
        || *next == ':'
        || *next == '#'
        || *next == '`'
        || *next == ','
        || *next == '\n'
        || *next == '\\'
        || Self::is_whitespace_char(next)
    }

    /**
     * Determines if the given character is considered whitespace, excluding newline characters.
     * This is because newlines are considered significant in the language's syntax.
     */
    fn is_whitespace_char(next: &char) -> bool
    {
        *next != '\n' && next.is_whitespace()
    }
}

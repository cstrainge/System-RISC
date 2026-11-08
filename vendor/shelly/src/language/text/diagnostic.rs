
use crate::language::text::span::SourceSpan;


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticKind
{
    Incomplete,
    Invalid,
    Limit,
}


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticPhase
{
    Tokenizer,
    Parser,
}


#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceDiagnostic
{
    pub span: SourceSpan,
    pub kind: DiagnosticKind,
    pub phase: DiagnosticPhase,
    pub message: String,
}

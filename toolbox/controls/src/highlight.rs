//! Windowed highlighter interface. The stubs store the opt-in but never invoke
//! a module or pretend a registry/module loader exists for this interface yet.
#![forbid(unsafe_code)]
use alloc::{string::String, vec::Vec};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HighlightError
{
    BadQuery,
    BadSpan,
    NoMemory,
    ModuleFailed
}
pub struct SpanQuery<'a>
{
    pub start:          u64,
    pub end:            u64,
    pub text:           &'a str,
    pub lookback_start: u64,
    pub lookback:       &'a str
}
pub struct Span
{
    pub start: u64,
    pub end:   u64,
    pub kind:  String
}
pub trait Highlighter
{
    fn lookback_lines(&self) -> usize
    {
        0
    }
    fn spans(&self, query: &SpanQuery<'_>) -> Result<Vec<Span>, HighlightError>;
}
impl SpanQuery<'_>
{
    pub fn validate(&self) -> Result<(), HighlightError>
    {
        if self.start.checked_add(self.text.len() as u64) != Some(self.end)
           || self.lookback_start.checked_add(self.lookback.len() as u64) != Some(self.start)
        {
            return Err(HighlightError::BadQuery);
        }
        Ok(())
    }

    /// A bad span rejects this window's entire answer, allowing plain rendering.
    pub fn validate_spans(&self, spans: &[Span]) -> Result<(), HighlightError>
    {
        self.validate()?;
        let mut previous = self.start;
        for span in spans
        {
            if span.start < previous
               || span.start > span.end
               || span.end > self.end
               || span.kind.is_empty()
               || !span.kind.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
            {
                return Err(HighlightError::BadSpan);
            }
            let start =
                usize::try_from(span.start - self.start).map_err(|_| HighlightError::BadSpan)?;
            let end = usize::try_from(span.end - self.start).map_err(|_| HighlightError::BadSpan)?;
            if !self.text.is_char_boundary(start) || !self.text.is_char_boundary(end)
            {
                return Err(HighlightError::BadSpan);
            }
            previous = span.end;
        }
        Ok(())
    }
}

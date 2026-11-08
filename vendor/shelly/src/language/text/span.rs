
/// Half-open UTF-8 byte offsets into the original source, independent of display columns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceSpan
{
    pub start: usize,
    pub end: usize,
}

impl SourceSpan
{
    pub fn new(start: usize, end: usize) -> Self { Self { start, end } }

    /// Reject reversed, out-of-bounds, or non-character-boundary slices.
    pub fn slice(self, source: &str) -> Option<&str> { source.get(self.start..self.end) }

    pub fn cover(self, other: Self) -> Self
    { Self::new(self.start.min(other.start), self.end.max(other.end)) }
}

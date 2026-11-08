
use std::sync::{ Arc, Mutex };

use reedline::{ Highlighter, StyledText };

use crate::{ language::analysis::{ analyze_semantics, SymbolSnapshot,
                                  highlight::{ highlight_spans, HighlightSpan } },
             runtime::color::TtyColorMode };

mod palette;
pub use crate::runtime::highlighting::palette::{ SyntaxSettings, SYNTAX_COLORS };


#[derive(Default)]
struct HighlightCache
{
    source: String,
    spans: Vec<HighlightSpan>,
}


pub struct ShellyHighlighter
{
    snapshot: Arc<SymbolSnapshot>,
    settings: SyntaxSettings,
    cache: Mutex<HighlightCache>,
}


impl ShellyHighlighter
{
    pub fn new(snapshot: Arc<SymbolSnapshot>, settings: SyntaxSettings) -> Self
    {
        Self
            {
                snapshot, settings, cache: Mutex::new(HighlightCache::default()),
            }
    }

    fn styled(&self, source: &str, spans: &[HighlightSpan]) -> StyledText
    {
        let mut styled = StyledText::new();
        for span in spans
        {
            let color = self.settings.colors[span.kind as usize];
            let style = color.map_or_else(Default::default, |color| color.normal());
            if let Some(text) = span.span.slice(source) { styled.push((style, text.to_string())); }
        }
        styled
    }
}


impl Highlighter for ShellyHighlighter
{
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText
    {
        if !self.settings.enabled || self.settings.mode == TtyColorMode::TtyMonochrome
        {
            let mut plain = StyledText::new();
            plain.push((Default::default(), line.to_string()));
            return plain;
        }
        let mut cache = self.cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if cache.source != line
        {
            let analysis = analyze_semantics(line, &self.snapshot);
            cache.spans = highlight_spans(line, &analysis);
            cache.source = line.to_string();
        }
        // Cursor movement and completion redraws reuse the same classifications.
        self.styled(line, &cache.spans)
    }
}


use unicode_width::UnicodeWidthStr;


#[derive(Clone, Copy, Debug)]
pub struct LayoutPosition
{
    pub column: usize,
    pub tab_width: usize,
    pub hard_tabs: bool,
}


// Original bytes stay intact. Only layout anchors need display-width coordinates.
#[derive(Default)]
pub struct LineLayout
{
    prefix: String,
}


impl LineLayout
{
    pub fn advance(&mut self, next: char)
    {
        if next == '\n' { self.prefix.clear(); }
        else { self.prefix.push(next); }
    }

    pub fn position(&self, tab_width: usize) -> LayoutPosition
    {
        let mut column = 1;
        let mut parts = self.prefix.split('\t');
        column += parts.next().unwrap_or_default().width();
        for part in parts
        {
            column += tab_width - ((column - 1) % tab_width);
            column += part.width();
        }
        LayoutPosition { column, tab_width, hard_tabs: self.prefix.contains('\t') }
    }

    pub fn indentation(&self) -> bool
    { self.prefix.chars().all(|c| matches!(c, ' ' | '\t' | '\r')) }
}

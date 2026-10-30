//! Divider tracking is resident. The parent applies its final coordinate.
use mac_abi::controls as wire;

use super::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitterOrientation
{
    Vertical,
    Horizontal
}
#[derive(Clone, Copy, Debug)]
pub struct SplitterSpec
{
    pub bounds:        Rect,
    pub orientation:   SplitterOrientation,
    /// Inclusive coordinates along the moving axis, in window-local pixels.
    pub range:         Option<(i16, i16)>,
    /// Optional parent rectangle in window-local coordinates. The drag outline
    /// spans its height for a vertical divider, or its width for a horizontal
    /// divider. This does not constrain movement; range does that separately.
    pub parent_bounds: Option<Rect>,
    pub enabled:       bool,
    pub reflow:        Reflow
}
impl SplitterSpec
{
    fn state(self) -> Result<StandardState>
    {
        if self.range.is_some_and(|(min, max)| min > max)
        {
            return Err(Error::BadArgument);
        }
        let horizontal = self.orientation == SplitterOrientation::Horizontal;
        let (minimum, maximum) = self.range.unwrap_or((0, 0));
        Ok(StandardState { bounds: self.bounds,
                           enabled: u32::from(self.enabled),
                           value: if horizontal { self.bounds.top } else { self.bounds.left },
                           minimum,
                           maximum,
                           flags: if horizontal { wire::SPLITTER_HORIZONTAL } else { 0 }
                                  | if self.range.is_some() { wire::SPLITTER_RANGE } else { 0 }
                                  | if self.parent_bounds.is_some()
                                  {
                                      wire::SPLITTER_PARENT
                                  }
                                  else
                                  {
                                      0
                                  },
                           splitter_parent: self.parent_bounds.unwrap_or(Rect::ZERO),
                           ..Default::default() })
    }
}
pub struct Splitter<'a>
{
    control: Control<'a>
}
#[derive(Clone, Copy)]
pub struct SplitterRef<'a>
{
    control: ControlRef<'a>
}
impl<'a> Controls<'a>
{
    pub fn create_splitter(&self, window: WindowId, spec: SplitterSpec) -> Result<Splitter<'a>>
    {
        Ok(Splitter { control: self.create_standard(window,
                                                    StandardKind::Splitter,
                                                    spec.state()?,
                                                    spec.reflow)? })
    }
}
impl<'a> Splitter<'a>
{
    pub fn control(&self) -> ControlRef<'a>
    {
        self.control.control()
    }

    pub fn splitter(&self) -> SplitterRef<'a>
    {
        self.control().splitter()
    }

    pub fn into_control(self) -> Control<'a>
    {
        self.control
    }
}
impl<'a> ControlRef<'a>
{
    pub fn splitter(self) -> SplitterRef<'a>
    {
        SplitterRef { control: self }
    }
}
impl<'a> SplitterRef<'a>
{
    pub fn control(self) -> ControlRef<'a>
    {
        self.control
    }

    /// Replace the complete divider state. Reflow remains its creation choice.
    pub fn configure(self, spec: SplitterSpec) -> Result<()>
    {
        self.control.configure(StandardKind::Splitter, spec.state()?).map(|_| ())
    }

    /// Calls the parent only after release. During tracking, System draws the
    /// outline and preserves pane geometry. Callback errors still cancel capture.
    pub fn track(self, anchor: Point, mut released: impl FnMut(i16) -> Result<()>) -> Result<bool>
    {
        self.control.track(anchor, |info| released(info.standard.value))
    }
}

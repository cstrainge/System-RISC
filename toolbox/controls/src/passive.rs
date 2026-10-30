//! Passive controls retain their state and cannot capture input. Their CDEFs
//! own all pixels, text placement and progress-change geometry.
use super::{Control, Controls, FontSelection, Rect, Result, StandardKind, StandardState, WindowId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress
{
    /// No denominator has been established. An empty bar promises no percentage.
    Preparing,
    /// Both forks contribute to these checked, full-width counters.
    Determinate
    {
        completed: u64, total: u64
    },
    /// Counts current work separately; only completed work fills the meter.
    Working
    {
        completed: u64, in_progress: u64, total: u64
    }
}
impl Progress
{
    fn state(self, bounds: Rect) -> Result<StandardState>
    {
        let (completed, in_progress, total, determinate) = match self
        {
            Self::Preparing => (0, 0, 0, 0),
            Self::Determinate { completed,
                                total } => (completed, 0, total, 1),
            Self::Working { completed,
                            in_progress,
                            total } => (completed, in_progress, total, 1)
        };
        if completed > total || in_progress > total - completed
        {
            return Err(super::Error::BadArgument);
        }
        Ok(StandardState { bounds,
                           progress: mac_abi::controls::ProgressRange { completed,
                                                                        in_progress,
                                                                        total,
                                                                        determinate,
                                                                        reserved: 0 },
                           ..Default::default() })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressStyle
{
    Color,
    Monochrome
}
pub struct ProgressBar<'a>
{
    control: Control<'a>,
    state:   StandardState,
    kind:    StandardKind
}
impl ProgressBar<'_>
{
    pub fn id(&self) -> super::ControlId
    {
        self.control.id()
    }

    /// Identical values do not cross the trap boundary. Subpixel changes are
    /// coalesced by the CDEF and never trigger a compositor update.
    pub fn set(&mut self, progress: Progress) -> Result<()>
    {
        let next = progress.state(self.state.bounds)?;
        if self.state == next
        {
            return Ok(());
        }
        self.control.control().configure(self.kind, next)?;
        self.state = next;
        Ok(())
    }

    pub fn draw(&self) -> Result<()>
    {
        self.control.control().draw()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i16)]
pub enum TextAlignment
{
    Left   = 0,
    Center = 1,
    Right  = -1
}
/// Ellipsis preserves explicit line breaks; Wrap additionally breaks at whitespace
/// and, for overlong tokens, at extended grapheme boundaries. Neither mode edits text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextLayout
{
    #[default]
    Ellipsis,
    Wrap
}
impl From<TextLayout> for i16
{
    fn from(value: TextLayout) -> Self
    {
        if value == TextLayout::Wrap { mac_abi::controls::TEXT_WRAP } else { 0 }
    }
}
impl TryFrom<i16> for TextLayout
{
    type Error = super::Error;

    fn try_from(value: i16) -> Result<Self>
    {
        match value
        {
            0 => Ok(Self::Ellipsis),
            mac_abi::controls::TEXT_WRAP => Ok(Self::Wrap),
            _ => Err(super::Error::BadArgument)
        }
    }
}
#[derive(Clone, Copy)]
pub struct StaticTextSpec<'a>
{
    pub bounds:    Rect,
    pub text:      &'a str,
    pub font:      FontSelection,
    pub alignment: TextAlignment,
    pub layout:    TextLayout,
    pub reflow:    super::Reflow
}
pub struct StaticText<'a>
{
    control: Control<'a>
}
#[derive(Clone, Copy)]
pub struct StaticTextRef<'a>
{
    control: super::ControlRef<'a>
}
impl<'a> super::ControlRef<'a>
{
    /// Each operation checks the resident control family and ownership.
    pub fn static_text(self) -> StaticTextRef<'a>
    {
        StaticTextRef { control: self }
    }
}
impl<'a> StaticText<'a>
{
    pub fn control(&self) -> super::ControlRef<'a>
    {
        self.control.control()
    }

    pub fn into_control(self) -> Control<'a>
    {
        self.control
    }

    pub fn text(&self) -> StaticTextRef<'a>
    {
        self.control().static_text()
    }

    pub fn id(&self) -> super::ControlId
    {
        self.control.id()
    }

    pub fn set_bounds(&mut self, bounds: Rect) -> Result<()>
    {
        self.text().set_bounds(bounds)
    }

    pub fn set(&mut self, text: &str) -> Result<()>
    {
        self.text().set(text)
    }

    pub fn draw(&self) -> Result<()>
    {
        self.control().draw()
    }
}
impl StaticTextRef<'_>
{
    fn update(self, flags: u32, standard: StandardState) -> Result<()>
    {
        let mut call =
            mac_abi::controls::ControlCall { operation:
                                                 mac_abi::controls::SET_STATIC_TEXT_OBJECT,
                                             id: self.control.id().0,
                                             flags,
                                             standard,
                                             ..Default::default() };
        self.control.client().control(&mut call)
    }

    /// Preserve the latest resident rectangle and all other properties.
    pub fn set(self, text: &str) -> Result<()>
    {
        self.update(mac_abi::controls::TEXT_CONTENT,
                    text_state(Rect::ZERO, text, FontSelection::default(), 0)?)
    }

    pub fn set_bounds(self, bounds: Rect) -> Result<()>
    {
        self.update(mac_abi::controls::TEXT_BOUNDS,
                    StandardState { bounds,
                                    ..Default::default() })
    }

    pub fn set_font(self, font: FontSelection) -> Result<()>
    {
        self.update(mac_abi::controls::TEXT_FONT,
                    StandardState { font,
                                    ..Default::default() })
    }

    pub fn set_alignment(self, alignment: TextAlignment) -> Result<()>
    {
        self.update(mac_abi::controls::TEXT_ALIGNMENT,
                    StandardState { value: alignment as i16,
                                    ..Default::default() })
    }

    pub fn set_layout(self, layout: TextLayout) -> Result<()>
    {
        self.update(mac_abi::controls::TEXT_LAYOUT,
                    StandardState { flags: layout.into(),
                                    ..Default::default() })
    }
}
fn text_state(bounds: Rect, text: &str, font: FontSelection, alignment: i16)
              -> Result<StandardState>
{
    // Never silently truncate a document name or diagnostic at the ABI boundary.
    if text.len() > mac_abi::INLINE_STRING_BYTES
    {
        return Err(super::Error::BadArgument);
    }
    Ok(StandardState { font,
                       value: alignment,
                       minimum: 0,
                       maximum: 0,
                       ..StandardState::new(bounds, text) })
}
impl<'a> Controls<'a>
{
    pub fn create_progress_bar(&self, window: WindowId, bounds: Rect, progress: Progress)
                               -> Result<ProgressBar<'a>>
    {
        self.create_progress_bar_with_style(window, bounds, progress, ProgressStyle::Color)
    }

    pub fn create_progress_bar_with_style(&self, window: WindowId, bounds: Rect,
                                          progress: Progress, style: ProgressStyle)
                                          -> Result<ProgressBar<'a>>
    {
        let state = progress.state(bounds)?;
        let kind = match style
        {
            ProgressStyle::Color => StandardKind::ProgressBar,
            ProgressStyle::Monochrome => StandardKind::MonochromeProgressBar
        };
        Ok(ProgressBar { control: self.create_standard(window, kind, state, crate::Reflow::No)?,
                         state,
                         kind })
    }

    pub fn create_static_text(&self, window: WindowId, bounds: Rect, text: &str,
                              font: FontSelection, alignment: TextAlignment)
                              -> Result<StaticText<'a>>
    {
        self.create_static_text_with(window,
                                     StaticTextSpec { bounds,
                                                      text,
                                                      font,
                                                      alignment,
                                                      layout: TextLayout::Ellipsis,
                                                      reflow: crate::Reflow::No })
    }

    /// Opt-in wrapping and reflow with the same owned lifetime as other controls.
    pub fn create_static_text_with(&self, window: WindowId, spec: StaticTextSpec<'_>)
                                   -> Result<StaticText<'a>>
    {
        let mut state = text_state(spec.bounds, spec.text, spec.font, spec.alignment as i16)?;
        state.flags = spec.layout.into();
        Ok(StaticText { control: self.create_standard(window,
                                                      StandardKind::StaticText,
                                                      state,
                                                      spec.reflow)? })
    }
}

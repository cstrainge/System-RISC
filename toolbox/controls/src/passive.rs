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
pub struct StaticText<'a>
{
    control: Control<'a>,
    state:   StandardState
}
impl StaticText<'_>
{
    /// Move/resize the existing managed object; keep identity and text storage.
    pub fn set_bounds(&mut self, bounds: Rect) -> Result<()>
    {
        if self.state.bounds == bounds
        {
            return Ok(());
        }
        let next = StandardState { bounds,
                                   ..self.state };
        self.control.control().configure(StandardKind::StaticText, next)?;
        self.state = next;
        Ok(())
    }

    pub fn id(&self) -> super::ControlId
    {
        self.control.id()
    }

    pub fn set(&mut self, text: &str) -> Result<()>
    {
        let next = text_state(self.state.bounds, text, self.state.font, self.state.value)?;
        if self.state == next
        {
            return Ok(());
        }
        self.control.control().configure(StandardKind::StaticText, next)?;
        self.state = next;
        Ok(())
    }

    pub fn draw(&self) -> Result<()>
    {
        self.control.control().draw()
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
        Ok(ProgressBar { control: self.create_standard(window, kind, state)?,
                         state,
                         kind })
    }

    pub fn create_static_text(&self, window: WindowId, bounds: Rect, text: &str,
                              font: FontSelection, alignment: TextAlignment)
                              -> Result<StaticText<'a>>
    {
        let state = text_state(bounds, text, font, alignment as i16)?;
        Ok(StaticText { control: self.create_standard(window, StandardKind::StaticText, state)?,
                        state })
    }
}

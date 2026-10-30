//! Native control-family values and their private wire codecs.
use mac_abi::{controls, definitions::ControlDefinitionState};

use crate::{Alignment, Error, FontSelection, Part, Rect, Result, StaticText};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind
{
    Push,
    Check,
    Radio
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptionMode
{
    System,
    ControlFont
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Highlight
{
    Normal,
    Pressed,
    Inactive,
    Disabled
}
#[derive(Clone, Copy, Debug)]
pub struct Button<'a>
{
    pub bounds:         Rect,
    pub title:          &'a str,
    pub enabled:        bool,
    pub highlight:      Highlight,
    pub value:          i16,
    pub default_button: bool,
    pub kind:           ButtonKind,
    pub caption:        CaptionMode,
    pub font:           FontSelection
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProgressRange
{
    pub completed:   u64,
    pub in_progress: u64,
    pub total:       u64,
    pub determinate: bool
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressStyle
{
    Color,
    Monochrome
}
#[derive(Clone, Copy, Debug)]
pub struct Progress
{
    pub bounds:   Rect,
    pub progress: ProgressRange,
    pub style:    ProgressStyle
}
/// Table frame state. Grid calculation and immutable cell outlines are separate borrowed messages.
#[derive(Clone, Copy, Debug)]
pub struct Table
{
    pub bounds:  Rect,
    pub enabled: bool,
    pub lines:   toolbox_controls::GridLines
}
#[derive(Clone, Copy, Debug)]
pub struct Scrollbar
{
    pub bounds:      Rect,
    pub value:       i16,
    pub maximum:     i16,
    pub active:      bool,
    pub highlighted: Part
}
impl Default for Scrollbar
{
    fn default() -> Self
    {
        Self { bounds:      Rect::ZERO,
               value:       0,
               maximum:     0,
               active:      false,
               highlighted: Part::None }
    }
}
/// Typed reply; the binding verifies it belongs to the requested operation.
pub enum Reply
{
    Done,
    Measurement(crate::reflow::Measurement),
    Hit(Part),
    Value(u16)
}

pub(crate) trait Family
{
    type Wire;
    type View<'a>: Copy;
    const ID: i16;
    const TRACK: bool = false;
    const DELTA: bool = false;
    fn variation(value: i16) -> Result<()>;
    fn decode<'a>(wire: &'a Self::Wire, variation: i16) -> Result<Self::View<'a>>;
    fn bounds(state: &Self::View<'_>) -> Rect;
    fn change<'a>(_before: &'a ControlDefinitionState, _after: &'a ControlDefinitionState,
                  _variation: i16)
                  -> Result<(Self::View<'a>, Self::View<'a>)>
    {
        Err(Error::NotSupported)
    }
}
pub(crate) struct StaticFamily;
pub(crate) struct ButtonFamily;
pub(crate) struct ProgressFamily;
pub(crate) struct ScrollbarFamily;
pub(crate) struct TableFamily;

impl Family for TableFamily
{
    type View<'a> = Table;
    type Wire = ControlDefinitionState;

    const ID: i16 = controls::CDEF_TABLE;

    fn variation(value: i16) -> Result<()>
    {
        if value == 0 { Ok(()) } else { Err(Error::NotSupported) }
    }

    fn decode<'a>(wire: &'a Self::Wire, _: i16) -> Result<Self::View<'a>>
    {
        if wire.highlighted != 0
        {
            return Err(Error::BadArgument);
        }
        Ok(Table { bounds:  wire.bounds,
                   enabled: wire.enabled != 0,
                   lines:   wire.flags.try_into()? })
    }

    fn bounds(state: &Self::View<'_>) -> Rect
    {
        state.bounds
    }
}
fn title(wire: &ControlDefinitionState) -> Result<&str>
{
    core::str::from_utf8(wire.title.get(..wire.title_length as usize).ok_or(Error::BadArgument)?).map_err(|_|Error::BadArgument)
}
impl Family for StaticFamily
{
    type View<'a> = StaticText<'a>;
    type Wire = ControlDefinitionState;

    const ID: i16 = controls::CDEF_STATIC_TEXT;

    fn variation(value: i16) -> Result<()>
    {
        if value == 0 { Ok(()) } else { Err(Error::NotSupported) }
    }

    fn decode<'a>(wire: &'a Self::Wire, _: i16) -> Result<Self::View<'a>>
    {
        if wire.flags != 0 || wire.highlighted != 0 || wire.minimum != 0 || wire.maximum != 0
        {
            return Err(Error::BadArgument);
        }
        let alignment = match wire.value
        {
            -1 => Alignment::Right,
            0 => Alignment::Left,
            1 => Alignment::Center,
            _ => return Err(Error::BadArgument)
        };
        Ok(StaticText { bounds: wire.bounds,
                        text: title(wire)?,
                        font: wire.font,
                        enabled: wire.enabled != 0,
                        alignment })
    }

    fn bounds(state: &Self::View<'_>) -> Rect
    {
        state.bounds
    }
}
impl Family for ButtonFamily
{
    type View<'a> = Button<'a>;
    type Wire = ControlDefinitionState;

    const ID: i16 = 0;

    fn variation(value: i16) -> Result<()>
    {
        if matches!(value,0..=2|8..=10) { Ok(()) } else { Err(Error::NotSupported) }
    }

    fn decode<'a>(wire: &'a Self::Wire, variation: i16) -> Result<Self::View<'a>>
    {
        if wire.flags & !mac_abi::definitions::DEFAULT_BUTTON != 0
        {
            return Err(Error::BadArgument);
        }
        let kind = match variation & 7
        {
            0 => ButtonKind::Push,
            1 => ButtonKind::Check,
            2 => ButtonKind::Radio,
            _ => return Err(Error::NotSupported)
        };
        let default_button = wire.flags != 0;
        if default_button && kind != ButtonKind::Push
        {
            return Err(Error::BadArgument);
        }
        let highlight = match wire.highlighted
        {
            0 => Highlight::Normal,
            254 => Highlight::Inactive,
            255 => Highlight::Disabled,
            _ => Highlight::Pressed
        };
        Ok(Button { bounds: wire.bounds,
                    title: title(wire)?,
                    enabled: wire.enabled != 0,
                    highlight,
                    value: wire.value,
                    default_button,
                    kind,
                    caption: if variation & 8 == 0
                    {
                        CaptionMode::System
                    }
                    else
                    {
                        CaptionMode::ControlFont
                    },
                    font: wire.font })
    }

    fn bounds(state: &Self::View<'_>) -> Rect
    {
        state.bounds
    }
}
impl Family for ProgressFamily
{
    type View<'a> = Progress;
    type Wire = ControlDefinitionState;

    const DELTA: bool = true;
    const ID: i16 = controls::CDEF_PROGRESS_BAR;

    fn variation(value: i16) -> Result<()>
    {
        if matches!(value, 0 | 1) { Ok(()) } else { Err(Error::NotSupported) }
    }

    fn decode(wire: &Self::Wire, variation: i16) -> Result<Progress>
    {
        if wire.progress.reserved != 0
           || wire.progress.determinate > 1
           || wire.flags != 0
           || wire.highlighted != 0
           || wire.title_length != 0
        {
            return Err(Error::BadArgument);
        }
        Ok(Progress { bounds:   wire.bounds,
                      progress: ProgressRange { completed:   wire.progress.completed,
                                                in_progress: wire.progress.in_progress,
                                                total:       wire.progress.total,
                                                determinate: wire.progress.determinate != 0 },
                      style:    if variation == 0
                      {
                          ProgressStyle::Color
                      }
                      else
                      {
                          ProgressStyle::Monochrome
                      } })
    }

    fn bounds(state: &Progress) -> Rect
    {
        state.bounds
    }

    fn change(before: &ControlDefinitionState, after: &ControlDefinitionState, variation: i16)
              -> Result<(Progress, Progress)>
    {
        Ok((Self::decode(before, variation)?, Self::decode(after, variation)?))
    }
}
impl Family for ScrollbarFamily
{
    type View<'a> = Scrollbar;
    type Wire = controls::ScrollbarState;

    const ID: i16 = 1;
    const TRACK: bool = true;

    fn variation(value: i16) -> Result<()>
    {
        if value == 0 { Ok(()) } else { Err(Error::NotSupported) }
    }

    fn decode(wire: &Self::Wire, _: i16) -> Result<Scrollbar>
    {
        if wire.reserved != 0
        {
            return Err(Error::BadArgument);
        }
        // Original drawing treats unknown highlight parts as unhighlighted.
        let highlighted = Part::from_code(wire.highlighted).unwrap_or(Part::None);
        Ok(Scrollbar { bounds: wire.bounds,
                       value: wire.value,
                       maximum: wire.maximum,
                       active: wire.active != 0,
                       highlighted })
    }

    fn bounds(state: &Scrollbar) -> Rect
    {
        state.bounds
    }
}

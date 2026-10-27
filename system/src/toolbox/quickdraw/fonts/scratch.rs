//! Fallible, owned scratch for the resident font renderers.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_abi::{BAD_ARGUMENT, NO_MEMORY, Status};
use skrifa::outline::{DrawError,
                      pen::{OutlinePen, PathElement}};

pub(super) fn filled<T: Copy>(length: usize, value: T) -> Result<Vec<T>, Status>
{
    let mut data = super::super::storage::vector(length).map_err(|_| NO_MEMORY)?;
    data.resize(length, value);
    Ok(data)
}
pub(super) fn push<T>(data: &mut Vec<T>, value: T) -> Result<(), Status>
{
    data.try_reserve(1).map_err(|_| NO_MEMORY)?;
    data.push(value);
    Ok(())
}
pub(super) fn draw_error(error: DrawError) -> Status
{
    match error
    {
        DrawError::InsufficientMemory => NO_MEMORY,
        DrawError::NoSources
        | DrawError::GlyphNotFound(_)
        | DrawError::RecursionLimitExceeded(_)
        | DrawError::TooManyPoints(_)
        | DrawError::HintingFailed(_)
        | DrawError::InvalidAnchorPoint(..)
        | DrawError::PostScript(_)
        | DrawError::Malformed
        | DrawError::HarfBuzzHintingUnsupported => BAD_ARGUMENT
    }
}
/// OutlinePen cannot return errors. Retain the first refusal and stop admitting
/// commands; the caller checks it before interpreting or painting any path.
#[derive(Default)]
pub(super) struct Path
{
    elements: Vec<PathElement>,
    error:    Option<Status>
}
impl Path
{
    fn append(&mut self, value: PathElement)
    {
        if self.error.is_some()
        {
            return;
        }
        if self.elements.len() == 131072
        {
            self.error = Some(BAD_ARGUMENT);
            return;
        }
        if let Err(error) = push(&mut self.elements, value)
        {
            self.error = Some(error);
        }
    }

    pub(super) fn finish(self) -> Result<Vec<PathElement>, Status>
    {
        match self.error
        {
            Some(error) => Err(error),
            None => Ok(self.elements)
        }
    }
}
impl OutlinePen for Path
{
    fn move_to(&mut self, x: f32, y: f32)
    {
        self.append(PathElement::MoveTo { x,
                                          y });
    }

    fn line_to(&mut self, x: f32, y: f32)
    {
        self.append(PathElement::LineTo { x,
                                          y });
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32)
    {
        self.append(PathElement::QuadTo { cx0,
                                          cy0,
                                          x,
                                          y });
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32)
    {
        self.append(PathElement::CurveTo { cx0,
                                           cy0,
                                           cx1,
                                           cy1,
                                           x,
                                           y });
    }

    fn close(&mut self)
    {
        self.append(PathElement::Close);
    }
}

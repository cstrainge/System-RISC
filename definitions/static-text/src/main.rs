//! Independently relocatable static-text CDEF. Text sizing and drawing use the
//! same resident font metrics and grapheme-safe line selection.
#![cfg_attr(target_arch = "riscv64", no_std)]
#![cfg_attr(target_arch = "riscv64", no_main)]
#![deny(unsafe_code)]
use mac_definition::{Alignment, Error, Message, Part, Point, Reply, Result, Services, StaticText,
                     SurfacePort,
                     reflow::{self, AxisConstraint, Extent, Measurement, Requirements, Size}};
use unicode_segmentation::UnicodeSegmentation;

const ELLIPSIS: &str = "…";
fn width(services: Services<'_>, state: StaticText<'_>, text: &str) -> Result<i16>
{
    if text.is_empty()
    {
        return Ok(0);
    }
    let width = services.font_width(state.font, text)?;
    if width < 0 { Err(Error::BadArgument) } else { Ok(width) }
}
fn hard_lines(text: &str) -> impl Iterator<Item = &str>
{
    let mut remaining = Some(text);
    core::iter::from_fn(move || {
        let text = remaining.take()?;
        if let Some(end) = text.find(['\r', '\n'])
        {
            let skip = if text[end..].starts_with("\r\n") { 2 } else { 1 };
            remaining = Some(&text[end + skip..]);
            Some(&text[..end])
        }
        else
        {
            Some(text)
        }
    })
}
fn line_height(services: Services<'_>, state: StaticText<'_>) -> Result<i32>
{
    let m = services.font_metrics(state.font)?;
    Ok((i32::from(m.ascent) + i32::from(m.descent) + i32::from(m.leading)).max(1))
}
fn size(w: i32, h: i32) -> Result<Size>
{
    Ok(Size::new(Extent::from_wide(i64::from(w)).map_err(|_| Error::BadArgument)?,
                 Extent::from_wide(i64::from(h)).map_err(|_| Error::BadArgument)?))
}

/// Visit lines without allocation. Whole hard lines get one width query when
/// they fit. Wrapped candidates use whole substrings, preserving font shaping;
/// oversized tokens advance by complete extended graphemes, even at zero width.
/// Returning false stops before measuring invisible trailing lines during paint.
fn wrapped(services: Services<'_>, state: StaticText<'_>, available: i16,
           mut emit: impl FnMut(&str, i16) -> Result<bool>)
           -> Result<()>
{
    for mut rest in hard_lines(state.text)
    {
        loop
        {
            let all = width(services, state, rest)?;
            if all <= available || rest.is_empty()
            {
                if !emit(rest, all)?
                {
                    return Ok(());
                }
                break;
            }
            let soft = |cluster: &str| {
                cluster.chars().all(|c| c.is_whitespace() && c != '\u{a0}' && c != '\u{202f}')
            };
            let mut word: Option<(usize, usize, i16)> = None;
            let mut in_space = false;
            for (offset, cluster) in rest.grapheme_indices(true)
            {
                if soft(cluster)
                {
                    if !in_space && offset != 0
                    {
                        let measured = width(services, state, &rest[..offset])?;
                        if measured > available
                        {
                            break;
                        }
                        word = Some((offset, offset + cluster.len(), measured));
                    }
                    else if let Some((_, consumed, _)) = &mut word
                    {
                        *consumed = offset + cluster.len();
                    }
                    in_space = true;
                }
                else
                {
                    in_space = false;
                }
            }
            let (end, consumed, measured) = if let Some(word) = word
            {
                word
            }
            else
            {
                // Only an overlong token needs per-grapheme candidate queries.
                let mut fit = 0;
                let mut fit_width = 0;
                for (offset, cluster) in rest.grapheme_indices(true)
                {
                    let end = offset + cluster.len();
                    let current = width(services, state, &rest[..end])?;
                    if current > available
                    {
                        if fit == 0
                        {
                            fit = end;
                            fit_width = current;
                        }
                        break;
                    }
                    fit = end;
                    fit_width = current;
                }
                (fit, fit, fit_width)
            };
            if !emit(&rest[..end], measured)?
            {
                return Ok(());
            }
            rest = &rest[consumed..];
            // A whitespace scalar can be the base of a larger grapheme. Trim
            // only complete whitespace graphemes, never its base alone.
            let skip = rest.grapheme_indices(true)
                           .take_while(|(_, cluster)| soft(cluster))
                           .map(|(offset, cluster)| offset + cluster.len())
                           .last()
                           .unwrap_or(0);
            rest = &rest[skip..];
            if rest.is_empty()
            {
                break;
            }
        }
    }
    Ok(())
}

fn measure(services: Services<'_>, state: StaticText<'_>, constraints: reflow::Constraints)
           -> Result<Reply>
{
    let height = line_height(services, state)?;
    let mut natural_width = 0;
    let mut lines = 0;
    for line in hard_lines(state.text)
    {
        natural_width = natural_width.max(i32::from(width(services, state, line)?));
        lines += 1;
    }
    let minimum_width =
        if state.text.is_empty() { 0 } else { i32::from(width(services, state, ELLIPSIS)?) };
    let minimum = size(minimum_width, height)?;
    let intrinsic = Requirements::new(minimum, size(natural_width, lines * height)?);
    let mut constrained = intrinsic;
    if state.layout == mac_definition::TextLayout::Wrap
    {
        let available = match constraints.width
        {
            AxisConstraint::Unbounded => None,
            AxisConstraint::AtMost(v) =>
            {
                Some(v.pixels().min(intrinsic.preferred().width().pixels()))
            }
            AxisConstraint::Exact(v) => Some(v.pixels())
        };
        if let Some(available) = available
        {
            let mut rows = 0;
            wrapped(services, state, available, |_, _| {
                rows += 1;
                Ok(true)
            })?;
            // Shortage may assign below minimum, but must not rewrite the
            // content's requirements. Height constraints clip rather than erase.
            constrained = Requirements::new(minimum, size(i32::from(available), rows * height)?);
        }
    }
    Ok(Reply::Measurement(Measurement { intrinsic,
                                        constrained }))
}

fn draw(canvas: &mut SurfacePort<'_>, services: Services<'_>, state: StaticText<'_>, color: u32)
        -> Result<()>
{
    let r = state.bounds;
    if r.is_empty()
    {
        return Ok(());
    }
    let available =
        i16::try_from(i32::from(r.right) - i32::from(r.left)).map_err(|_| Error::BadArgument)?;
    let metrics = services.font_metrics(state.font)?;
    let height = line_height(services, state)?;
    let mut top = i32::from(r.top);
    let mut paint = |line: &str, line_width: i16| -> Result<bool> {
        if top >= i32::from(r.bottom)
        {
            return Ok(false);
        }
        let x =
            i32::from(r.left)
            + match state.alignment
            {
                Alignment::Left => 0,
                Alignment::Center => ((i32::from(available) - i32::from(line_width)) / 2).max(0),
                Alignment::Right => (i32::from(available) - i32::from(line_width)).max(0)
            };
        let baseline = top + i32::from(metrics.ascent);
        if baseline <= i32::from(i16::MAX) && !line.is_empty()
        {
            canvas.font_text_clipped(state.font,
                                     Point { h: x as i16,
                                             v: baseline as i16 },
                                     line,
                                     (i32::from(r.right) - x) as i16,
                                     color)?;
        }
        top += height;
        Ok(top < i32::from(r.bottom))
    };
    if state.layout == mac_definition::TextLayout::Wrap
    {
        return wrapped(services, state, available, paint);
    }
    // One bounded scratch buffer only for a line that actually needs ellipsis.
    let mut shortened = [0u8; mac_definition::INLINE_STRING_BYTES];
    for line in hard_lines(state.text)
    {
        let full = width(services, state, line)?;
        if full <= available
        {
            if !paint(line, full)?
            {
                break;
            }
            continue;
        }
        let dots = width(services, state, ELLIPSIS)?;
        if dots > available
        {
            if !paint("", 0)?
            {
                break;
            }
            continue;
        }
        let mut chosen = 0;
        let mut measured = dots;
        for (offset, grapheme) in line.grapheme_indices(true)
        {
            let end = offset + grapheme.len();
            if end + ELLIPSIS.len() > shortened.len()
            {
                break;
            }
            shortened[..end].copy_from_slice(&line.as_bytes()[..end]);
            shortened[end..end + ELLIPSIS.len()].copy_from_slice(ELLIPSIS.as_bytes());
            let candidate =
                core::str::from_utf8(&shortened[..end + ELLIPSIS.len()]).map_err(|_| {
                                                                            Error::BadArgument
                                                                        })?;
            let candidate_width = width(services, state, candidate)?;
            if candidate_width > available
            {
                break;
            }
            chosen = end;
            measured = candidate_width;
        }
        shortened[..chosen].copy_from_slice(&line.as_bytes()[..chosen]);
        shortened[chosen..chosen + ELLIPSIS.len()].copy_from_slice(ELLIPSIS.as_bytes());
        let line = core::str::from_utf8(&shortened[..chosen + ELLIPSIS.len()]).map_err(|_| {
                                                                                  Error::BadArgument
                                                                              })?;
        if !paint(line, measured)?
        {
            break;
        }
    }
    Ok(())
}
#[mac_definition::cdef_main]
fn main<'a>(message: Message<'a, StaticText<'a>>, services: Services<'a>) -> Result<Reply>
{
    match message
    {
        Message::DrawGrid { .. } | Message::CalculateGridAxis(_) =>
        {
            return Err(Error::NotSupported);
        }
        Message::Reflow { request,
                          state,
                          .. } =>
        {
            if services.reflow() != mac_definition::Reflow::Yes
            {
                return Err(Error::NotSupported);
            }
            return match request
            {
                reflow::Message::AskSize(constraints) => measure(services, state, constraints),
                reflow::Message::AssignRect(_)
                | reflow::Message::SetVisibleRect(_)
                | reflow::Message::InvalidateLayout(_) => Ok(Reply::Done),
                _ => Err(Error::NotSupported)
            };
        }
        Message::Draw { state,
                        mut port } =>
        {
            let theme = services.theme()?;
            draw(&mut port,
                 services,
                 state,
                 if state.enabled { theme.text } else { theme.disabled_text })?;
        }
        Message::Validate(state) =>
        {
            services.font_metrics(state.font)?;
            // Reject unrepresentable content before a retained state can commit
            // and later fail to paint. Height remains constraint-dependent.
            for line in hard_lines(state.text)
            {
                width(services, state, line)?;
            }
        }
        Message::HitTest { .. } => return Ok(Reply::Hit(Part::None)),
        Message::CalculateRegions(_) | Message::Initialize(_) | Message::Dispose(_) => (),
        Message::ChangeBounds { .. } | Message::Track { .. } => return Err(Error::NotSupported)
    }
    Ok(Reply::Done)
}

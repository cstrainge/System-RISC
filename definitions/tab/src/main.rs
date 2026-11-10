//! Tab control: a strip of tabs, and in reflow mode the box they open onto.
//!
//! A tab is the System 7 push button with its bottom taken off: the same one
//! pixel outline, the same rounded top corners, the same inversion while it is
//! pressed. Nothing here is shaded or bevelled, because nothing in that look
//! is. There is no hover state for the same reason.
//!
//! Each tab is its own part, so a press tracks, highlights and activates
//! exactly as a button's does. What activation means is not decided here.
//!
//! In reflow mode the control owns its views, and draws the box they live in:
//! the whole tab area is outlined, and the outline is left open under the
//! selected tab so that tab and box are one surface. Without reflow the
//! application owns what is under the strip and only the tabs are drawn.
//!
//! Each tab is as wide as its title plus the button padding. When the tabs do
//! not all fit, two arrows appear at the right end of the strip and only whole
//! tabs are drawn. The selected title has a dotted underline in Chicago and
//! is bold in Geneva, since Chicago has no bold to mark it with.
#![cfg_attr(systemrisc, no_std)]
#![cfg_attr(systemrisc, no_main)]
#![deny(unsafe_code)]
use mac_definition::{FontSelection, Message, Part, Point, Rect, Reflow, Reply, Result, Services,
                     SurfacePort, Tab, Theme, cdef_main, properties::Voice};

/// Space either side of a title, as a push button gives its caption.
const PADDING: i32 = 10;
/// Space above and below a title. With Chicago this makes a tab the height of
/// a standard push button.
const LEADING: i32 = 3;
/// The strip starts this far in from the box's left edge.
const INSET: i32 = 6;
/// Width of one overflow arrow.
const ARROW: i32 = 15;
/// How far the outline steps in on each of a tab's top rows: the corner of a
/// push button of this height.
const CORNER: [i16; 3] = [3, 1, 1];

/// What is under a point.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Place
{
    Nothing,
    Back,
    On,
    Tab(u16)
}

#[cdef_main]
fn main<'a>(message: Message<'a, Tab<'a>>, services: Services<'a>) -> Result<Reply>
{
    match message
    {
        Message::Draw { state,
                        mut port } =>
        {
            draw(&mut port, &state, services)?;
            Ok(Reply::Done)
        }
        Message::HitTest { state,
                           point } =>
        {
            let part = if !state.enabled
            {
                Part::None
            }
            else
            {
                match place(&state, services, point)?
                {
                    Place::Nothing => Part::None,
                    Place::Back => Part::Up,
                    Place::On => Part::Down,
                    Place::Tab(position) => Part::Tab(position)
                }
            };
            Ok(Reply::Hit(part))
        }
        other => mac_definition::stub::reply(other, services)
    }
}

/// Where everything in the strip is, for one invocation.
struct Strip
{
    /// The row of tabs, top of the control down to the box's top edge.
    strip:    Rect,
    /// The box below it. Drawn only in reflow mode.
    pane:     Rect,
    /// Left edge of the first tab before scrolling.
    origin:   i32,
    /// Tabs are drawn only if they lie wholly between these.
    left:     i32,
    right:    i32,
    overflow: bool,
    ascent:   i32
}

fn plain(voice: Voice) -> FontSelection
{
    match voice
    {
        Voice::Chicago => FontSelection::default(),
        Voice::Geneva =>
        {
            FontSelection { family: 3,
                            size:   9,
                            face:   0 }
        }
    }
}

fn title_font(state: &Tab<'_>, key: u64) -> FontSelection
{
    let font = plain(state.voice);
    if state.selection == Some(key) && state.voice == Voice::Geneva
    {
        FontSelection { face: 1,
                        ..font }
    }
    else
    {
        font
    }
}

fn tab_width(state: &Tab<'_>, services: Services<'_>, key: u64, title: &str) -> Result<i32>
{
    Ok(i32::from(services.font_width(title_font(state, key), title)?) + PADDING * 2)
}

fn strip(state: &Tab<'_>, services: Services<'_>) -> Result<Strip>
{
    let b = state.bounds;
    let metrics = services.font_metrics(plain(state.voice))?;
    let ascent = i32::from(metrics.ascent);
    let height = (ascent + i32::from(metrics.descent) + LEADING * 2 + 2)
        .min(i32::from(b.bottom) - i32::from(b.top));
    let seam = i32::from(b.top) + height;
    // Neighbouring tabs share their border column.
    let mut total = 1;
    for tab in state.tabs.iter()
    {
        total += tab_width(state, services, tab.key, tab.title)? - 1;
    }
    let left = i32::from(b.left) + INSET;
    let mut right = i32::from(b.right) - INSET;
    let overflow = total > right - left;
    if overflow
    {
        right -= ARROW * 2;
    }
    let scroll = if overflow { state.strip_scroll_offset.min(i32::MAX as u32) as i32 } else { 0 };
    Ok(Strip { strip: Rect::new(b.left, b.top, b.right, seam as i16),
               pane: Rect::new(b.left, (seam - 1) as i16, b.right, b.bottom),
               origin: left - scroll,
               left,
               right,
               overflow,
               ascent })
}

/// Call `visit` with each tab that is drawn, its position in the list and its
/// rectangle. A tab that would be cut by either end of the strip is not drawn.
fn tabs<'a>(state: &Tab<'a>, services: Services<'_>, strip: &Strip,
            mut visit: impl FnMut(u16, mac_definition::properties::TabItem<'a>, Rect) -> Result<()>)
            -> Result<()>
{
    let mut x = strip.origin;
    for (position, tab) in state.tabs.iter().enumerate()
    {
        let width = tab_width(state, services, tab.key, tab.title)?;
        let end = x + width;
        if x >= strip.left && end <= strip.right
        {
            visit(position.min(u16::MAX as usize) as u16,
                  tab,
                  Rect::new(x as i16, strip.strip.top + 2, end as i16, strip.strip.bottom))?;
        }
        x = end - 1;
    }
    Ok(())
}

fn arrows(strip: &Strip) -> Option<(Rect, Rect)>
{
    if !strip.overflow
    {
        return None;
    }
    let top = strip.strip.top + 2;
    let bottom = strip.strip.bottom - 2;
    let edge = i32::from(strip.strip.right) - INSET;
    Some((Rect::new((edge - ARROW * 2 + 1) as i16, top, (edge - ARROW + 1) as i16, bottom),
          Rect::new((edge - ARROW) as i16, top, edge as i16, bottom)))
}

fn place(state: &Tab<'_>, services: Services<'_>, point: Point) -> Result<Place>
{
    if !state.bounds.contains(point)
    {
        return Ok(Place::Nothing);
    }
    let strip = strip(state, services)?;
    if let Some((back, on)) = arrows(&strip)
    {
        if back.contains(point)
        {
            return Ok(Place::Back);
        }
        if on.contains(point)
        {
            return Ok(Place::On);
        }
    }
    let mut found = Place::Nothing;
    tabs(state, services, &strip, |position, _, bounds| {
        if bounds.contains(point)
        {
            found = Place::Tab(position);
        }
        Ok(())
    })?;
    Ok(found)
}

fn draw(port: &mut SurfacePort<'_>, state: &Tab<'_>, services: Services<'_>) -> Result<()>
{
    let b = state.bounds;
    if b.is_empty()
    {
        return Ok(());
    }
    let theme = services.theme()?;
    let strip = strip(state, services)?;
    let boxed = services.reflow() == Reflow::Yes;
    let outline = if state.enabled { theme.control_outline } else { theme.separator };
    port.paint_rect(strip.strip, theme.content)?;
    if boxed
    {
        // What is in the box is its views'. Only its outline is drawn.
        port.frame_rect(strip.pane, outline)?;
    }
    let baseline = (i32::from(strip.strip.top) + 2 + LEADING + strip.ascent) as i16;
    tabs(state, services, &strip, |position, tab, bounds| {
        let active = state.selection == Some(tab.key);
        // Tracking highlights the pressed part, as it does a button.
        let pressed = state.enabled && state.highlighted == Part::Tab(position).code() as u32;
        let (face, ink) = if pressed
        {
            (theme.control_outline, theme.content)
        }
        else if state.enabled
        {
            (theme.content, theme.text)
        }
        else
        {
            (theme.content, theme.disabled_text)
        };
        lid(port, bounds, outline, face)?;
        if boxed && active
        {
            // Open the box's outline under the selected tab: its sides run
            // into the box and there is no line beneath it.
            port.paint_rect(Rect::new(bounds.left + 1,
                                      strip.pane.top,
                                      bounds.right - 1,
                                      strip.pane.top + 1),
                            face)?;
        }
        let font = title_font(state, tab.key);
        let width = services.font_width(font, tab.title)?;
        let left = bounds.left + PADDING as i16;
        port.font_text_clipped(font,
                               Point { h: left,
                                       v: baseline },
                               tab.title,
                               width,
                               ink)?;
        if active && state.voice == Voice::Chicago
        {
            // A dotted rule: every other pixel, starting on the first.
            let mut x = left;
            while x < left + width
            {
                port.paint_rect(Rect::new(x, baseline + 2, x + 1, baseline + 3), ink)?;
                x += 2;
            }
        }
        Ok(())
    })?;
    if let Some((back, on)) = arrows(&strip)
    {
        arrow(port, back, false, state, outline, theme)?;
        arrow(port, on, true, state, outline, theme)?;
    }
    Ok(())
}

/// A push button with no bottom edge: rounded top corners, straight sides
/// running down to where the tab meets what is below it.
fn lid(port: &mut SurfacePort<'_>, r: Rect, outline: u32, face: u32) -> Result<()>
{
    let (l, t, right, bottom) = (r.left, r.top, r.right, r.bottom);
    let rows = CORNER.len() as i16;
    for (row, inset) in CORNER.iter().enumerate()
    {
        let y = t + row as i16;
        // The outline on this row runs from this row's inset out to the one
        // above it, so the corner is one unbroken line.
        let above = if row == 0 { right - l - *inset * 2 } else { CORNER[row - 1] - *inset };
        if row == 0
        {
            port.paint_rect(Rect::new(l + *inset, y, right - *inset, y + 1), outline)?;
        }
        else
        {
            let run = above.max(1);
            port.paint_rect(Rect::new(l + *inset, y, l + *inset + run, y + 1), outline)?;
            port.paint_rect(Rect::new(right - *inset - run, y, right - *inset, y + 1), outline)?;
            let (from, to) = (l + *inset + run, right - *inset - run);
            if from < to
            {
                port.paint_rect(Rect::new(from, y, to, y + 1), face)?;
            }
        }
    }
    port.paint_rect(Rect::new(l + 1, t + rows, right - 1, bottom - 1), face)?;
    port.paint_rect(Rect::new(l, t + rows, l + 1, bottom), outline)?;
    port.paint_rect(Rect::new(right - 1, t + rows, right, bottom), outline)
}

/// One overflow arrow: a small framed box with a triangle pointing the way
/// the strip will move. It inverts while pressed, as a button does.
fn arrow(port: &mut SurfacePort<'_>, r: Rect, forward: bool, state: &Tab<'_>, outline: u32,
         theme: Theme)
         -> Result<()>
{
    let part = if forward { Part::Down } else { Part::Up };
    let pressed = state.enabled && state.highlighted == part.code() as u32;
    let (face, ink) = if pressed { (outline, theme.content) } else { (theme.content, outline) };
    port.paint_rect(r, face)?;
    port.frame_rect(r, outline)?;
    let middle = (i32::from(r.top) + i32::from(r.bottom)) / 2;
    let centre = (i32::from(r.left) + i32::from(r.right)) / 2;
    for step in 0..4
    {
        let x = if forward { centre + 1 - step } else { centre - 2 + step };
        port.paint_rect(Rect::new(x as i16,
                                  (middle - step) as i16,
                                  (x + 1) as i16,
                                  (middle + step + 1) as i16),
                        ink)?;
    }
    Ok(())
}

//! Tab control: a strip of button-like tabs over an outlined pane.
//!
//! The definition owns the strip's pixels and its geometry and nothing else.
//! The pane's contents belong to the child views; which view is live, and
//! what a press means, are the resident manager's to apply. A press is
//! reported the way a splitter reports a drag: as a position.
//!
//! Each tab is as wide as its title plus the button padding. When the tabs
//! do not all fit, two arrows appear at the right end of the strip and only
//! whole tabs are drawn. The selected tab is joined to the pane: the pane's
//! outline is broken under it. Its title is underlined in Chicago and bold in
//! Geneva, since Chicago has no bold to mark it with.
#![cfg_attr(systemrisc, no_std)]
#![cfg_attr(systemrisc, no_main)]
#![deny(unsafe_code)]
use mac_definition::{FontSelection, Message, Part, Point, Rect, Reply, Result, Services,
                     SurfacePort, Tab, Theme, cdef_main, properties::Voice};

/// Space either side of a title, as a push button gives its caption.
const PADDING: i32 = 10;
/// Space above and below a title.
const LEADING: i32 = 4;
/// The strip starts this far in from the pane's left edge.
const INSET: i32 = 6;
/// Width of one overflow arrow.
const ARROW: i32 = 15;

/// `Track` reports the tab under the pointer by its position in the list.
/// These are the answers that are not a tab.
const NOTHING: i16 = -1;
const SCROLL_BACK: i16 = -2;
const SCROLL_ON: i16 = -3;

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
            let part = match place(&state, services, point)?
            {
                _ if !state.enabled => Part::None,
                NOTHING => Part::None,
                SCROLL_BACK => Part::Up,
                SCROLL_ON => Part::Down,
                _ => Part::Button
            };
            Ok(Reply::Hit(part))
        }
        // A tab activates on release over the tab that was pressed. Anything
        // else, including a release over a different tab, is nothing.
        Message::Track { state,
                         anchor,
                         point } =>
        {
            let pressed = place(&state, services, anchor)?;
            let released = place(&state, services, point)?;
            Ok(Reply::Position(if pressed == released { pressed } else { NOTHING }))
        }
        other => mac_definition::stub::reply(other, services)
    }
}

/// Where everything in the strip is, for one invocation.
struct Strip
{
    /// The row of tabs, top of the control down to the pane's top edge.
    strip:    Rect,
    /// The outlined pane below it.
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

/// The selected title's font. Chicago marks selection with an underline.
fn selected(voice: Voice) -> FontSelection
{
    match voice
    {
        Voice::Chicago => plain(voice),
        Voice::Geneva =>
        {
            FontSelection { face: 1,
                            ..plain(voice) }
        }
    }
}

fn title_font(state: &Tab<'_>, key: u64) -> FontSelection
{
    if state.selection == Some(key) { selected(state.voice) } else { plain(state.voice) }
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
    let height = (ascent + i32::from(metrics.descent) + LEADING * 2)
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
            mut visit: impl FnMut(i16, mac_definition::properties::TabItem<'a>, Rect) -> Result<()>)
            -> Result<()>
{
    let mut x = strip.origin;
    for (index, tab) in state.tabs.iter().enumerate()
    {
        let width = tab_width(state, services, tab.key, tab.title)?;
        let end = x + width;
        if x >= strip.left && end <= strip.right
        {
            visit(index.min(i16::MAX as usize) as i16,
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

/// What is at `point`: a tab's position in the list, an arrow, or nothing.
fn place(state: &Tab<'_>, services: Services<'_>, point: Point) -> Result<i16>
{
    if !state.bounds.contains(point)
    {
        return Ok(NOTHING);
    }
    let strip = strip(state, services)?;
    if let Some((back, on)) = arrows(&strip)
    {
        if back.contains(point)
        {
            return Ok(SCROLL_BACK);
        }
        if on.contains(point)
        {
            return Ok(SCROLL_ON);
        }
    }
    let mut found = NOTHING;
    tabs(state, services, &strip, |index, _, bounds| {
        if bounds.contains(point)
        {
            found = index;
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
    let outline = if state.enabled { theme.control_outline } else { theme.separator };
    port.paint_rect(strip.strip, theme.content)?;
    // The pane's contents are its child views'. Only its outline is drawn.
    port.frame_rect(strip.pane, outline)?;
    let baseline = (i32::from(strip.strip.top) + 2 + LEADING + strip.ascent) as i16;
    tabs(state, services, &strip, |_, tab, bounds| {
        let active = state.selection == Some(tab.key);
        let hover = state.enabled && !active && state.hover == Some(tab.key);
        lid(port, bounds, outline, theme)?;
        if active
        {
            // The tab is the pane's lid: open the pane's outline beneath it.
            port.paint_rect(Rect::new(bounds.left + 1,
                                      strip.pane.top,
                                      bounds.right - 1,
                                      strip.pane.top + 1),
                            theme.content)?;
        }
        if hover
        {
            port.frame_rect(Rect::new(bounds.left + 2,
                                      bounds.top + 2,
                                      bounds.right - 2,
                                      bounds.bottom - 1),
                            theme.separator)?;
        }
        let font = title_font(state, tab.key);
        let width = services.font_width(font, tab.title)?;
        let left = bounds.left + PADDING as i16;
        port.font_text_clipped(font,
                               Point { h: left,
                                       v: baseline },
                               tab.title,
                               width,
                               if state.enabled { theme.text } else { theme.disabled_text })?;
        if active && state.voice == Voice::Chicago
        {
            port.paint_rect(Rect::new(left, baseline + 1, left + width, baseline + 2),
                            if state.enabled { theme.text } else { theme.disabled_text })?;
        }
        Ok(())
    })?;
    if let Some((back, on)) = arrows(&strip)
    {
        arrow(port, back, false, outline, theme)?;
        arrow(port, on, true, outline, theme)?;
    }
    Ok(())
}

/// A button with its top corners rounded and no bottom edge: the bottom is
/// where the tab meets the pane.
fn lid(port: &mut SurfacePort<'_>, r: Rect, outline: u32, theme: Theme) -> Result<()>
{
    let (l, t, right, bottom) = (r.left, r.top, r.right, r.bottom);
    port.paint_rect(Rect::new(l + 1, t + 2, right - 1, bottom - 1), theme.content)?;
    port.paint_rect(Rect::new(l + 2, t + 1, right - 2, t + 2), theme.content)?;
    port.paint_rect(Rect::new(l + 2, t, right - 2, t + 1), outline)?;
    port.paint_rect(Rect::new(l + 1, t + 1, l + 2, t + 2), outline)?;
    port.paint_rect(Rect::new(right - 2, t + 1, right - 1, t + 2), outline)?;
    port.paint_rect(Rect::new(l, t + 2, l + 1, bottom), outline)?;
    port.paint_rect(Rect::new(right - 1, t + 2, right, bottom), outline)
}

/// One overflow arrow: a small framed box with a triangle pointing the way
/// the strip will move.
fn arrow(port: &mut SurfacePort<'_>, r: Rect, forward: bool, outline: u32, theme: Theme)
         -> Result<()>
{
    port.paint_rect(r, theme.content)?;
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
                        outline)?;
    }
    Ok(())
}

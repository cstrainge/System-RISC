//! Suite placeholder plus the classic fixed-width popup used by Standard File.
#![cfg_attr(systemrisc, no_std)]
#![cfg_attr(systemrisc, no_main)]
#![deny(unsafe_code)]
use mac_definition::{DropMenu, FontSelection, Message, Part, Point, Reply, Result, Services,
                     cdef_main};

#[cdef_main]
fn main<'a>(message: Message<'a, DropMenu<'a>>, services: Services<'a>) -> Result<Reply>
{
    match message
    {
        Message::Draw { state,
                        mut port }
            if state.variation == 1 =>
        {
            let b = state.bounds;
            if b.is_empty()
            {
                return Ok(Reply::Done);
            }
            let theme = services.theme()?;
            let selected = state.enabled && state.highlighted != 0;
            let ink = if !state.enabled
            {
                theme.disabled_text
            }
            else if selected
            {
                theme.selected_text
            }
            else
            {
                theme.text
            };
            port.paint_rect(b, if selected { theme.selection } else { theme.content })?;
            port.frame_rect(b,
                            if state.enabled { theme.control_outline } else { theme.separator })?;
            // Narrow controls still draw within their actual borrowed bounds.
            if i32::from(b.right) - i32::from(b.left) >= 24
               && i32::from(b.bottom) - i32::from(b.top) >= 10
            {
                let font = FontSelection::default();
                let metrics = services.font_metrics(font)?;
                let baseline = i32::from(b.top)
                               + (i32::from(b.bottom)
                                  - i32::from(b.top)
                                  - i32::from(metrics.ascent)
                                  - i32::from(metrics.descent))
                                 / 2
                               + i32::from(metrics.ascent);
                port.font_text_clipped(font,
                                       Point { h: b.left + 8,
                                               v: baseline as i16 },
                                       state.caption,
                                       b.right - 21,
                                       ink)?;
                let x = b.right - 10;
                let y = b.top + (b.bottom - b.top) / 2 - 1;
                for row in 0..4
                {
                    port.stroke_line(Point { h: x - 3 + row,
                                             v: y + row },
                                     Point { h: x + 3 - row,
                                             v: y + row },
                                     ink)?;
                }
            }
            Ok(Reply::Done)
        }
        Message::HitTest { state,
                           point }
            if state.variation == 1 =>
        {
            Ok(Reply::Hit(if state.enabled && state.bounds.contains(point)
                          {
                              Part::Button
                          }
                          else
                          {
                              Part::None
                          }))
        }
        other => mac_definition::stub::reply(other, services)
    }
}

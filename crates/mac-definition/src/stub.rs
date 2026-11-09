//! Common V2 placeholder contract. Shared chrome is deliberate: each real CDEF
//! owns its typed family and will replace these ignored behavior arms as built.
#![forbid(unsafe_code)]
use mac_abi::controls::suite::Dimensions;

use crate::{Message, Part, Point, Rect, Reply, Result, Services, reflow};
pub trait View: Copy
{
    fn bounds(&self) -> Rect;
    fn limits(&self) -> (Dimensions, Option<Dimensions>);
}
pub fn reply<S: View>(message: Message<'_, S>, services: Services<'_>) -> Result<Reply>
{
    match message
    {
        Message::Draw { state,
                        mut port } =>
        {
            let b = state.bounds();
            if b.is_empty()
            {
                return Ok(Reply::Done);
            }
            let color = services.theme()?.separator;
            port.frame_rect(b, color)?;
            port.stroke_line(Point { h: b.left,
                                     v: b.top },
                             Point { h: b.right - 1,
                                     v: b.bottom - 1 },
                             color)?;
            port.stroke_line(Point { h: b.right - 1,
                                     v: b.top },
                             Point { h: b.left,
                                     v: b.bottom - 1 },
                             color)?;
            Ok(Reply::Done)
        }
        // No implemented interactive part exists yet. No misleading hit advertises
        // an action which this stub cannot take.
        Message::HitTest { .. } => Ok(Reply::Hit(Part::None)),
        Message::Reflow { state,
                          request: reflow::Message::AskSize(_),
                          creation } =>
        {
            let (min, max) = state.limits();
            let size = |d: Dimensions| -> Result<reflow::Size> {
                Ok(reflow::Size::new(reflow::Extent::from_wide(i64::from(d.width)).map_err(|_|crate::Error::BadArgument)?,reflow::Extent::from_wide(i64::from(d.height)).map_err(|_|crate::Error::BadArgument)?))
            };
            let minimum = size(min)?;
            let preferred = if let Some(max) = max
            {
                {
                    let max = size(max)?;
                    reflow::Size::new(creation.width().min(max.width()),
                                      creation.height().min(max.height()))
                }
            }
            else
            {
                creation
            };
            let requirements = reflow::Requirements::new(minimum, preferred);
            Ok(Reply::Measurement(reflow::Measurement { intrinsic:   requirements,
                                                        constrained: requirements }))
        }
        // Deliberate no-op: stub behavior neither mutates instance properties nor
        // emits notifications. The resident manager still owns identity, geometry,
        // validated reflow assignment, source custody and canonical disposal.
        Message::Track { .. } => Ok(Reply::Position(0)),
        Message::Control { .. }
        | Message::CalculateGridAxis(_)
        | Message::DrawGrid { .. }
        | Message::Reflow { .. }
        | Message::CalculateRegions(_)
        | Message::Initialize(_)
        | Message::Dispose(_)
        | Message::Validate(_)
        | Message::ChangeBounds { .. } => Ok(Reply::Done)
    }
}

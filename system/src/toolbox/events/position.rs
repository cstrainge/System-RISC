//! Shared native coordinate projection. No classic EventRecord layout changes.
#![forbid(unsafe_code)]
use mac_abi::{Event, EventKind, Rect, WindowId, events::EventPosition};

pub fn target(event: Event, front: Option<WindowId>) -> Option<WindowId>
{
    use mac_abi::{WINDOW_CLOSE_REQUEST, drag, events::WINDOW_GEOMETRY_CHANGED};
    let id = match event.kind
    {
        EventKind::MouseDown | EventKind::Update | EventKind::Activate => event.message,
        EventKind::MouseUp if event.message != 0 => event.message,
        EventKind::OsEvent if event.message == WINDOW_GEOMETRY_CHANGED => event.payload as u32,
        EventKind::OsEvent if event.message & 0xff00_0000 == WINDOW_CLOSE_REQUEST =>
        {
            event.message & 0x00ff_ffff
        }
        EventKind::OsEvent
            if matches!(event.message & 0xff00_0000,
                        drag::ENTERED_EVENT
                        | drag::WITHIN_EVENT
                        | drag::LEFT_EVENT
                        | drag::DROP_EVENT) =>
        {
            (event.payload >> 32) as u32
        }
        EventKind::Null
        | EventKind::MouseUp
        | EventKind::KeyDown
        | EventKind::KeyUp
        | EventKind::AutoKey => return front,
        _ => return None
    };
    (id != 0).then_some(WindowId(id))
}

pub fn project(event: Event, window: WindowId, bounds: Rect) -> EventPosition
{
    EventPosition { window: window.0,
                    h:      i32::from(event.where_.h) - i32::from(bounds.left),
                    v:      i32::from(event.where_.v) - i32::from(bounds.top) }
}

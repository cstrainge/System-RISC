//! Original System7.6.1 low-level OS queue observations and explicitly separate
//! native application-queue contracts, through the public userspace facade.
use mac_toolbox::{System,
                  events::{Events, Point,
                           classic::{BTN_STATE, EVERY_EVENT, Event, EventKind, SHIFT_KEY}}};

use super::{Failure, require};
include!("../data/events/observations.rs");
include!("../data/events/mouse-observations.rs");

fn original(action: &str) -> Result<(i32, bool, u16, u32), Failure>
{
    ORIGINAL.iter()
            .find(|row| row.0 == action)
            .map(|row| (row.1, row.2, row.3, row.4))
            .ok_or(Failure::Mismatch("missing original event observation"))
}
fn posted(events: Events<'_>, action: &str, kind: EventKind, message: u32) -> Result<(), Failure>
{
    let expected = original(action)?;
    let actual = events.os_post(kind, message).map_or_else(|error| error.status(), |_| 0);
    require(actual == expected.0, "original OS posting status")
}
fn read(events: Events<'_>, action: &'static str, mask: u16, remove: bool) -> Result<(), Failure>
{
    let expected = original(action)?;
    let value = events.os_event(mask, remove)?;
    if (value.available, value.event.kind as u16, value.event.message)
       == (expected.1, expected.2, expected.3)
    {
        Ok(())
    }
    else
    {
        Err(Failure::Observation { case:  action,
                                   field: "OS queue record" })
    }
}
fn kind(value: u16) -> EventKind
{
    match value
    {
        0 => EventKind::Null,
        1 => EventKind::MouseDown,
        2 => EventKind::MouseUp,
        3 => EventKind::KeyDown,
        4 => EventKind::KeyUp,
        5 => EventKind::AutoKey,
        6 => EventKind::Update,
        7 => EventKind::Disk,
        8 => EventKind::Activate,
        9 => EventKind::Reserved9,
        10 => EventKind::Network,
        11 => EventKind::Driver,
        12 => EventKind::Application1,
        13 => EventKind::Application2,
        14 => EventKind::MenuCommand,
        _ => EventKind::OsEvent
    }
}
pub fn run(system: &System<'_>) -> Result<(), Failure>
{
    let events = system.events();
    events.set_os_mask(EVERY_EVENT)?;
    events.os_flush(EVERY_EVENT, 0)?;
    read(events, "empty_null", EVERY_EVENT, true)?;
    posted(events, "post_key3_A", EventKind::KeyDown, 0x111)?;
    posted(events, "post_mouse1_B", EventKind::MouseDown, 0x222)?;
    posted(events, "post_key3_C", EventKind::KeyDown, 0x333)?;
    read(events, "peek_mouse1", 2, false)?;
    read(events, "peek_mouse1_again", 2, false)?;
    read(events, "get_mouse1", 2, true)?;
    read(events, "get_all_first", EVERY_EVENT, true)?;
    read(events, "get_all_second", EVERY_EVENT, true)?;
    read(events, "get_all_empty", EVERY_EVENT, true)?;
    for overlap in [false, true]
    {
        events.os_post(EventKind::KeyDown, if overlap { 0x777 } else { 0x444 })?;
        events.os_post(EventKind::MouseDown, if overlap { 0x888 } else { 0x555 })?;
        events.os_post(EventKind::KeyDown, if overlap { 0x999 } else { 0x666 })?;
        events.os_flush(if overlap { EVERY_EVENT } else { 8 }, 2)?;
        read(events,
             if overlap { "overlap_stop_first" } else { "flush_stop_first" },
             EVERY_EVENT,
             true)?;
        read(events,
             if overlap { "overlap_stop_second" } else { "flush_stop_second" },
             EVERY_EVENT,
             true)?;
        read(events,
             if overlap { "overlap_stop_empty" } else { "flush_stop_empty" },
             EVERY_EVENT,
             true)?;
    }
    events.set_os_mask(2)?;
    require(events.os_mask()? == 2, "OS posting mask roundtrip")?;
    posted(events, "mask_disabled_key", EventKind::KeyDown, 0xabc)?;
    posted(events, "mask_enabled_mouse", EventKind::MouseDown, 0xdef)?;
    read(events, "masked_read", EVERY_EVENT, true)?;
    events.set_os_mask(EVERY_EVENT)?;
    // All16 documented mask positions, including a queued null event. Original
    // codes beyond15 and PPostEvent queue-element pointers are a deferred ABI.
    for value in 0..16
    {
        let expected =
            ORIGINAL.iter()
                    .find(|row| {
                        row.0.starts_with("kind_") && row.0.ends_with("_read") && row.3 == value
                    })
                    .ok_or(Failure::Mismatch("missing original kind observation"))?;
        events.os_post(kind(value), expected.4)?;
        let actual = events.os_event(EVERY_EVENT, true)?;
        require(actual.available == expected.2
                && actual.event.kind as u16 == value
                && actual.event.message == expected.4,
                "original low-level event kind")?;
    }
    for message in 1..=40
    {
        events.os_post(EventKind::KeyDown, message)?;
    }
    for row in ORIGINAL.iter().filter(|row| row.0.starts_with("capacity_get_"))
    {
        read(events, row.0, EVERY_EVENT, true)?;
    }
    // PostEvent stamps a current global point/modifier/time snapshot. Different
    // device profiles have different pointer origins, so compare to the public
    // current state rather than copying original absolute screen coordinates.
    let before = events.input_state()?;
    events.os_post(EventKind::KeyDown, 0x1234)?;
    let stamped = events.os_event(EVERY_EVENT, true)?.event;
    let after = events.input_state()?;
    require(stamped.where_ == before.mouse && stamped.modifiers == before.modifiers.bits(),
            "posting captures physical input state")?;
    require(stamped.when.wrapping_sub(before.ticks) <= after.ticks.wrapping_sub(before.ticks),
            "posting stamp uses wrapping TickCount")?;
    require(after.ticks == after.uptime_ticks as u32 && after.uptime_ticks >= before.uptime_ticks,
            "native uptime and wrapping TickCount")?;
    for (index, row) in RELEASED_MOUSE.iter().enumerate()
    {
        match index
        {
            1 => events.os_post(EventKind::MouseDown, 0)?,
            2 => events.os_post(EventKind::MouseUp, 0)?,
            3 | 4 =>
            {
                events.os_get(6)?;
            }
            _ =>
            {}
        }
        require(events.button()? == row.1 && events.still_down()? == row.2,
                "original StillDown with physical released and synthetic queue")?;
    }
    native(system)?;
    system.log("EVENTS_USERSPACE_PASS original_queue=passed native_queue=passed\n")
          .map_err(|error| Failure::Service(error.status()))?;
    Ok(())
}

fn native(system: &System<'_>) -> Result<(), Failure>
{
    let events = system.events();
    events.flush(mac_toolbox::events::EventMask::from_bits_retain(EVERY_EVENT),
                 mac_toolbox::events::EventMask::from_bits_retain(0))?;
    let event = Event { payload:   0,
                        kind:      EventKind::Application1,
                        message:   0xfeed_beef,
                        when:      u32::MAX - 2,
                        where_:    Point { h: -17,
                                           v: 1024 },
                        modifiers: SHIFT_KEY | BTN_STATE };
    events.post(event.into())?;
    require(events.peek(mac_toolbox::events::EventMask::from_bits_retain(1 << 12))?
            == Some(event.into()),
            "native peek preserves full record")?;
    require(events.peek(mac_toolbox::events::EventMask::from_bits_retain(1 << 12))?
            == Some(event.into()),
            "native peek does not dequeue")?;
    require(events.os_get(EVERY_EVENT)?.is_none(), "native posting isolated from OS queue")?;
    require(events.next(mac_toolbox::events::EventMask::from_bits_retain(1 << 12))?.into_record()
            == event,
            "native next receives posted record")?;
    require(events.peek(mac_toolbox::events::EventMask::from_bits_retain(1 << 12))?.is_none(),
            "native next removes record")?;
    for message in 0..130
    {
        events.post(Event { message,
                            ..event }.into())?;
    }
    for message in 2..130
    {
        require(events.next(mac_toolbox::events::EventMask::from_bits_retain(1 << 12))?
                      .into_record()
                      .message
                == message,
                "native128 queue retention")?;
    }
    require(events.peek(mac_toolbox::events::EventMask::from_bits_retain(1 << 12))?.is_none(),
            "native queue fully drained")?;
    events.post(Event { kind: EventKind::KeyDown,
                        message: 1,
                        ..event }.into())?;
    events.post(Event { kind: EventKind::MouseDown,
                        message: 2,
                        ..event }.into())?;
    events.post(Event { kind: EventKind::KeyDown,
                        message: 3,
                        ..event }.into())?;
    events.flush(mac_toolbox::events::EventMask::from_bits_retain(EVERY_EVENT),
                 mac_toolbox::events::EventMask::from_bits_retain(2))?;
    require(events.next(mac_toolbox::events::EventMask::from_bits_retain(EVERY_EVENT))?
                  .into_record()
                  .message
            == 2,
            "native flush keeps stopping event")?;
    require(events.next(mac_toolbox::events::EventMask::from_bits_retain(EVERY_EVENT))?
                  .into_record()
                  .message
            == 3,
            "native flush keeps trailing event")?;
    events.flush(mac_toolbox::events::EventMask::from_bits_retain(EVERY_EVENT),
                 mac_toolbox::events::EventMask::from_bits_retain(0))?;
    events.os_flush(EVERY_EVENT, 0)?;
    Ok(())
}

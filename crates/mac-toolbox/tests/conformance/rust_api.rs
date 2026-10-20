//! Native vocabulary acceptance through public SDK calls and real services.
use mac_toolbox::{Error, Event, EventMask, Modifiers, System,
                  events::{Input, Key}};
#[cfg(target_arch = "riscv64")]
use mac_toolbox::{Rect, windows::WindowSpec};

use super::{Failure, require};

pub fn events_and_errors(system: &System<'_>) -> Result<(), Failure>
{
    let id = mac_toolbox::WindowId(0x1234);
    let events = system.events();
    events.flush(EventMask::ALL, EventMask::NONE)?;
    let input = Input { at:        mac_toolbox::Point { h: -17,
                                                        v: 37 },
                        when:      19,
                        modifiers: Modifiers::SHIFT | Modifiers::COMMAND };
    let key = Key::new('é', 0x73)?;
    let posted = [Event::KeyDown { key,
                                   repeat: false,
                                   input },
                  Event::KeyDown { key,
                                   repeat: true,
                                   input },
                  Event::KeyUp { key,
                                 input },
                  Event::MouseDown { window: Some(id),
                                     input },
                  Event::MouseUp { window: None,
                                   input },
                  Event::Update { window: id,
                                  input },
                  Event::Activate { window: id,
                                    active: true,
                                    input:  Input { modifiers: input.modifiers
                                                               | Modifiers::ACTIVE,
                                                    ..input } },
                  Event::MenuCommand { menu: -17,
                                       item: 3,
                                       input },
                  Event::CloseRequested { window: id,
                                          input },
                  Event::Idle { input,
                                tag: 0x6e75_6c6c }];
    for event in posted
    {
        events.post(event)?;
        require(events.peek(EventMask::ALL)? == Some(event), "typed peek retains event payload")?;
        require(events.next(EventMask::ALL)? == event, "typed poll decodes event payload")?;
        require(events.peek(EventMask::ALL)?.is_none(), "typed poll consumes once")?;
        if system.capabilities() & mac_toolbox::CAP_EVENT_WAIT != 0
        {
            events.post(event)?;
            require(events.wait_event(EventMask::ALL, 0)? == event, "typed wait agrees with poll")?;
        }
    }
    require(events.next(EventMask::ALL)?.is_idle(), "empty poll is Idle")?;
    if system.capabilities() & mac_toolbox::CAP_EVENT_WAIT != 0
    {
        require(events.wait_event(EventMask::ALL, 0)?.is_idle(), "empty wait is Idle")?;
    }
    else
    {
        require(events.wait_event(EventMask::ALL, 0) == Err(Error::NotSupported),
                "unsupported wait remains a typed error")?;
    }
    require(key.character() == 'é' && key.code() == 0x73, "decoded key character and code")?;
    require(Key::new('🦀', 0).err() == Some(Error::BadArgument),
            "synthetic keys reject unrepresentable scalars")?;
    require(input.modifiers.contains(Modifiers::SHIFT)
            && !input.modifiers.contains(Modifiers::CONTROL),
            "named modifier queries")?;
    require(input.modifiers.intersects(Modifiers::SHIFT | Modifiers::CONTROL),
            "modifier intersections")?;
    #[cfg(target_arch = "riscv64")]
    {
        let mut window = system.windows().create_owned(WindowSpec::new(Rect::new(100, 100, 280, 220), "Typed API").procedure(4))?;
        let id = window.id();
        window.close()?;
        require(system.windows().info(id).err() == Some(Error::NotFound),
                "facade preserves typed stale-window error")?;
    }
    let empty = system.memory().new_empty_handle()?;
    require(empty.len() == Err(Error::NilHandle), "typed Memory Manager error")?;
    empty.dispose()?;
    events.flush(EventMask::ALL, EventMask::NONE)?;
    system.log("RUST_API_USERSPACE_PASS typed_events=10 typed_errors=passed\n")?;
    Ok(())
}

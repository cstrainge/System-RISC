//! Deliberate application panics exercise the shared, resident crash viewer.
#![no_std]
#![no_main]

use mac_toolbox::{quickdraw::fonts::FontSelection, *};


const PANIC_NOW: u32 = u32::from_be_bytes(*b"pnow");
const LONG_PANIC: u32 = u32::from_be_bytes(*b"plng");
const QUIT: u32 = u32::from_be_bytes(*b"quit");
const BUTTONS: [Rect; 3] =
    [Rect::new(20, 155, 145, 185), Rect::new(167, 155, 292, 185), Rect::new(314, 155, 439, 185)];
const FONT: FontSelection = FontSelection { family: 0,
                                            size:   12,
                                            face:   0 };

fn label<'a>(system: &System<'_>, index: usize, buffer: &'a mut [u8]) -> Result<&'a str, Error>
{
    system.resources().application().indexed_string(2000, index as u32, buffer)
}

fn layout(system: &System<'_>) -> Result<(Rect, i16), Error>
{
    let resource = system.resources()
                         .application()
                         .get(resources::ResourceKey { kind: mac_toolbox::TypeCode::new(*b"WIND"),
                                                       id:   2003 })?;
    let mut bytes = [0; 20];
    let read = if resource.size() == bytes.len()
    {
        resource.read(0, &mut bytes)
    }
    else
    {
        Err(Error::BadArgument)
    };
    resource.release()?;
    if read? != bytes.len()
    {
        return Err(Error::BadArgument);
    }
    let word = |at| i16::from_be_bytes([bytes[at], bytes[at + 1]]);
    let bounds = Rect::new(word(2), word(0), word(6), word(4));
    if bounds.right.checked_sub(bounds.left) != Some(460)
       || bounds.bottom.checked_sub(bounds.top) != Some(240)
       || word(8) != 4
    {
        return Err(Error::BadArgument);
    }
    Ok((bounds, word(8)))
}

fn draw(system: &System<'_>, window: WindowId, buttons: &[controls::Control<'_>; 3])
        -> Result<(), Error>
{
    system.windows().begin_update(window)?;
    let port = system.quickdraw().graphics(window);
    port.reset()?;
    port.colors(0xffffff, 0xffffff)?;
    port.paint_rect(Rect::new(0, 0, 460, 240))?;
    port.colors(0, 0xffffff)?;
    let mut buffer = [0; 256];
    for (index, top) in [(0, 28), (4, 68), (5, 92), (6, 116)]
    {
        port.text(FONT,
                  Point { h: 18,
                          v: top },
                  label(system, index, &mut buffer)?,
                  0)?;
    }
    for button in buttons
    {
        button.control().draw()?;
    }
    system.windows().end_update(window)
}

fn command(system: &System<'_>, command: u32) -> Result<bool, Error>
{
    match command
    {
        PANIC_NOW =>
        {
            system.log("PANIC_TEST_TRIGGER kind=short\n")?;
            panic!("Panic Test deliberately requested a panic: value={}, detail={:?}; café λ",
                   42,
                   Some("formatted diagnostic"));
        }
        LONG_PANIC =>
        {
            system.log("PANIC_TEST_TRIGGER kind=long\n")?;
            panic!("Panic Test long diagnostic ({} bytes)\n{}",
                   include_str!("../assets/long-panic.txt").len(),
                   include_str!("../assets/long-panic.txt"));
        }
        QUIT => Ok(true),
        _ => Ok(false)
    }
}

fn run(system: &System<'_>) -> Result<(), Error>
{
    system.menus().clear()?;
    let _menu = system.menus().install_owned(resources::APPLICATION, 128)?;
    let mut buffer = [0; 128];
    let title = system.resources().application().string(128, &mut buffer)?;
    let (bounds, procedure) = layout(system)?;
    let mut owner =
        system.windows()
              .create_owned(windows::WindowSpec::new(bounds, title).procedure(procedure))?;
    let window = owner.id();
    system.windows().set_grow_icon_clip(window, Some(Rect::ZERO))?;
    let create_button = |index: usize| -> Result<controls::Control<'_>, Error> {
        let mut text = [0; 256];
        system.controls().create_standard(window,
                                          controls::StandardKind::Button,
                                          controls::StandardState::new(BUTTONS[index],
                                                                       label(system,
                                                                             index + 1,
                                                                             &mut text)?))
    };
    let buttons = [create_button(0)?, create_button(1)?, create_button(2)?];
    draw(system, window, &buttons)?;
    system.log("PANIC_TEST_READY\n")?;
    loop
    {
        let (event, position) =
            system.wait_positioned(mac_toolbox::events::EventMask::ALL, 30)?.into_parts();
        if let Some(selection) = system.menus().selection(&event)?
           && command(system, selection.command)?
        {
            owner.close()?;
            return Ok(());
        }
        let _bounds = match system.windows().info(window)
        {
            Ok(info) => info.bounds,
            Err(Error::NotFound) => return Ok(()),
            Err(error) => return Err(error)
        };
        match event
        {
            Event::Update { .. } if event.window().unwrap_or_default() == window =>
            {
                draw(system, window, &buttons)?
            }
            Event::MouseDown { .. } if event.window().unwrap_or_default() == window =>
            {
                let local = position.in_window(window)?;
                for (index, button) in buttons.iter().enumerate()
                {
                    let mut quit = false;
                    let handled = button.control().track_at(local, |_| {
                                                       quit = command(system,
                                                                      [PANIC_NOW, LONG_PANIC,
                                                                       QUIT][index])?;
                                                       Ok(())
                                                   })?;
                    if quit
                    {
                        owner.close()?;
                        return Ok(());
                    }
                    if handled
                    {
                        break;
                    }
                }
            }
            _ =>
            {}
        }
    }
}

mac_toolbox::application!(run);

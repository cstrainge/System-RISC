//! Deliberate application panics exercise the shared, resident crash viewer.
#![no_std]
#![no_main]

use mac_toolbox::{events::EVERY_EVENT, quickdraw::fonts::FontSelection, *};

mac_toolbox::panic_handler!();

const PANIC_NOW: u32 = u32::from_be_bytes(*b"pnow");
const LONG_PANIC: u32 = u32::from_be_bytes(*b"plng");
const QUIT: u32 = u32::from_be_bytes(*b"quit");
const BUTTONS: [Rect; 3] =
    [Rect::new(20, 155, 145, 185), Rect::new(167, 155, 292, 185), Rect::new(314, 155, 439, 185)];
const FONT: FontSelection = FontSelection { family: 0,
                                            size:   12,
                                            face:   0 };

fn label<'a>(system: &System<'_>, index: usize, buffer: &'a mut [u8]) -> Result<&'a str, Status>
{
    system.resources().application().indexed_string(2000, index as u32, buffer).map_err(|e| e.0)
}

fn layout(system: &System<'_>) -> Result<(Rect, i16), Status>
{
    let resource = system.resources()
                         .application()
                         .get(resources::ResourceKey { kind: *b"WIND",
                                                       id:   2003 })
                         .map_err(|e| e.0)?;
    let mut bytes = [0; 20];
    let read = if resource.size() == bytes.len()
    {
        resource.read(0, &mut bytes).map_err(|e| e.0)
    }
    else
    {
        Err(BAD_ARGUMENT)
    };
    resource.release().map_err(|e| e.0)?;
    if read? != bytes.len()
    {
        return Err(BAD_ARGUMENT);
    }
    let word = |at| i16::from_be_bytes([bytes[at], bytes[at + 1]]);
    let bounds = Rect::new(word(2), word(0), word(6), word(4));
    if bounds.right.checked_sub(bounds.left) != Some(460)
       || bounds.bottom.checked_sub(bounds.top) != Some(240)
       || word(8) != 4
    {
        return Err(BAD_ARGUMENT);
    }
    Ok((bounds, word(8)))
}

fn draw(system: &System<'_>, window: WindowId, buttons: &[controls::Control<'_>; 3])
        -> Result<(), Status>
{
    system.windows().begin_update(window).map_err(|e| e.0)?;
    let port = system.quickdraw().graphics(window);
    port.reset().map_err(|e| e.0)?;
    port.colors(0xffffff, 0xffffff).map_err(|e| e.0)?;
    port.paint_rect(Rect::new(0, 0, 460, 240)).map_err(|e| e.0)?;
    port.colors(0, 0xffffff).map_err(|e| e.0)?;
    let mut buffer = [0; 256];
    for (index, top) in [(0, 28), (4, 68), (5, 92), (6, 116)]
    {
        port.text(FONT,
                  Point { h: 18,
                          v: top },
                  label(system, index, &mut buffer)?,
                  0)
            .map_err(|e| e.0)?;
    }
    for button in buttons
    {
        button.control().draw().map_err(|e| e.0)?;
    }
    system.windows().end_update(window).map_err(|e| e.0)
}

fn command(system: &System<'_>, command: u32) -> Result<bool, Status>
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

fn run(system: &System<'_>) -> Result<(), Status>
{
    system.menus().clear().map_err(|e| e.0)?;
    system.menus().install_menu_resource(resources::APPLICATION, 128).map_err(|e| e.0)?;
    let mut buffer = [0; 128];
    let title = system.resources().application().string(128, &mut buffer).map_err(|e| e.0)?;
    let (bounds, procedure) = layout(system)?;
    let mut owner =
        system.windows()
              .create_owned(windows::WindowSpec::new(bounds, title).procedure(procedure))
              .map_err(|e| e.0)?;
    let window = owner.id();
    system.windows().set_grow_icon_clip(window, Some(Rect::ZERO)).map_err(|e| e.0)?;
    let create_button = |index: usize| -> Result<controls::Control<'_>, Status> {
        let mut text = [0; 256];
        system.controls()
              .create_standard(window,
                               controls::StandardKind::Button,
                               controls::StandardState::new(BUTTONS[index],
                                                            label(system, index + 1, &mut text)?))
              .map_err(|e| e.0)
    };
    let buttons = [create_button(0)?, create_button(1)?, create_button(2)?];
    draw(system, window, &buttons)?;
    system.log("PANIC_TEST_READY\n")?;
    loop
    {
        let event = system.wait_event(EVERY_EVENT, 30)?;
        if let Some(selection) = system.menus().selection(&event).map_err(|e| e.0)?
           && command(system, selection.command)?
        {
            owner.close().map_err(|e| e.0)?;
            return Ok(());
        }
        let bounds = match system.windows().info(window)
        {
            Ok(info) => info.bounds,
            Err(Error(NOT_FOUND)) => return Ok(()),
            Err(error) => return Err(error.0)
        };
        match event.kind
        {
            EventKind::Update if event.message == window.0 => draw(system, window, &buttons)?,
            EventKind::MouseDown if event.message == window.0 =>
            {
                let local = Point { h: event.where_.h.saturating_sub(bounds.left),
                                    v: event.where_.v.saturating_sub(bounds.top) };
                for (index, button) in buttons.iter().enumerate()
                {
                    let mut quit = false;
                    let handled = button.control().track(local, |_| {
                        quit = command(system, [PANIC_NOW, LONG_PANIC, QUIT][index]).map_err(Error)?;
                        Ok(())
                    }).map_err(|e| e.0)?;
                    if quit
                    {
                        owner.close().map_err(|e| e.0)?;
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

/// # Safety
/// The native loader supplies initialized segments and a live service table.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> Status
{
    if let Err(status) = unsafe { mac_toolbox::panic::install(services) }
    {
        return status;
    }
    let system = match unsafe { System::from_raw(services) }
    {
        Ok(system) => system,
        Err(status) => return status
    };
    match run(&system)
    {
        Ok(()) => OK,
        Err(status) =>
        {
            let _ = system.log("PANIC_TEST_ERROR\n");
            status
        }
    }
}

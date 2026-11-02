//! Standalone QuickDraw exerciser; all drawing is provided by System services.
#![no_std]
#![no_main]


use mac_toolbox::*;

mod demo;
mac_toolbox::include_resource_ids!();

use crate::resources::commands::{CLOSE, QUIT};

const PAGE_COMMANDS: [u32; 10] = [resources::commands::P000,
                                  resources::commands::P001,
                                  resources::commands::P002,
                                  resources::commands::P003,
                                  resources::commands::P004,
                                  resources::commands::P005,
                                  resources::commands::P006,
                                  resources::commands::P007,
                                  resources::commands::P008,
                                  resources::commands::P009];

fn run(system: &System<'_>) -> Result<(), Error>
{
    if system.capabilities() & CAP_GRAPHICS == 0
    {
        return Err(Error::NotSupported);
    }
    system.menus().clear()?;
    for id in [128, 129]
    {
        system.menus().install_menu_resource(mac_toolbox::ResourceScope::Application, id)?;
    }
    let window = demo::open(system)?;
    let mut selectors = [controls::ControlId(0); 10];
    for (index, id) in selectors.iter_mut().enumerate()
    {
        *id = system.controls()
                    .create_standard(window,
                                     controls::StandardKind::WindowFontButton,
                                     demo::selector(system, index)?)?
                    .into_id();
    }
    let mut page = 0;
    let mut icon_offset = 0;
    demo::draw(system, window, page, icon_offset, &selectors)?;
    system.log("QUICKDRAW_TEST_READY\n")?;
    loop
    {
        let (event, position) =
            system.wait_positioned(mac_toolbox::events::EventMask::ALL, 30)?.into_parts();
        if let Some(selection) = system.menus().selection(&event)?
        {
            if [CLOSE, QUIT].contains(&selection.command)
            {
                system.close_window(window)?;
                return Ok(());
            }
            if let Some(index) =
                PAGE_COMMANDS.iter().position(|command| *command == selection.command)
            {
                page = index as u8;
                demo::draw(system, window, page, icon_offset, &selectors)?;
            }
            continue;
        }
        match system.window(window)
        {
            Err(Error::NotFound) => return Ok(()),
            Err(status) => return Err(status),
            Ok(_) =>
            {}
        }
        let mut redraw =
            matches!(event, Event::Update { .. }) && event.window().unwrap_or_default() == window;
        if matches!(event, Event::KeyDown { .. })
           && !event.input().modifiers.contains(mac_toolbox::events::Modifiers::COMMAND)
        {
            let key = event.key().map_or('\0', |key| key.character());
            if key.is_ascii_digit()
            {
                page = if key == '0' { 9 } else { key as u8 - b'1' };
                redraw = true;
            }
            else if page == 7 && [0x7b, 0x7c].contains(&(event.key().map_or(0, |key| key.code())))
            {
                let count = system.quickdraw().icons().count()?;
                if event.key().map_or(0, |key| key.code()) == 0x7c
                {
                    icon_offset = if icon_offset + 16 < count { icon_offset + 16 } else { 0 };
                }
                else
                {
                    icon_offset = if icon_offset >= 16
                    {
                        icon_offset - 16
                    }
                    else
                    {
                        count.saturating_sub(1) / 16 * 16
                    };
                }
                redraw = true;
            }
        }
        if matches!(event, Event::MouseDown { .. }) && event.window().unwrap_or_default() == window
        {
            let local = position.in_window(window)?;
            for (index, id) in selectors.into_iter().enumerate()
            {
                if system.controls().control(id).track_at(local, |_| {
                                                     page = index as u8;
                                                     redraw = true;
                                                     Ok(())
                                                 })?
                {
                    break;
                }
            }
        }
        if redraw
        {
            demo::draw(system, window, page, icon_offset, &selectors)?;
        }
    }
}

mac_toolbox::application!(run);

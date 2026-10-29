//! Standalone QuickDraw exerciser; all drawing is provided by System services.
#![no_std]
#![no_main]


use mac_toolbox::*;

mod demo;
include_resource_ids!();

const CLOSE: u32 = u32::from_be_bytes(*b"clos");
const QUIT: u32 = u32::from_be_bytes(*b"quit");

fn run(system: &System<'_>) -> Result<(), Error>
{
    system.menus().clear()?;
    let _menus = [system.menus().install_owned(mac_toolbox::resources::APPLICATION, 128)?,
                  system.menus().install_owned(mac_toolbox::resources::APPLICATION, 129)?];
    let window = demo::open(system)?;
    let mut selectors = [controls::ControlId(0); 10];
    for (index, id) in selectors.iter_mut().enumerate()
    {
        *id = system.controls()
                    .create_standard(window,
                                     controls::StandardKind::WindowFontButton,
                                     demo::selector(system, index)?,
                                     mac_toolbox::controls::Reflow::No)?
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
            if selection.menu_id == MenuId(129) && (1..=10).contains(&selection.item)
            {
                page = (selection.item - 1) as u8;
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

/// # Safety
/// The native loader supplies initialized segments and a live service table.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> mac_toolbox::abi::Status
{
    if let Err(status) = unsafe { mac_toolbox::panic::install(services) }
    {
        return status;
    }
    let system = match unsafe { System::from_raw(services) }
    {
        Ok(system) => system,
        Err(status) => return status.status()
    };
    if system.capabilities() & CAP_GRAPHICS == 0
    {
        return mac_toolbox::abi::NOT_SUPPORTED;
    }
    match run(&system)
    {
        Ok(()) => mac_toolbox::abi::OK,
        Err(status) =>
        {
            let _ = system.log("QUICKDRAW_TEST_ERROR\n");
            status.status()
        }
    }
}

mac_toolbox::panic_handler!();

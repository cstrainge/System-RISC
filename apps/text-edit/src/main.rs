//! Text Edit: resource-based native document editing through the public Toolbox.
#![no_std]
#![no_main]


use mac_toolbox::*;

mod editor;
mod options;
mod preferences;
mod font_choices;
mod encoding;
mod documents;
mod batch;
include_resource_ids!();
mod menus;
mod ui;
use ui::{Text, text};

fn run(system: &System<'_>) -> Result<(), Error>
{
    let _menus = menus::install(system)?;
    let mut application = documents::Application::new(system)?;
    system.log("TEXT_EDIT_READY\n")?;
    loop
    {
        let (event, position) = system.wait_positioned(mac_toolbox::events::EventMask::ALL,
                                                       application.wait_ticks())?
                                      .into_parts();
        if !application.event(system, event, position)?
        {
            return Ok(());
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
            let _ = system.log("TEXT_EDIT_ERROR\n");
            status.status()
        }
    }
}

mac_toolbox::panic_handler!();

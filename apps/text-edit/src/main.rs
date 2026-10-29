//! Text Edit: resource-based native document editing through the public Toolbox.
#![no_std]
#![no_main]


use core::fmt::Write;

use mac_toolbox::*;

mod editor;
mod options;
mod preferences;
mod font_choices;
mod encoding;
mod documents;
mod batch;
mod resources
{
    use mac_toolbox as mac_abi;
    include!(concat!(env!("OUT_DIR"), "/resource_ids.rs"));
}
mod menus;
mod ui;
use ui::{Text, text};

fn diagnostic(system: &System<'_>, stage: &str, error: Error) -> Error
{
    let mut message = Text::<256>::default();
    let _ = writeln!(&mut message,
                     "TEXT_EDIT_ERROR stage={stage} status={} cause={error}",
                     error.status());
    let _ = system.log(message.as_str());
    error
}

fn run(system: &System<'_>) -> Result<(), Error>
{
    let _menus = menus::install(system).map_err(|error| diagnostic(system, "menus", error))?;
    let mut application = documents::Application::new(system).map_err(|error| {
                                                                 diagnostic(system, "documents",
                                                                            error)
                                                             })?;
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
        Err(status) => diagnostic(&system, "run", status).status()
    }
}

mac_toolbox::panic_handler!();

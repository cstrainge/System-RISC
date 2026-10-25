//! Minimal real process: public SDK image checks, then ordinary EXIT.
#![no_std]
#![no_main]
use mac_toolbox::{Error, System, SystemServices};
#[path = "../../../image_lifetimes_cases.rs"]
mod image_lifetimes;
mac_toolbox::panic_handler!();
/// # Safety
/// The loader's service table remains valid for the application's lifetime.
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
        Err(error) => return error.status()
    };
    let checked = (|| {
        let info = system.processes().current()?;
        let image = info.image.ok_or(Error::BadAbi)?;
        if image.raw() <= u64::from(u32::MAX)
        {
            return Err(Error::BadAbi);
        }
        image_lifetimes::stale(&system, mac_toolbox::resources::ImageScope::from_raw(1)?)?;
        image_lifetimes::string(&system, image, "image-probe")?;
        let window = system.windows().create_owned(mac_toolbox::windows::WindowSpec::new(
            mac_toolbox::Rect::new(80, 80, 320, 180), "Image source probe"))?;
        let text = system.controls()
                         .create_static_text(window.id(),
                                             mac_toolbox::Rect::new(8, 8, 220, 28),
                                             "Application definition source",
                                             Default::default(),
                                             mac_toolbox::controls::TextAlignment::Left)?;
        text.draw()?;
        system.log("IMAGE_PROBE_DEFINITION_PASS\n")?;
        system.log("IMAGE_PROBE_PASS\n")
    })();
    match checked
    {
        Ok(()) => 0,
        Err(error) =>
        {
            let _ = system.log("PROCESS_FAIL image lifetime probe failed\n");
            error.status()
        }
    }
}

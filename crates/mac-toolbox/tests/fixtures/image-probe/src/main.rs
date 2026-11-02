//! Minimal real process: public SDK image checks, then ordinary EXIT.
#![no_std]
#![no_main]
use mac_toolbox::{Error, System, SystemServices};
#[path = "../../../image_lifetimes_cases.rs"]
mod image_lifetimes;
mac_toolbox::panic_handler!();

fn owned_definition(system: &System<'_>) -> Result<(), Error>
{
    for _ in 0..16
    {
        let result = (|| {
            let snapshot = system.inspection()
                                 .snapshot(mac_toolbox::inspection::Scope::Own)
                                 .map_err(|failure| failure.cause)?;
            if snapshot.count != 1
            {
                return Err(Error::BadAbi);
            }
            let ledger = snapshot.ledger(0).map_err(|failure| failure.cause)?;
            for index in 0..ledger.len()
            {
                let pool = ledger.at(index).map_err(|failure| failure.cause)?;
                if pool.kind == *b"DDEF" && pool.live > 0
                {
                    return Ok(());
                }
            }
            Err(Error::BadAbi)
        })();
        match result
        {
            Err(Error::ScanStale | Error::ServiceBusy) =>
            {
                match system.client().wait_event(0, 1)
                {
                    Ok(_) | Err(Error::ServiceBusy) => (),
                    Err(error) => return Err(error)
                }
            }
            other => return other
        }
    }
    Err(Error::ServiceBusy)
}
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
    let checked = (|| -> Result<(), Error> {
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
        owned_definition(&system)?;
        system.log("IMAGE_PROBE_OWNED_DDEF_PASS\n")?;
        system.log("IMAGE_PROBE_DEFINITION_PASS\n")?;
        #[cfg(feature = "abort-live-definition")]
        {
            system.log("IMAGE_PROBE_DEFINITION_ABORT_READY\n")?;
            panic!("Image Probe: abort while an application-owned definition control is live");
        }
        #[cfg(not(feature = "abort-live-definition"))]
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

//! Minimal real process: public SDK image checks, then ordinary EXIT.
#![no_std]
#![no_main]
extern crate alloc;

use mac_toolbox::{Error, System};
mod isolation;
mod offscreen_controls;
mod window_definition;
#[path = "../../../image_lifetimes_cases.rs"]
mod image_lifetimes;

fn witness(witness: &mut mac_toolbox::controls::StaticText<'_>, selector: &str, entries: i16,
           effects: i16)
           -> Result<(), Error>
{
    use mac_toolbox::controls::{Extent, Invalidation, Requirements, Size};

    witness.set(selector)?;
    let control = witness.control();
    // Counter changes are author state, not a resident content revision. Force
    // a genuine author measurement instead of accepting a cached old answer.
    control.invalidate_layout(Invalidation::Measurement)?;
    let actual = control.ask_size(Default::default())?;
    let expected = Size::new(Extent::new(entries).map_err(|_| Error::BadAbi)?,
                             Extent::new(effects).map_err(|_| Error::BadAbi)?);
    let expected = Requirements::new(expected, expected);
    if actual.intrinsic != expected || actual.constrained != expected
    {
        return Err(Error::BadAbi);
    }
    Ok(())
}

fn stale_control(system: &System<'_>, id: mac_toolbox::controls::ControlId) -> Result<(), Error>
{
    match system.controls().control(id).info()
    {
        Err(Error::NotFound) => Ok(()),
        _ => Err(Error::BadAbi)
    }
}

fn owned_definition(system: &System<'_>, minimum: u64) -> Result<(), Error>
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
                if pool.kind == *b"DDEF" && pool.live >= minimum
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
mac_toolbox::application!(run);

fn run(system: &System<'_>) -> Result<(), Error>
{
    if isolation::run(system)?
    {
        return Ok(());
    }
    let mut stage = "current-process";
    let checked = (|| -> Result<(), Error> {
        let info = system.processes().current()?;
        let image = info.image.ok_or(Error::BadAbi)?;
        if image.raw() <= u64::from(u32::MAX)
        {
            return Err(Error::BadAbi);
        }
        stage = "image-source-checks";
        image_lifetimes::stale(system, mac_toolbox::resources::ImageScope::from_raw(1)?)?;
        image_lifetimes::string(system, image, "image-probe")?;
        stage = "window-create";
        let window = system.windows().create_owned(mac_toolbox::windows::WindowSpec::new(
            mac_toolbox::Rect::new(80, 80, 320, 180), "Image source probe"))?;
        stage = "witness-create";
        let mut counts = system.controls().create_static_text_with(
            window.id(), mac_toolbox::controls::StaticTextSpec {
                bounds: mac_toolbox::Rect::new(8, 80, 220, 100),
                text: "dispose-probe/witness-first",
                font: Default::default(),
                alignment: mac_toolbox::controls::TextAlignment::Left,
                layout: mac_toolbox::controls::TextLayout::Ellipsis,
                reflow: mac_toolbox::controls::Reflow::Yes
            })?;
        stage = "witness-query-initial";
        witness(&mut counts, "dispose-probe/witness-first", 0, 0)?;
        witness(&mut counts, "dispose-probe/witness-second", 0, 0)?;
        witness(&mut counts, "dispose-probe/witness-duplicates", 0, 0)?;
        stage = "first-control-create";
        let text = system.controls()
                         .create_static_text(window.id(),
                                             mac_toolbox::Rect::new(8, 8, 220, 28),
                                             "dispose-probe/first",
                                             Default::default(),
                                             mac_toolbox::controls::TextAlignment::Left)?;
        stage = "first-control-draw";
        text.draw()?;
        stage = "own-definition-ledger";
        owned_definition(system, 1)?;
        system.log("IMAGE_PROBE_OWNED_DDEF_PASS\n")?;
        // Transfer lifetime to the Window, then inspect the explicit Result.
        // Keeping the owned wrapper would implicitly retry in Drop on error.
        let original = text.into_control().into_id();
        stage = "first-control-dispose";
        system.controls().control(original).dispose()?;
        stale_control(system, original)?;
        stage = "witness-query-disposed";
        witness(&mut counts, "dispose-probe/witness-first", 1, 1)?;
        witness(&mut counts, "dispose-probe/witness-duplicates", 0, 0)?;
        system.log("IMAGE_PROBE_CDEF_DISPOSE_CHECKED\n")?;
        stage = "replacement-create";
        let text = system.controls()
                         .create_static_text(window.id(),
                                             mac_toolbox::Rect::new(8, 40, 220, 60),
                                             "dispose-probe/second",
                                             Default::default(),
                                             mac_toolbox::controls::TextAlignment::Left)?;
        text.draw()?;
        if text.id() == original
        {
            return Err(Error::BadAbi);
        }
        stale_control(system, original)?;
        text.control().info()?;
        witness(&mut counts, "dispose-probe/witness-first", 1, 1)?;
        witness(&mut counts, "dispose-probe/witness-second", 0, 0)?;
        witness(&mut counts, "dispose-probe/witness-duplicates", 0, 0)?;
        system.log("IMAGE_PROBE_CDEF_RECREATE_PASS\n")?;
        system.log("IMAGE_PROBE_DEFINITION_PASS\n")?;
        #[cfg(feature = "abort-live-definition")]
        {
            system.log("IMAGE_PROBE_DEFINITION_ABORT_READY\n")?;
            panic!("Image Probe: abort while an application-owned definition control is live");
        }
        #[cfg(not(feature = "abort-live-definition"))]
        {
            let replacement = text.into_control().into_id();
            system.controls().control(replacement).dispose()?;
            stale_control(system, original)?;
            stale_control(system, replacement)?;
            witness(&mut counts, "dispose-probe/witness-first", 1, 1)?;
            witness(&mut counts, "dispose-probe/witness-second", 1, 1)?;
            witness(&mut counts, "dispose-probe/witness-duplicates", 0, 0)?;
            system.log("IMAGE_PROBE_CDEF_COUNTS_PASS first=1/1 second=1/1 duplicates=0\n")?;
            let witness_id = counts.into_control().into_id();
            system.controls().control(witness_id).dispose()?;
            stale_control(system, witness_id)?;
            system.log("IMAGE_PROBE_PASS\n")
        }
    })();
    match checked
    {
        Ok(()) => Ok(()),
        Err(error) =>
        {
            isolation::log_failure(system, stage, error);
            Err(error)
        }
    }
}

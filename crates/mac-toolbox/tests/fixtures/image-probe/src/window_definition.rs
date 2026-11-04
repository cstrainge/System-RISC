//! Real application-owned WDEF workload. The packager embeds the production
//! WDEF artifact in this APPL; nothing is imported into resident System code.
use mac_toolbox::{Error, Rect, System,
                  windows::{WindowId, WindowSpec}};

fn absent(system: &System<'_>, window: WindowId) -> Result<(), Error>
{
    match system.windows().info(window)
    {
        Err(Error::NotFound) => Ok(()),
        _ => Err(Error::BadAbi)
    }
}

pub(super) fn run(system: &System<'_>, mode: &str) -> Result<(), Error>
{
    let mut stage = "wdef-current-image";
    let checked = (|| {
        let image = system.processes().current()?.image.ok_or(Error::BadAbi)?;
        stage = "wdef-resource";
        let resource =
            system
                .resources()
                .image(image)
                .get(mac_toolbox::resources::ResourceKey {
                    kind: mac_toolbox::TypeCode::new(*b"WDEF"),
                    id: 0,
                })?;
        if resource.size() == 0
        {
            return Err(Error::BadAbi);
        }
        drop(resource);
        let windows = system.windows();
        let bounds = Rect::new(100, 100, 460, 300);
        system.log("ISOLATION_WDEF_CREATE_BEGIN\n")?;
        stage = "wdef-create";
        let window = windows.create_owned(WindowSpec::new(bounds, "Owned WDEF").minimum(120, 80)
                                                                               .procedure(8))?;
        if mode == "window-definition-calculate-fault"
        {
            system.log("ISOLATION_WDEF_CALCULATE_UNEXPECTED_RETURN\n")?;
            return Err(Error::BadAbi);
        }
        system.log("ISOLATION_WDEF_CREATE_RETURNED\n")?;
        // No CDEF or other application definition has been requested in this
        // selector. A live definition in our actual owner ledger establishes that
        // the WDEF resource was selected from this application rather than System.
        stage = "wdef-owned-definition";
        super::owned_definition(system, 1)?;
        stage = "wdef-first-info";
        let info = window.info()?;
        stage = "wdef-first-info-check";
        if info.bounds != bounds || info.title() != "Owned WDEF" || info.procedure != 8
        {
            return Err(Error::BadAbi);
        }

        let moved = Rect::new(120, 115, 500, 335);
        stage = "wdef-title";
        window.set_title("Owned WDEF geometry")?;
        stage = "wdef-bounds";
        window.set_bounds(moved)?;
        stage = "wdef-bounds-info";
        let before = window.info()?;
        stage = "wdef-bounds-check";
        if before.bounds != moved || before.title() != "Owned WDEF geometry"
        {
            return Err(Error::BadAbi);
        }
        stage = "wdef-invalid-standard";
        match window.set_standard_bounds(Some(Rect::ZERO))
        {
            Err(Error::BadArgument) => (),
            _ => return Err(Error::BadAbi)
        }
        stage = "wdef-refusal-info";
        if window.info()? != before
        {
            return Err(Error::BadAbi);
        }
        // A refused geometry request must leave the next real operation usable.
        stage = "wdef-standard";
        window.set_standard_bounds(Some(Rect::new(90, 90, 530, 370)))?;
        stage = "wdef-invalidate";
        window.invalidate(Rect::new(0, 0, 380, 220))?;
        system.log("IMAGE_PROBE_WDEF_GEOMETRY_READY\n")?;
        // Let the normal event/paint path run without a host acknowledgement or
        // a test-only barrier. The marker does not assert pixels or paint timing.
        stage = "wdef-paint-wait";
        system.client().wait_event(0, 30)?;

        if mode == "window-definition-draw-fault"
        {
            system.log("ISOLATION_WDEF_DRAW_NO_FAULT_AFTER_WAIT\n")?;
            return Err(Error::BadAbi);
        }
        let original = window.into_id();
        system.log("ISOLATION_WDEF_CLOSE_BEGIN\n")?;
        stage = "wdef-close";
        windows.close(original)?;
        if mode == "window-definition-dispose-fault"
        {
            system.log("ISOLATION_WDEF_DISPOSE_UNEXPECTED_RETURN\n")?;
            return Err(Error::BadAbi);
        }
        stage = "wdef-first-absent";
        absent(system, original)?;
        stage = "wdef-replacement";
        let replacement =
            windows.create_owned(WindowSpec::new(bounds, "Owned WDEF replacement").minimum(120,
                                                                                           80)
                                                                                  .procedure(8))?;
        stage = "wdef-replacement-id";
        if replacement.id() == original
        {
            return Err(Error::BadAbi);
        }
        stage = "wdef-stale-after-recreate";
        absent(system, original)?;
        stage = "wdef-replacement-info";
        replacement.info()?;
        let next = replacement.into_id();
        stage = "wdef-replacement-close";
        windows.close(next)?;
        stage = "wdef-final-original-absent";
        absent(system, original)?;
        stage = "wdef-final-next-absent";
        absent(system, next)?;
        stage = "wdef-pass-log";
        system.log("IMAGE_PROBE_WDEF_LIFECYCLE_PASS geometry_refusal=preserved stale=refused\n")
    })();
    if let Err(error) = checked
    {
        super::isolation::log_failure(system, stage, error);
    }
    checked
}

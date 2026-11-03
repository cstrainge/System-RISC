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
    let image = system.processes().current()?.image.ok_or(Error::BadAbi)?;
    let resource =
        system.resources()
              .image(image)
              .get(mac_toolbox::resources::ResourceKey { kind: mac_toolbox::TypeCode::new(*b"WDEF"),
                                                         id:   0 })?;
    if resource.size() == 0
    {
        return Err(Error::BadAbi);
    }
    drop(resource);
    let windows = system.windows();
    let bounds = Rect::new(100, 100, 460, 300);
    system.log("ISOLATION_WDEF_CREATE_BEGIN\n")?;
    let window =
        windows.create_owned(WindowSpec::new(bounds, "Owned WDEF").minimum(120, 80).procedure(8))?;
    if mode == "window-definition-calculate-fault"
    {
        system.log("ISOLATION_WDEF_CALCULATE_UNEXPECTED_RETURN\n")?;
        return Err(Error::BadAbi);
    }
    system.log("ISOLATION_WDEF_CREATE_RETURNED\n")?;
    // No CDEF or other application definition has been requested in this
    // selector. A live definition in our actual owner ledger establishes that
    // the WDEF resource was selected from this application rather than System.
    super::owned_definition(system, 1)?;
    let info = window.info()?;
    if info.bounds != bounds || info.title() != "Owned WDEF" || info.procedure != 8
    {
        return Err(Error::BadAbi);
    }

    let moved = Rect::new(120, 115, 500, 335);
    window.set_title("Owned WDEF geometry")?;
    window.set_bounds(moved)?;
    let before = window.info()?;
    if before.bounds != moved || before.title() != "Owned WDEF geometry"
    {
        return Err(Error::BadAbi);
    }
    match window.set_standard_bounds(Some(Rect::ZERO))
    {
        Err(Error::BadArgument) => (),
        _ => return Err(Error::BadAbi)
    }
    if window.info()? != before
    {
        return Err(Error::BadAbi);
    }
    // A refused geometry request must leave the next real operation usable.
    window.set_standard_bounds(Some(Rect::new(90, 90, 530, 370)))?;
    window.invalidate(Rect::new(0, 0, 380, 220))?;
    system.log("IMAGE_PROBE_WDEF_GEOMETRY_READY\n")?;
    // Let the normal event/paint path run without a host acknowledgement or
    // a test-only barrier. The marker does not assert pixels or paint timing.
    system.client().wait_event(0, 30)?;

    if mode == "window-definition-draw-fault"
    {
        system.log("ISOLATION_WDEF_DRAW_NO_FAULT_AFTER_WAIT\n")?;
        return Err(Error::BadAbi);
    }
    let original = window.into_id();
    system.log("ISOLATION_WDEF_CLOSE_BEGIN\n")?;
    windows.close(original)?;
    if mode == "window-definition-dispose-fault"
    {
        system.log("ISOLATION_WDEF_DISPOSE_UNEXPECTED_RETURN\n")?;
        return Err(Error::BadAbi);
    }
    absent(system, original)?;
    let replacement =
        windows.create_owned(WindowSpec::new(bounds, "Owned WDEF replacement").minimum(120, 80)
                                                                              .procedure(8))?;
    if replacement.id() == original
    {
        return Err(Error::BadAbi);
    }
    absent(system, original)?;
    replacement.info()?;
    let next = replacement.into_id();
    windows.close(next)?;
    absent(system, original)?;
    absent(system, next)?;
    system.log("IMAGE_PROBE_WDEF_LIFECYCLE_PASS geometry_refusal=preserved stale=refused\n")
}

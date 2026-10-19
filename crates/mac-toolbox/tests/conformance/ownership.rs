//! Native Rust ownership contracts through the public Toolbox and real services.
use mac_toolbox::{BAD_ARGUMENT, Error, NOT_FOUND, Rect, System, ToolboxResult, WindowId,
                  windows::{Window, WindowSpec}};

use super::Failure;

fn create<'a>(system: &System<'a>) -> ToolboxResult<Window<'a>>
{
    system.windows()
          .create_owned(WindowSpec::new(Rect::new(100, 100, 340, 240), "Owned window").procedure(4))
}

fn fail_after_creation(system: &System<'_>, identity: &mut WindowId) -> ToolboxResult<()>
{
    let window = create(system)?;
    *identity = window.id();
    let handle = system.memory().new_handle_clear(256 * 1024)?;
    // A checked service failure propagates through ?, dropping both owners.
    handle.write(256 * 1024, b"outside")?;
    window.set_title("unreachable")
}

/// Runs only in the graphical profile, where the resident Window Manager and
/// real WDEF are available. Memory ownership also has host acceptance coverage.
pub fn graphical(system: &System<'_>) -> Result<(), Failure>
{
    for _ in 0..40
    {
        let identity;
        {
            let window = create(system)?;
            identity = window.id();
            assert_ne!(identity, WindowId(0));
            window.set_title("Automatic cleanup")?;
            window.select()?;
            window.invalidate(Rect::new(0, 0, 240, 140))?;
            assert_eq!(window.info()?.id, identity.0);
            // Moving the Rust owner transfers the single destruction duty.
            let moved = window;
            assert_eq!(moved.id(), identity);
        }
        assert!(matches!(system.windows().info(identity), Err(Error(NOT_FOUND))));
    }
    for _ in 0..16
    {
        let mut identity = WindowId(0);
        assert_eq!(fail_after_creation(system, &mut identity), Err(Error(BAD_ARGUMENT)));
        assert_ne!(identity, WindowId(0));
        assert!(matches!(system.windows().info(identity), Err(Error(NOT_FOUND))));
    }
    // More than the available arena has passed through failed calls. This full
    // allocation succeeds only if those temporary handle owners freed storage.
    let handle = system.memory().new_handle_clear(1024 * 1024 - 4096)?;
    drop(handle);

    let mut window = create(system)?;
    let identity = window.id();
    window.set_close_deferred(true)?;
    window.close()?;
    window.close()?;
    assert_eq!(window.id(), identity, "a closed owner must never become front-window ID0");
    assert!(matches!(window.info(), Err(Error(NOT_FOUND))));
    let replacement = create(system)?;
    drop(window);
    assert_eq!(replacement.info()?.id, replacement.id().0);
    drop(replacement);

    let mut already_closed = create(system)?;
    let identity = already_closed.id();
    system.windows().close(identity)?;
    let replacement = create(system)?;
    assert_ne!(replacement.id(), identity);
    already_closed.close()?;
    drop(already_closed);
    assert_eq!(replacement.info()?.id, replacement.id().0);
    drop(replacement);

    let identity = create(system)?.into_id();
    assert_eq!(system.windows().info(identity)?.id, identity.0);
    system.windows().close(identity)?;
    system.log("OWNERSHIP_USERSPACE_PASS scope=40 early_returns=16 stale=passed transfer=passed\n")
          .map_err(Failure::Service)
}

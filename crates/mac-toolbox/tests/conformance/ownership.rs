//! Native Rust ownership contracts through the public Toolbox and real services.
use mac_toolbox::{Error, Rect, System, ToolboxResult, WindowId,
                  abi::NOT_FOUND,
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

fn rejected_window(system: &System<'_>, id: WindowId, gate: mac_abi::TrapGate)
                   -> Result<(), Failure>
{
    let windows = system.windows();
    assert!(matches!(windows.info(id), Err(Error::NotFound)));
    for result in [windows.close(id),
                   windows.select(id),
                   windows.set_title(id, "stale"),
                   windows.invalidate(id, Rect::new(0, 0, 10, 10)),
                   windows.validate(id),
                   windows.set_standard_bounds(id, Some(Rect::new(80, 80, 300, 220))),
                   windows.set_close_deferred(id, true),
                   windows.end_update(id)]
    {
        assert_eq!(result, Err(Error::NotFound));
    }
    assert_eq!(windows.begin_update(id), Err(Error::NotFound));
    assert_eq!(system.quickdraw().graphics(id).paint_rect(Rect::new(0, 0, 10, 10)),
               Err(Error::NotFound));
    assert_eq!(system.quickdraw().graphics(id).content_outline(Some(Rect::new(0, 0, 10, 10))),
               Err(Error::NotFound));
    assert!(matches!(system.quickdraw()
                           .graphics(id)
                           .masked_rectangles(&[Rect::new(0, 0, 10, 10)]),
                     Err(Error::NotFound)));
    for operation in [mac_abi::GET_WINDOW, mac_abi::SET_WINDOW_TITLE, mac_abi::CLOSE_WINDOW]
    {
        let mut request = mac_abi::traps::WindowCall { operation,
            request: mac_abi::WindowRequest { id: id.0, bounds: Rect::ZERO,
                title: [0; mac_abi::WINDOW_TITLE_BYTES], title_length: 1, procedure: 0,
                minimum_width: 1, minimum_height: 1, reserved: 0 } };
        // The legacy Pascal trap borrows this initialized request synchronously.
        assert_eq!(unsafe {
                       gate(mac_abi::traps::PSTRING_WINDOW,
                            core::ptr::from_mut(&mut request).cast(),
                            core::mem::size_of_val(&request))
                   },
                   NOT_FOUND as i64);
    }
    Ok(())
}

/// Runs only in the graphical profile, where the resident Window Manager and
/// real WDEF are available. Memory ownership also has host acceptance coverage.
pub fn graphical(system: &System<'_>, gate: mac_abi::TrapGate) -> Result<(), Failure>
{
    let mut memory = mac_abi::memory::MemoryCall { operation: mac_abi::memory::NEW_CLEAR,
                                                   size: 4,
                                                   ..Default::default() };
    system.client().memory(&mut memory, &[], &mut [])?;
    let memory_id = memory.handle;
    let window = create(system)?;
    assert_ne!(memory_id & 0xffff,
               window.id().0 & 0xffff,
               "memory and windows must have disjoint registered slots");
    rejected_window(system, WindowId(memory_id), gate)?;
    memory.operation = mac_abi::memory::DISPOSE;
    memory.handle = window.id().0;
    assert_eq!(system.client().memory(&mut memory, &[], &mut []), Err(Error::NilHandle));
    assert_eq!(window.info()?.id, window.id());
    memory.operation = mac_abi::memory::SIZE;
    memory.handle = memory_id;
    system.client().memory(&mut memory, &[], &mut [])?;
    assert_eq!(memory.size, 4, "a foreign window ID must not dispose memory");
    memory.operation = mac_abi::memory::DISPOSE;
    system.client().memory(&mut memory, &[], &mut [])?;
    drop(window);
    let mut previous: Option<WindowId> = None;
    for _ in 0..40
    {
        let identity;
        {
            let window = create(system)?;
            identity = window.id();
            assert_ne!(identity, WindowId(0));
            assert_ne!(identity.0 >> 16, 0);
            assert_eq!(identity.0 >> 24, 0, "close-event identity must fit24 bits");
            if let Some(stale) = previous
            {
                assert_eq!(identity.0 & 0xffff, stale.0 & 0xffff, "force reuse");
                assert_ne!(identity, stale);
                rejected_window(system, stale, gate)?;
            }
            rejected_window(system, WindowId(identity.0 & 0xffff), gate)?;
            rejected_window(system, WindowId(u32::MAX), gate)?;
            window.set_title("Automatic cleanup")?;
            window.select()?;
            window.invalidate(Rect::new(0, 0, 240, 140))?;
            assert_eq!(window.info()?.id, identity);
            // Moving the Rust owner transfers the single destruction duty.
            let moved = window;
            assert_eq!(moved.id(), identity);
        }
        rejected_window(system, identity, gate)?;
        previous = Some(identity);
    }
    for _ in 0..16
    {
        let mut identity = WindowId(0);
        assert_eq!(fail_after_creation(system, &mut identity), Err(Error::BadArgument));
        assert_ne!(identity, WindowId(0));
        assert!(matches!(system.windows().info(identity), Err(Error::NotFound)));
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
    assert!(matches!(window.info(), Err(Error::NotFound)));
    let replacement = create(system)?;
    drop(window);
    assert_eq!(replacement.info()?.id, replacement.id());
    drop(replacement);

    let mut already_closed = create(system)?;
    let identity = already_closed.id();
    system.windows().close(identity)?;
    let replacement = create(system)?;
    assert_ne!(replacement.id(), identity);
    assert_eq!(replacement.id().0 & 0xffff, identity.0 & 0xffff);
    rejected_window(system, identity, gate)?;
    already_closed.close()?;
    drop(already_closed);
    assert_eq!(replacement.info()?.id, replacement.id());
    drop(replacement);

    let identity = create(system)?.into_id();
    assert_eq!(system.windows().info(identity)?.id, identity);
    system.windows().close(identity)?;
    // Reach the native24-bit event ceiling through normal SDK allocation.
    // A retired generation must move to a different slot, never wrap to an old ID.
    let exhausted = create(system)?.into_id();
    system.windows().close(exhausted)?;
    let mut retired = false;
    for _ in 0..255
    {
        let replacement = create(system)?;
        assert_ne!(replacement.id(), exhausted);
        assert!(matches!(system.windows().info(exhausted), Err(Error::NotFound)));
        assert_eq!(replacement.id().0 >> 24, 0);
        if replacement.id().0 & 0xffff != exhausted.0 & 0xffff
        {
            retired = true;
            rejected_window(system, exhausted, gate)?;
            break;
        }
    }
    assert!(retired, "window generations must retire rather than wrap into event flags");
    editors(system, Some(gate))?;
    system.log("OWNERSHIP_MENU_BEGIN\n")?;
    menus(system, 257)?;
    system.log("OWNERSHIP_ALERT_BEGIN\n")?;
    alerts(system)?;
    scroll_boxes(system, gate)?;
    managed_controls(system, gate)?;
    system.log("HANDLE_REUSE_USERSPACE_PASS windows=40 native=9 legacy=3 retirement=passed \
                foreign=passed\n")
          .map_err(|error| Failure::Service(error.status()))?;
    system.log("OWNERSHIP_USERSPACE_PASS scope=40 early_returns=16 stale=passed transfer=passed\n")
          .map_err(|error| Failure::Service(error.status()))
}

fn managed_controls(system: &System<'_>, gate: mac_abi::TrapGate) -> Result<(), Failure>
{
    use mac_toolbox::controls::{StandardKind, StandardState};
    let window = create(system)?;
    super::managed_controls::tracking(system, window.id())?;
    super::managed_controls::reuse(system, window.id(), gate)?;
    let state = StandardState::new(Rect::new(8, 8, 128, 32), "Parent cleanup");
    let owned = system.controls().create_standard(window.id(), StandardKind::Button, state)?;
    let stale = owned.id();
    drop(window);
    assert!(matches!(owned.control().info(), Err(Error::NotFound)));
    let replacement_window = create(system)?;
    let replacement =
        system.controls().create_standard(replacement_window.id(), StandardKind::Button, state)?;
    assert_eq!(replacement.id().0 & 0xffff, stale.0 & 0xffff);
    assert_ne!(replacement.id(), stale);
    drop(owned);
    assert!(replacement.control().info().is_ok());
    system.log("MANAGED_CONTROL_OWNERSHIP_PASS parent_cleanup=passed reuse=passed legacy=passed\n")
          .map_err(|error| Failure::Service(error.status()))
}

fn scroll_boxes(system: &System<'_>, gate: mac_abi::TrapGate) -> Result<(), Failure>
{
    use mac_toolbox::controls::{ScrollBoxId, ScrollBoxSpec};
    for _ in 0..8
    {
        let window = create(system)?;
        let spec = ScrollBoxSpec { viewport: Rect::new(8, 8, 192, 72),
                                   scrollbar: Rect::new(192, 8, 208, 72),
                                   count: 100000,
                                   ..Default::default() };
        let control = system.controls().create_scroll_box(window.id(), spec)?;
        let id = control.id();
        assert_ne!(id.0 & 0xffff, window.id().0 & 0xffff);
        let view = control.control().info()?;
        for row in view.missing
        {
            control.control().set_row(row, "Retained viewport", false)?;
        }
        system.windows().begin_update(window.id())?;
        control.control().draw()?;
        system.windows().end_update(window.id())?;
        let view = control.control().scroll_to(1)?;
        assert_eq!(view.missing, 4..5);
        control.control().set_row(4, "Exposed row only", true)?;
        control.control().draw()?;
        let mut pixels = [0; 240 * 100];
        control.control().draw_pixels(&mut pixels, 240, 100)?;
        drop(window);
        assert!(matches!(control.control().info(), Err(Error::NotFound)));
        let replacement_window = create(system)?;
        let replacement = system.controls().create_scroll_box(replacement_window.id(), spec)?;
        assert_eq!(replacement.id().0 & 0xffff, id.0 & 0xffff);
        assert_ne!(replacement.id(), id);
        drop(control);
        assert!(replacement.control().info().is_ok());
        for foreign in [id, ScrollBoxId(replacement_window.id().0)]
        {
            assert!(matches!(system.controls().scroll_box(foreign).info(), Err(Error::NotFound)));
            let mut call = mac_abi::scrollbox::Call { id: foreign.0,
                                                      ..Default::default() };
            assert_eq!(unsafe {
                           gate(mac_abi::traps::PSTRING_SCROLL_BOX,
                                core::ptr::from_mut(&mut call).cast(),
                                core::mem::size_of_val(&call))
                       },
                       NOT_FOUND as i64);
        }
    }
    system.log("SCROLL_BOX_USERSPACE_PASS parent_cleanup=8 stale=passed legacy=passed \
                rows=passed\n")
          .map_err(|error| Failure::Service(error.status()))
}

/// The same resident TextEdit identities protect native and Pascal trap callers.
/// Runs on host and RV64 so generation safety is not just a model-unit assertion.
pub fn editors(system: &System<'_>, gate: Option<mac_abi::TrapGate>) -> Result<(), Failure>
{
    use mac_toolbox::textedit::EditorId;
    let te = system.textedit();
    let bounds = Rect::new(0, 0, 120, 60);
    let stale = te.create(bounds, bounds)?;
    te.dispose(stale)?;
    let replacement = te.create(bounds, bounds)?;
    assert_eq!(stale.0 & 0xffff, replacement.0 & 0xffff, "force TextEdit slot reuse");
    assert_ne!(stale, replacement);
    // Use another live manager's real issued ID, not just an invalid integer.
    let mut raw = mac_abi::memory::MemoryCall { operation: mac_abi::memory::NEW_CLEAR,
                                                size: 4,
                                                ..Default::default() };
    system.client().memory(&mut raw, &[], &mut [])?;
    for id in [stale, EditorId(raw.handle)]
    {
        assert_eq!(te.info(id), Err(Error::NotFound));
        assert_eq!(te.set_text(id, "must not replace"), Err(Error::NotFound));
        assert_eq!(te.dispose(id), Err(Error::NotFound));
        let empty_pascal = [0u8];
        let mut call = mac_abi::textedit::TextEditCall { operation: mac_abi::textedit::SET_TEXT,
                                                         input: empty_pascal.as_ptr(),
                                                         input_length: 1,
                                                         id,
                                                         ..Default::default() };
        // The classic adapter reaches the same resident lifetime check.
        if let Some(gate) = gate
        {
            let status = unsafe {
                gate(mac_abi::traps::PSTRING_TEXTEDIT,
                     core::ptr::from_mut(&mut call).cast(),
                     core::mem::size_of_val(&call))
            };
            assert_eq!(status, mac_abi::NOT_FOUND as i64);
        }
    }
    assert_eq!(te.info(replacement)?.text_length, 0);
    te.dispose(replacement)?;
    raw.operation = mac_abi::memory::DISPOSE;
    system.client().memory(&mut raw, &[], &mut [])?;
    Ok(())
}

/// Resource menu numbers remain semantic properties; references carry lifetimes.
pub fn menus(system: &System<'_>, resource: i16) -> Result<(), Failure>
{
    use mac_toolbox::{ResourceScope, menus::MenuHandle};
    let api = system.menus();
    api.clear()?;
    let owned = api.install_owned(ResourceScope::Application, resource)?;
    let stale = owned.menu();
    let id = owned.id()?;
    assert_eq!(api.find(id)?.handle(), owned.handle());
    drop(owned);
    let mut replacement = api.install_owned(ResourceScope::Application, resource)?;
    assert_eq!(stale.handle().0 & 0xffff, replacement.handle().0 & 0xffff);
    assert_ne!(stale.handle(), replacement.handle());
    for result in [stale.layout().map(|_| ()),
                   stale.item_count().map(|_| ()),
                   stale.set_item_enabled(0, false),
                   stale.dispose()]
    {
        assert_eq!(result, Err(Error::NotFound));
    }
    let mut memory = mac_abi::memory::MemoryCall { operation: mac_abi::memory::NEW_CLEAR,
                                                   size: 4,
                                                   ..Default::default() };
    system.client().memory(&mut memory, &[], &mut [])?;
    assert_eq!(api.menu(MenuHandle(memory.handle)).layout(), Err(Error::NotFound));
    memory.operation = mac_abi::memory::DISPOSE;
    system.client().memory(&mut memory, &[], &mut [])?;
    assert_eq!(replacement.id()?, id);
    assert!(replacement.item_count()? > 0);
    api.clear()?;
    let current = api.install_owned(ResourceScope::Application, resource)?;
    assert_ne!(current.handle(), replacement.handle());
    replacement.dispose()?;
    drop(replacement);
    assert!(current.layout().is_ok(), "dropping a cleared menu must not dispose its replacement");
    drop(current);
    assert!(api.find(id).is_err());
    Ok(())
}

fn alerts(system: &System<'_>) -> Result<(), Failure>
{
    let dialogs = system.dialogs();
    let owned = dialogs.alert_owned(mac_toolbox::ResourceScope::Application, 6012, "Owned alert")?;
    let stale = owned.alert();
    let window = owned.window();
    drop(owned);
    assert!(matches!(system.windows().info(window), Err(Error::NotFound)));
    let current =
        dialogs.alert_owned(mac_toolbox::ResourceScope::Application, 6012, "Replacement alert")?;
    assert_eq!(window.0 & 0xffff, current.window().0 & 0xffff);
    assert_ne!(window, current.window());
    assert_eq!(stale.draw(), Err(Error::NotFound));
    assert_eq!(stale.close(), Err(Error::NotFound));
    assert!(system.windows().info(current.window()).is_ok());
    drop(current);
    Ok(())
}

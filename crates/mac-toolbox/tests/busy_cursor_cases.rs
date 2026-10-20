//! Real userspace work deliberately stops pumping events, with and without traps.
#![cfg(target_arch = "riscv64")]
use mac_toolbox::{events::EVERY_EVENT, quickdraw::cursors::StandardCursor, *};

fn checkpoint(system: &System<'_>, window: u32, marker: &str) -> Result<(), i32>
{
    system.log(marker)?;
    loop
    {
        let event = system.events().wait_event(EVERY_EVENT, 600).map_err(|e| e.0)?;
        if event.kind == EventKind::KeyDown
        {
            return Ok(());
        }
        if event.kind == EventKind::Update && event.message == window
        {
            system.begin_update(window)?;
            system.quickdraw()
                  .port(WindowId(window))
                  .paint_rect(Rect::new(0, 0, 400, 300), 0xffffff)
                  .map_err(|e| e.0)?;
            system.end_update(window)?;
        }
    }
}

pub fn run(system: &System<'_>, clock: impl Fn() -> u64) -> Result<(), i32>
{
    system.log("DESKTOP_READY: busy cursor userspace regression\n")?;
    let mine = system.processes().current().map_err(|e| e.0)?;
    let window = system.new_window(Rect::new(100, 100, 500, 400), "Busy Cursor", (100, 60), 0)?;
    system.select_window(window)?;
    system.begin_update(window)?;
    system.quickdraw()
          .port(WindowId(window))
          .paint_rect(Rect::new(0, 0, 400, 300), 0xffffff)
          .map_err(|e| e.0)?;
    system.end_update(window)?;
    let cursor = system.quickdraw().cursors();
    cursor.set_standard(StandardCursor::Crosshair).map_err(|e| e.0)?;
    checkpoint(system, window, "BUSY_STAGE idle\n")?;
    assert_eq!(system.processes().count().map_err(|e| e.0)?,
               1,
               "CPU-only phase must have no cooperating background presenter");
    system.log("BUSY_STAGE spin\n")?;
    let start = clock();
    while clock().wrapping_sub(start) < 25_000_000
    {
        core::hint::spin_loop();
    }
    assert_eq!(cursor.visibility().map_err(|e| e.0)?, 0);
    system.events().wait_event(0, 0).map_err(|e| e.0)?;
    checkpoint(system, window, "BUSY_STAGE recovered\n")?;
    system.log("BUSY_STAGE peeking\n")?;
    let start = clock();
    while clock().wrapping_sub(start) < 12_000_000
    {
        system.events().peek(0).map_err(|e| e.0)?;
    }
    checkpoint(system, window, "BUSY_STAGE peek-recovered\n")?;
    system.log("BUSY_STAGE traps\n")?;
    let start = system.events().monotonic_nanoseconds().map_err(|e| e.0)?;
    while system.events().monotonic_nanoseconds().map_err(|e| e.0)? - start < 1_500_000_000
    {}
    system.events().wait_event(0, 0).map_err(|e| e.0)?;
    checkpoint(system, window, "BUSY_STAGE trap-recovered\n")?;
    cursor.hide().map_err(|e| e.0)?;
    system.log("BUSY_STAGE hidden\n")?;
    let start = clock();
    while clock().wrapping_sub(start) < 15_000_000
    {
        core::hint::spin_loop();
    }
    assert_eq!(cursor.visibility().map_err(|e| e.0)?, -1, "System watch changed app hide depth");
    system.events().wait_event(0, 0).map_err(|e| e.0)?;
    checkpoint(system, window, "BUSY_STAGE hidden-recovered\n")?;
    cursor.show().map_err(|e| e.0)?;
    let volume = system.files().volume_at(0).map_err(|e| e.0)?.id;
    let child = system.files().lookup(volume, 2, "Applications/QuickDraw Test").map_err(|e| e.0)?;
    let child = system.processes().launch(volume, child.id).map_err(|e| e.0)?;
    assert_ne!(child.thread, mine.thread);
    for _ in 0..10
    {
        system.events().wait_event(0, 1).map_err(|e| e.0)?;
    }
    system.processes().activate(mine.id).map_err(|e| e.0)?;
    system.events().wait_event(0, 0).map_err(|e| e.0)?;
    checkpoint(system, window, "BUSY_STAGE switch-ready\n")?;
    system.log("BUSY_STAGE switch-spin\n")?;
    let start = clock();
    while clock().wrapping_sub(start) < 30_000_000
    {
        core::hint::spin_loop();
    }
    system.events().wait_event(0, 0).map_err(|e| e.0)?;
    system.log("BUSY_CURSOR_USERSPACE_PASS\n")?;
    loop
    {
        system.events().wait_event(EVERY_EVENT, 600).map_err(|e| e.0)?;
    }
}

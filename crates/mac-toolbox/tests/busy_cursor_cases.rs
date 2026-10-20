//! Real userspace work deliberately stops pumping events, with and without traps.
#![cfg(target_arch = "riscv64")]
use mac_toolbox::{events::classic::EVERY_EVENT, quickdraw::cursors::StandardCursor, *};

fn checkpoint(system: &System<'_>, window: u32, marker: &str) -> Result<(), mac_toolbox::Error>
{
    system.log(marker)?;
    loop
    {
        let event =
            system.events()
                  .wait_event(mac_toolbox::events::EventMask::from_bits_retain(EVERY_EVENT), 600)
                  .map(mac_toolbox::Event::into_record)?;
        if event.kind == EventKind::KeyDown
        {
            return Ok(());
        }
        if event.kind == EventKind::Update && event.message == window
        {
            system.begin_update(window)?;
            system.quickdraw()
                  .port(WindowId(window))
                  .paint_rect(Rect::new(0, 0, 400, 300), 0xffffff)?;
            system.end_update(window)?;
        }
    }
}

pub fn run(system: &System<'_>, clock: impl Fn() -> u64) -> Result<(), mac_toolbox::Error>
{
    system.log("DESKTOP_READY: busy cursor userspace regression\n")?;
    let mine = system.processes().current()?;
    let window = system.new_window(Rect::new(100, 100, 500, 400), "Busy Cursor", (100, 60), 0)?;
    system.select_window(window)?;
    system.begin_update(window)?;
    system.quickdraw().port(WindowId(window)).paint_rect(Rect::new(0, 0, 400, 300), 0xffffff)?;
    system.end_update(window)?;
    let cursor = system.quickdraw().cursors();
    cursor.set_standard(StandardCursor::Crosshair)?;
    checkpoint(system, window, "BUSY_STAGE idle\n")?;
    assert_eq!(system.processes().count()?,
               1,
               "CPU-only phase must have no cooperating background presenter");
    let start = clock();
    let mut stamp = *b"BUSY_CLOCK start=00000000000000000000\n";
    let end = stamp.len() - 1;
    let mut value = start;
    for digit in stamp[b"BUSY_CLOCK start=".len()..end].iter_mut().rev()
    {
        *digit = b'0' + (value % 10) as u8;
        value /= 10;
    }
    system.log("BUSY_STAGE spin\n")?;
    system.log(core::str::from_utf8(&stamp).unwrap())?;
    while clock().wrapping_sub(start) < 25_000_000
    {
        core::hint::spin_loop();
    }
    assert_eq!(cursor.visibility()?, 0);
    system.events()
          .wait_event(mac_toolbox::events::EventMask::from_bits_retain(0), 0)
          .map(mac_toolbox::Event::into_record)?;
    checkpoint(system, window, "BUSY_STAGE recovered\n")?;
    system.log("BUSY_STAGE peeking\n")?;
    let start = clock();
    while clock().wrapping_sub(start) < 12_000_000
    {
        system.events().peek(mac_toolbox::events::EventMask::from_bits_retain(0))?;
    }
    checkpoint(system, window, "BUSY_STAGE peek-recovered\n")?;
    system.log("BUSY_STAGE traps\n")?;
    let start = system.events().monotonic_nanoseconds()?;
    while system.events().monotonic_nanoseconds()? - start < 1_500_000_000
    {}
    system.events()
          .wait_event(mac_toolbox::events::EventMask::from_bits_retain(0), 0)
          .map(mac_toolbox::Event::into_record)?;
    checkpoint(system, window, "BUSY_STAGE trap-recovered\n")?;
    cursor.hide()?;
    system.log("BUSY_STAGE hidden\n")?;
    let start = clock();
    while clock().wrapping_sub(start) < 15_000_000
    {
        core::hint::spin_loop();
    }
    assert_eq!(cursor.visibility()?, -1, "System watch changed app hide depth");
    system.events()
          .wait_event(mac_toolbox::events::EventMask::from_bits_retain(0), 0)
          .map(mac_toolbox::Event::into_record)?;
    checkpoint(system, window, "BUSY_STAGE hidden-recovered\n")?;
    cursor.show()?;
    let volume = system.files().volume_at(0)?.id;
    let child = system.files().lookup(volume, 2, "Applications/QuickDraw Test")?;
    let child = system.processes().launch(volume, child.id)?;
    assert_ne!(child.thread, mine.thread);
    for _ in 0..10
    {
        system.events()
              .wait_event(mac_toolbox::events::EventMask::from_bits_retain(0), 1)
              .map(mac_toolbox::Event::into_record)?;
    }
    system.processes().activate(mine.id)?;
    system.events()
          .wait_event(mac_toolbox::events::EventMask::from_bits_retain(0), 0)
          .map(mac_toolbox::Event::into_record)?;
    checkpoint(system, window, "BUSY_STAGE switch-ready\n")?;
    system.log("BUSY_STAGE switch-spin\n")?;
    let start = clock();
    while clock().wrapping_sub(start) < 30_000_000
    {
        core::hint::spin_loop();
    }
    system.events()
          .wait_event(mac_toolbox::events::EventMask::from_bits_retain(0), 0)
          .map(mac_toolbox::Event::into_record)?;
    system.log("BUSY_CURSOR_USERSPACE_PASS\n")?;
    loop
    {
        system.events()
              .wait_event(mac_toolbox::events::EventMask::from_bits_retain(EVERY_EVENT), 600)
              .map(mac_toolbox::Event::into_record)?;
    }
}

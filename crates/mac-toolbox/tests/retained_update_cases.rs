//! Native retained drawing is committed by EndUpdate, including while another
//! actual application thread polls events and the writer explicitly presents.
#![cfg(target_arch = "riscv64")]
use mac_toolbox::{EventKind, Rect, System, WindowId, events::classic::EVERY_EVENT};

pub fn run(system: &System<'_>) -> Result<(), mac_toolbox::Error>
{
    system.log("DESKTOP_READY: retained update userspace regression\n")?;
    let mine = system.processes().current()?;
    let window = system.new_window(Rect::new(350, 350, 630, 450), "Atomic Update", (100, 60), 0)?;
    let volume = system.files().volume_at(0)?.id;
    let child = system.files().lookup(volume, 2, "Applications/QuickDraw Test")?;
    let child = system.processes().launch(volume, child.id)?;
    assert_ne!(child.thread, mine.thread, "background presenter needs its own OS thread");
    let start = system.events().uptime_ticks()?;
    loop
    {
        let ready = (0..system.processes().count()?).filter_map(|i| system.processes().at(i).ok())
                                                    .any(|p| p.id == child.id && p.switches >= 4);
        if ready
        {
            break;
        }
        assert!(system.events().uptime_ticks()? - start < 600,
                "background application did not run");
        system.next_event(mac_toolbox::events::EventMask::from_bits_retain(0))
              .map(mac_toolbox::Event::into_record)?;
    }
    system.processes().activate(mine.id)?;
    system.select_window(window)?;
    let port = system.quickdraw().port(WindowId(window));
    let bounds = Rect::new(0, 0, 280, 100);
    system.begin_update(window)?;
    port.paint_rect(bounds, 0xff0000)?;
    system.end_update(window)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE committed-red\n")?;

    system.begin_update(window)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE pending-empty\n")?;
    port.paint_rect(Rect::new(0, 0, 140, 100), 0x00ff00)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE pending-half\n")?;
    port.paint_rect(Rect::new(140, 0, 280, 100), 0x0000ff)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE pending-full\n")?;
    system.end_update(window)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE committed-split\n")?;

    system.begin_update(window)?;
    port.paint_rect(bounds, 0xffff00)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE pending-yellow\n")?;
    system.end_update(window)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE committed-yellow\n")?;
    // Exercise repeated presentations of an unchanged, clipped pattern at an
    // origin which is not aligned to its eight-pixel phase. Then append drawing
    // outside BeginUpdate and expose a previously obscured fragment.
    let graphics = system.quickdraw().graphics(WindowId(window));
    system.begin_update(window)?;
    graphics.colors(0x123456, 0xfedcba)?;
    graphics.pen_pattern([0x80, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01])?;
    graphics.paint_rect(bounds)?;
    system.end_update(window)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE committed-pattern\n")?;
    port.paint_rect(Rect::new(40, 20, 100, 60), 0)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE appended-black\n")?;
    let cover = system.new_window(Rect::new(400, 375, 530, 420), "Cover", (80, 30), 0)?;
    system.begin_update(cover)?;
    system.quickdraw().port(WindowId(cover)).paint_rect(Rect::new(0, 0, 130, 45), 0x00ffff)?;
    system.end_update(cover)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE pattern-occluded\n")?;
    system.close_window(cover)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE pattern-revealed\n")?;
    let background = (0..system.processes().count()?).filter_map(|i| system.processes().at(i).ok())
                                                     .find(|p| p.id == child.id)
                                                     .expect("background app unexpectedly exited");
    assert!(background.switches > child.switches + 4,
            "background presenter must run across staged updates");
    system.processes().hide(child.id)?;
    let desktop = system.quickdraw().desktop();
    let desktop_bounds = Rect::new(80, 180, 300, 280);
    desktop.begin_update()?;
    desktop.graphics().colors(0x00ffff, 0)?;
    desktop.graphics().paint_rect(desktop_bounds)?;
    desktop.end_update()?;
    checkpoint(system, "RETAINED_UPDATE_STAGE desktop-cyan\n")?;
    desktop.begin_update()?;
    desktop.graphics().colors(0xff0000, 0)?;
    desktop.graphics().paint_rect(desktop_bounds)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE desktop-pending\n")?;
    desktop.cancel_update()?;
    checkpoint(system, "RETAINED_UPDATE_STAGE desktop-cancelled\n")?;
    desktop.begin_update()?;
    desktop.graphics().colors(0xff00ff, 0)?;
    desktop.graphics().paint_rect(desktop_bounds)?;
    desktop.end_update()?;
    checkpoint(system, "RETAINED_UPDATE_STAGE desktop-magenta\n")?;
    let cover = system.new_window(Rect::new(90, 195, 290, 275), "Desktop Cover", (100, 40), 0)?;
    system.begin_update(cover)?;
    system.quickdraw().port(WindowId(cover)).paint_rect(Rect::new(0, 0, 200, 80), 0x123456)?;
    system.end_update(cover)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE desktop-covered\n")?;
    system.close_window(cover)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE desktop-revealed\n")?;
    // Restore the second real presenter, then commit cursor policy through the
    // same public drawing transaction as its pixels. An inactive editor still
    // owns an editable hover region without adding a blinking insertion point.
    system.processes().activate(child.id)?;
    system.processes().activate(mine.id)?;
    system.select_window(window)?;
    system.quickdraw().cursors().init()?;
    let text = system.textedit();
    let left = Rect::new(10, 10, 100, 70);
    let right = Rect::new(160, 10, 250, 70);
    let editor = text.create(left, left)?;
    text.activate(editor, false)?;
    system.begin_update(window)?;
    port.paint_rect(bounds, 0xffffff)?;
    text.draw(editor, WindowId(window))?;
    system.end_update(window)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE cursor-left\n")?;

    system.begin_update(window)?;
    port.paint_rect(bounds, 0xffffff)?;
    text.set_rects(editor, right, right)?;
    text.draw(editor, WindowId(window))?;
    checkpoint(system, "RETAINED_UPDATE_STAGE cursor-pending-right\n")?;
    system.end_update(window)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE cursor-right\n")?;

    system.begin_update(window)?;
    port.paint_rect(bounds, 0xffffff)?;
    system.end_update(window)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE cursor-omitted\n")?;
    system.log("RETAINED_UPDATE_USERSPACE_PASS separate_threads=2 stages=21\n")?;
    // Keep the last committed scene available until the owned VM is stopped.
    loop
    {
        system.next_event(mac_toolbox::events::EventMask::from_bits_retain(0))
              .map(mac_toolbox::Event::into_record)?;
    }
}

fn checkpoint(system: &System<'_>, stage: &str) -> Result<(), mac_toolbox::Error>
{
    // Both calls reach the resident compositor. A Null poll does not consume
    // update damage, and repeated polling must not expose pending commands.
    system.next_event(mac_toolbox::events::EventMask::from_bits_retain(0))
          .map(mac_toolbox::Event::into_record)?;
    system.events().input_state()?;
    system.log(stage)?;
    loop
    {
        let event = system.next_event(mac_toolbox::events::EventMask::ALL)
                          .map(mac_toolbox::Event::into_record)?;
        system.events().input_state()?;
        if event.kind == EventKind::KeyDown && event.character() == b' '
        {
            return Ok(());
        }
    }
}

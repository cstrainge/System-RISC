//! Native retained drawing is committed by EndUpdate, including while another
//! actual application thread polls events and the writer explicitly presents.
#![cfg(target_arch = "riscv64")]
use mac_toolbox::{EventKind, Rect, System, WindowId, events::EVERY_EVENT};

pub fn run(system: &System<'_>) -> Result<(), i32>
{
    system.log("DESKTOP_READY: retained update userspace regression\n")?;
    let mine = system.processes().current().map_err(|e| e.0)?;
    let window = system.new_window(Rect::new(350, 350, 630, 450), "Atomic Update", (100, 60), 0)?;
    let volume = system.files().volume_at(0).map_err(|e| e.0)?.id;
    let child = system.files().lookup(volume, 2, "Applications/QuickDraw Test").map_err(|e| e.0)?;
    let child = system.processes().launch(volume, child.id).map_err(|e| e.0)?;
    assert_ne!(child.thread, mine.thread, "background presenter needs its own OS thread");
    let start = system.events().uptime_ticks().map_err(|e| e.0)?;
    loop
    {
        let ready =
            (0..system.processes().count().map_err(|e| e.0)?).filter_map(|i| {
                                                                 system.processes().at(i).ok()
                                                             })
                                                             .any(|p| {
                                                                 p.id == child.id && p.switches >= 4
                                                             });
        if ready
        {
            break;
        }
        assert!(system.events().uptime_ticks().map_err(|e| e.0)? - start < 600,
                "background application did not run");
        system.next_event(0)?;
    }
    system.processes().activate(mine.id).map_err(|e| e.0)?;
    system.select_window(window)?;
    let port = system.quickdraw().port(WindowId(window));
    let bounds = Rect::new(0, 0, 280, 100);
    system.begin_update(window)?;
    port.paint_rect(bounds, 0xff0000).map_err(|e| e.0)?;
    system.end_update(window)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE committed-red\n")?;

    system.begin_update(window)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE pending-empty\n")?;
    port.paint_rect(Rect::new(0, 0, 140, 100), 0x00ff00).map_err(|e| e.0)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE pending-half\n")?;
    port.paint_rect(Rect::new(140, 0, 280, 100), 0x0000ff).map_err(|e| e.0)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE pending-full\n")?;
    system.end_update(window)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE committed-split\n")?;

    system.begin_update(window)?;
    port.paint_rect(bounds, 0xffff00).map_err(|e| e.0)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE pending-yellow\n")?;
    system.end_update(window)?;
    checkpoint(system, "RETAINED_UPDATE_STAGE committed-yellow\n")?;
    let background =
        (0..system.processes().count().map_err(|e| e.0)?).filter_map(|i| {
                                                             system.processes().at(i).ok()
                                                         })
                                                         .find(|p| p.id == child.id)
                                                         .expect("background app unexpectedly \
                                                                  exited");
    assert!(background.switches > child.switches + 4,
            "background presenter must run across staged updates");
    system.log("RETAINED_UPDATE_USERSPACE_PASS separate_threads=2 stages=7\n")?;
    // Keep the last committed scene available until the owned VM is stopped.
    loop
    {
        system.next_event(0)?;
    }
}

fn checkpoint(system: &System<'_>, stage: &str) -> Result<(), i32>
{
    // Both calls reach the resident compositor. A Null poll does not consume
    // update damage, and repeated polling must not expose pending commands.
    system.next_event(0)?;
    system.events().input_state().map_err(|e| e.0)?;
    system.log(stage)?;
    loop
    {
        let event = system.next_event(EVERY_EVENT)?;
        system.events().input_state().map_err(|e| e.0)?;
        if event.kind == EventKind::KeyDown && event.character() == b' '
        {
            return Ok(());
        }
    }
}

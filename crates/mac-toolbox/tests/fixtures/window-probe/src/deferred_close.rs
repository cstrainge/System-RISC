//! Collect actual close tracking observations through the public Window SDK.
use core::fmt::Write;

use mac_toolbox::{Rect, System, WindowId,
                  events::classic::{EVERY_EVENT, EventKind},
                  windows::{WINDOW_CLOSE_REQUEST, WindowSpec}};
struct Line
{
    bytes: [u8; 256],
    len:   usize
}
impl core::fmt::Write for Line
{
    fn write_str(&mut self, s: &str) -> core::fmt::Result
    {
        if self.len + s.len() > self.bytes.len()
        {
            return Err(core::fmt::Error);
        }
        self.bytes[self.len..self.len + s.len()].copy_from_slice(s.as_bytes());
        self.len += s.len();
        Ok(())
    }
}
fn log(system: &System<'_>, args: core::fmt::Arguments<'_>) -> mac_toolbox::ToolboxResult<()>
{
    let mut line = Line { bytes: [0; 256],
                          len:   0 };
    let _ = line.write_fmt(args);
    system.log(core::str::from_utf8(&line.bytes[..line.len]).unwrap())
}
fn draw(system: &System<'_>, window: WindowId) -> mac_toolbox::ToolboxResult<()>
{
    system.windows().begin_update(window)?;
    let port = system.quickdraw().graphics(window);
    port.colors(0xffffff, 0xffffff)?;
    port.paint_rect(Rect::new(0, 0, 440, 300))?;
    port.colors(0x336699, 0xffffff)?;
    port.paint_rect(Rect::new(20, 20, 60, 50))?;
    port.text(Default::default(),
              mac_toolbox::Point { h: 80,
                                   v: 45 },
              "Retained close contents",
              0)?;
    system.windows().end_update(window)
}
pub fn run(system: &System<'_>) -> mac_toolbox::ToolboxResult<()>
{
    let windows = system.windows();
    let deferred = windows.create(WindowSpec::new(Rect::new(100, 100, 540, 400),
                                                  "DEFERRED CLOSE").procedure(8))?;
    windows.set_close_deferred(deferred, true)?;
    system.quickdraw().cursors().hide()?;
    let mut automatic = None;
    let mut close_requests = 0u32;
    let mut auto_events = 0u32;
    let mut updates = 0u32;
    let mut query = 0u32;
    system.log("DESKTOP_READY: public SDK deferred close fixture\n")?;
    loop
    {
        let event = system.events().wait_event(mac_toolbox::events::EventMask::from_bits_retain(EVERY_EVENT), 1).map(mac_toolbox::Event::into_record)?;
        if event.kind == EventKind::Update
           && (event.message == deferred.0
               || automatic.is_some_and(|id: WindowId| id.0 == event.message))
        {
            draw(system, WindowId(event.message))?;
            updates += 1;
            log(system, format_args!("DEFERRED_PAINT id={} updates={}\n", event.message, updates))?;
        }
        if event.kind == EventKind::OsEvent
        {
            if event.message & 0xff00_0000 == WINDOW_CLOSE_REQUEST
            {
                close_requests += 1;
                let id = WindowId(event.message & 0x00ff_ffff);
                log(system,
                    format_args!("DEFERRED_REQUEST id={} alive={} count={}\n",
                                 id.0,
                                 u32::from(windows.info(id).is_ok()),
                                 close_requests))?;
            }
            if event.message & 0xff00_0000 == 0x0200_0000
            {
                auto_events += 1;
                log(system,
                    format_args!("AUTOMATIC_NOTICE id={} count={}\n",
                                 event.message & 0x00ff_ffff,
                                 auto_events))?;
            }
        }
        if event.kind == EventKind::KeyDown && event.character() == b'c' && automatic.is_none()
        {
            windows.close(deferred)?;
            log(system,
                format_args!("DEFERRED_DISPOSE id={} alive={}\n",
                             deferred.0,
                             u32::from(windows.info(deferred).is_ok())))?;
            let id = windows.create(WindowSpec::new(Rect::new(100, 100, 540, 400),
                                                    "AUTOMATIC CLOSE").procedure(8))?;
            automatic = Some(id);
            log(system, format_args!("AUTOMATIC_READY id={}\n", id.0))?;
        }
        if event.kind == EventKind::KeyDown && event.character() == b'p'
        {
            query += 1;
            log(system,
                format_args!("DEFERRED_STATE query={} deferred={} automatic={} requests={} \
                              notices={} updates={} front={}\n",
                             query,
                             u32::from(windows.info(deferred).is_ok()),
                             u32::from(automatic.is_some_and(|id| windows.info(id).is_ok())),
                             close_requests,
                             auto_events,
                             updates,
                             windows.front()?.map_or(0, |id| id.0)))?;
        }
    }
}

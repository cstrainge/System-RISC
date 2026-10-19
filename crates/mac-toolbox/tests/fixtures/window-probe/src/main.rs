//! Test application uses only the aggregate userspace SDK, no resident code.
#![no_std]
#![no_main]
use mac_toolbox::{System,
                  events::{EVERY_EVENT, EventKind},
                  quickdraw::Rect,
                  windows::WindowSpec};
#[cfg(feature = "menus")]
const KEYLESS_LABEL: &str = "Unclipped keyless label";

fn run(system: &System<'_>) -> mac_toolbox::ToolboxResult<()>
{
    let window = system.windows().create(WindowSpec::new(Rect::new(100,100,540,400), "TRACKING TEST")
        .minimum(160,100).procedure(8))?;
    system.quickdraw().cursors().hide()?;
    #[cfg(feature = "menus")]
    for id in 256..=261
    {
        system.menus().install_menu_resource(1, id)?;
    }
    system.log("DESKTOP_READY: public SDK tracking fixture\n").map_err(mac_toolbox::Error)?;
    loop
    {
        let event = system.events().next_event(EVERY_EVENT)?;
        #[cfg(feature = "menus")]
        if event.kind == EventKind::KeyDown && event.character() == b'g'
        {
            system.menus().clear()?;
            system.menus().install_menu_resource(1, 30000)?;
            use core::fmt::Write;
            let width = system.quickdraw().fonts().text_width(Default::default(), KEYLESS_LABEL)?;
            let mut line = Line { bytes: [0; 128],
                                  len:   0 };
            let _ = writeln!(&mut line, "MENU_LABEL width={width}");
            system.log(core::str::from_utf8(&line.bytes[..line.len]).unwrap())
                  .map_err(mac_toolbox::Error)?;
        }
        #[cfg(feature = "menus")]
        if let Some(selection) = system.menus().selection(&event)?
        {
            use core::fmt::Write;
            let mut line = Line { bytes: [0; 128],
                                  len:   0 };
            let _ = writeln!(&mut line,
                             "MENU_PROBE menu={} item={} command={:08x}",
                             selection.menu_id, selection.item, selection.command);
            system.log(core::str::from_utf8(&line.bytes[..line.len]).unwrap())
                  .map_err(mac_toolbox::Error)?;
        }
        if event.kind == EventKind::Update && event.message == window.0
        {
            let _ = system.windows().begin_update(window)?;
            let info = system.windows().info(window)?;
            let port = system.quickdraw().graphics(window);
            port.colors(0xffffff, 0xffffff)?;
            port.paint_rect(Rect::new(0,
                                      0,
                                      info.bounds.right - info.bounds.left,
                                      info.bounds.bottom - info.bounds.top))?;
            #[cfg(feature = "menus")]
            port.text(Default::default(),
                      mac_toolbox::Point { h: 16,
                                           v: 40 },
                      KEYLESS_LABEL,
                      0)?;
            system.windows().end_update(window)?;
            system.log("WINDOW_PROBE_READY\n").map_err(mac_toolbox::Error)?;
        }
    }
}

/// # Safety
/// Loader supplies the live public service table for this application.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const mac_toolbox::SystemServices)
                                -> mac_toolbox::Status
{
    let system = match unsafe { System::from_raw(services) }
    {
        Ok(system) => system,
        Err(error) => return error
    };
    match run(&system)
    {
        Ok(()) => 0,
        Err(error) => error.0
    }
}
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> !
{
    loop
    {
        core::hint::spin_loop();
    }
}

#[cfg(feature = "menus")]
struct Line
{
    bytes: [u8; 128],
    len:   usize
}
#[cfg(feature = "menus")]
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

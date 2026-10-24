//! Test application uses only the aggregate userspace SDK, no resident code.
#![no_std]
#![no_main]
use mac_toolbox::System;
#[cfg(not(feature = "deferred-close"))]
use mac_toolbox::{events::classic::{EVERY_EVENT, EventKind},
                  quickdraw::Rect,
                  windows::WindowSpec};
#[cfg(feature = "menus")]
#[path = "../../../conformance/menu_sizing.rs"]
mod menu_sizing;
#[cfg(feature = "menus")]
const KEYLESS_LABEL: &str = "Unclipped keyless label";

#[cfg(feature = "deferred-close")]
mod deferred_close;
#[cfg(feature = "deferred-close")]
use deferred_close::run;

#[cfg(not(feature = "deferred-close"))]
fn run(system: &System<'_>) -> mac_toolbox::ToolboxResult<()>
{
    let window = system.windows().create(WindowSpec::new(Rect::new(100,100,540,400), "TRACKING TEST")
        .minimum(160,100).procedure(8))?;
    system.quickdraw().cursors().hide()?;
    #[cfg(feature = "menus")]
    for id in 256..=261
    {
        system.menus().install_menu_resource(mac_toolbox::ResourceScope::Application, id)?;
    }
    system.log("DESKTOP_READY: public SDK tracking fixture\n")?;
    let mut painted = false;
    let mut cursor_visible = false;
    loop
    {
        let event = system.events()
                          .next(mac_toolbox::events::EventMask::from_bits_retain(EVERY_EVENT))
                          .map(mac_toolbox::Event::into_record)?;
        // Disabled command equivalents remain ordinary application key events.
        // Observe them through the public SDK; canonical host Rust assertions
        // verify exact-once delivery, Command modifiers and absent selection.
        #[cfg(feature = "menus")]
        if event.kind == EventKind::KeyDown
           && matches!(event.character(), b'o' | b'w')
           && event.modifiers & mac_toolbox::events::classic::CMD_KEY != 0
        {
            use core::fmt::Write;
            let mut line = Line { bytes: [0; 128],
                                  len:   0 };
            let _ = writeln!(&mut line,
                             "MENU_NATIVE_KEY kind={} character={} modifiers={} selection={}",
                             event.kind as u16,
                             event.character(),
                             event.modifiers,
                             system.menus().selection(&event.into())?.is_some());
            system.log(core::str::from_utf8(&line.bytes[..line.len]).unwrap())?;
        }
        #[cfg(feature = "menus")]
        if event.kind == EventKind::KeyDown && event.character() == b'g'
        {
            menu_sizing::run(system)?;
            system.log("MENU_SIZING_PASS cases=8\n")?;
            system.menus().clear()?;
            system.menus().install_menu_resource(mac_toolbox::ResourceScope::Application, 30000)?;
            use core::fmt::Write;
            let width = system.quickdraw().fonts().text_width(Default::default(), KEYLESS_LABEL)?;
            let mut line = Line { bytes: [0; 128],
                                  len:   0 };
            let _ = writeln!(&mut line, "MENU_LABEL width={width}");
            system.log(core::str::from_utf8(&line.bytes[..line.len]).unwrap())?;
        }
        #[cfg(feature = "menus")]
        if event.kind == EventKind::KeyDown && event.character() == b'h'
        {
            system.menus().clear()?;
            system.menus().install_menu_resource(mac_toolbox::ResourceScope::Application, 30001)?;
            system.log("MENU_KEYED_READY\n")?;
        }
        #[cfg(feature = "menus")]
        if let Some(selection) = system.menus().selection(&event.into())?
        {
            use core::fmt::Write;
            let mut line = Line { bytes: [0; 128],
                                  len:   0 };
            let _ = writeln!(&mut line,
                             "MENU_PROBE menu={} item={} command={:08x}",
                             selection.menu_id, selection.item, selection.command);
            system.log(core::str::from_utf8(&line.bytes[..line.len]).unwrap())?;
        }
        if event.kind == EventKind::KeyDown && event.character() == b'p'
        {
            painted = !painted;
            let port = system.quickdraw().graphics(window);
            port.colors(if painted { 0x0000ff } else { 0xffffff }, 0xffffff)?;
            port.paint_rect(Rect::new(0, 0, 440, 60))?;
            system.log(if painted
                       {
                           "WINDOW_PROBE_PAINT blue\n"
                       }
                       else
                       {
                           "WINDOW_PROBE_PAINT white\n"
                       })?;
        }
        if event.kind == EventKind::KeyDown && event.character() == b'v'
        {
            cursor_visible = !cursor_visible;
            if cursor_visible
            {
                system.quickdraw().cursors().show()?;
            }
            else
            {
                system.quickdraw().cursors().hide()?;
            }
            system.log("WINDOW_PROBE_CURSOR_CHANGED\n")?;
        }
        if event.kind == EventKind::Update && event.window == window.0
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
            #[cfg(feature = "menus")]
            {
                port.text(Default::default(),
                          mac_toolbox::Point { h: 16,
                                               v: 65 },
                          "Close Window",
                          0)?;
                port.text(Default::default(),
                          mac_toolbox::Point { h: 16,
                                               v: 90 },
                          "\u{11}W",
                          0)?;
            }
            system.windows().end_update(window)?;
            system.log("WINDOW_PROBE_READY\n")?;
        }
    }
}

/// # Safety
/// Loader supplies the live public service table for this application.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const mac_toolbox::SystemServices)
                                -> mac_toolbox::abi::Status
{
    let system = match unsafe { System::from_raw(services) }
    {
        Ok(system) => system,
        Err(error) => return error.status()
    };
    match run(&system)
    {
        Ok(()) => 0,
        Err(error) => error.status()
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

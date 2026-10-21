//! Capture transport only. Canonical acceptance belongs in tests/events.rs.
#![no_std]
#![no_main]
use core::fmt::{self, Write};

use mac_toolbox::{System, SystemServices, events::classic::{EVERY_EVENT, EventKind}};
struct Text
{
    bytes: [u8; 512],
    len:   usize
}
impl Text
{
    fn new() -> Self
    {
        Self { bytes: [0; 512],
               len:   0 }
    }

    fn str(&self) -> &str
    {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("EVENT_INPUT_FAIL utf8\n")
    }
}
impl Write for Text
{
    fn write_str(&mut self, value: &str) -> fmt::Result
    {
        if value.len() > self.bytes.len() - self.len
        {
            return Err(fmt::Error);
        }
        self.bytes[self.len..self.len + value.len()].copy_from_slice(value.as_bytes());
        self.len += value.len();
        Ok(())
    }
}
fn log_event(system: &System<'_>, e: mac_toolbox::events::classic::Event)
{
    let mut text = Text::new();
    let _ = writeln!(text,
                     "EVENT_INPUT_OS \
                      {{\"kind\":{},\"message\":{},\"x\":{},\"y\":{},\"modifiers\":{},\"ticks\":\
                      {}}}",
                     e.kind as u16, e.message, e.where_.h, e.where_.v, e.modifiers, e.when);
    let _ = system.log(text.str());
}
fn still_probe(system: &System<'_>) -> mac_toolbox::events::Result<()>
{
    let events = system.events();
    events.os_post(EventKind::MouseUp, 0)?;
    let before = events.still_down()?;
    let first = events.os_event(6, true)?;
    let after = events.still_down()?;
    let peek = events.os_event(6, false)?;
    let physical = events.button()?;
    let second = events.os_event(6, true)?;
    let final_state = events.still_down()?;
    let mut text = Text::new();
    let _ = writeln!(text,
                     "EVENT_INPUT_STILL_DOWN \
                      {{\"before\":{},\"first\":{},\"after\":{},\"peek_available\":{},\"peek\":\
                      {},\"physical\":{},\"second\":{},\"final\":{}}}",
                     before,
                     first.event.kind as u16,
                     after,
                     peek.available,
                     peek.event.kind as u16,
                     physical,
                     second.event.kind as u16,
                     final_state);
    let _ = system.log(text.str());
    log_event(system, first.event);
    log_event(system, second.event);
    Ok(())
}
/// # Safety
/// The loader supplies the live service table for the application's lifetime.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> mac_toolbox::abi::Status
{
    let system = match unsafe { System::from_raw(services) }
    {
        Ok(s) => s,
        Err(e) => return e.status()
    };
    if system.menus().clear().is_err() || system.menus().install_menu_resource(mac_toolbox::ResourceScope::Application, 128).is_err()
    {
        let _ = system.log("EVENT_INPUT_FAIL menu\n");
        return -1;
    }
    let events = system.events();
    let _ = events.set_os_mask(EVERY_EVENT);
    let _ = events.os_flush(EVERY_EVENT, 0);
    let _ = system.log("DESKTOP_READY: Event Manager physical input fixture\n");
    let mut previous = None;
    let mut mouse_probed = false;
    loop
    {
        let state = match events.input_state()
        {
            Ok(s) => s,
            Err(_) =>
            {
                let _ = system.log("EVENT_INPUT_FAIL state\n");
                return -1;
            }
        };
        let current = (state.mouse, state.modifiers.bits(), state.keys);
        if previous != Some(current)
        {
            let mut text = Text::new();
            let _ = write!(text,
                           "EVENT_INPUT_SAMPLE \
                            {{\"x\":{},\"y\":{},\"modifiers\":{},\"ticks\":{},\"uptime\":{},\"\
                            keys\":\"",
                           state.mouse.h,
                           state.mouse.v,
                           state.modifiers.bits(),
                           state.ticks,
                           state.uptime_ticks);
            for byte in state.keys.0
            {
                let _ = write!(text, "{byte:02x}");
            }
            let _ = writeln!(text, "\"}}");
            let _ = system.log(text.str());
            previous = Some(current);
        }
        if state.modifiers.button_down() && !mouse_probed
        {
            if still_probe(&system).is_err()
            {
                let _ = system.log("EVENT_INPUT_FAIL StillDown\n");
                return -1;
            }
            mouse_probed = true;
        }
        for _ in 0..32
        {
            // Input can arrive between the state snapshot and this drain. Keep
            // physical mouse events queued until StillDown has observed the held
            // button, instead of making capture timing part of its contract.
            let mask = if mouse_probed { EVERY_EVENT } else { EVERY_EVENT & !6 };
            match events.os_event(mask, true)
            {
                Ok(read) if read.available =>
                {
                    log_event(&system, read.event);
                }
                Ok(_) => break,
                Err(_) =>
                {
                    let _ = system.log("EVENT_INPUT_FAIL queue\n");
                    return -1;
                }
            }
        }
        if let Ok(event) = events.next(mac_toolbox::events::EventMask::from_bits_retain(EVERY_EVENT)).map(mac_toolbox::Event::into_record)
           && event.kind == EventKind::MenuCommand
        {
            let _ = system.log("EVENT_INPUT_MENU_COMMAND\n");
        }
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

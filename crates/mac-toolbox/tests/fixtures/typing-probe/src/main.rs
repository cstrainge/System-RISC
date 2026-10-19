//! Measure the actual TextEdit application editor and menu policy through resident services.
#![no_std]
#![no_main]
#![allow(dead_code)]
use core::{fmt::Write,
           sync::atomic::{AtomicPtr, AtomicUsize, Ordering}};

use mac_toolbox::*;
#[path = "../../../../../../apps/textedit-exerciser/src/editor.rs"]
mod editor;
#[path = "../../../../../../apps/textedit-exerciser/src/menus.rs"]
mod menus;
#[path = "../../../../../../apps/textedit-exerciser/src/ui.rs"]
mod ui;
use ui::{Text, text};
static SERVICES: AtomicPtr<SystemServices> = AtomicPtr::new(core::ptr::null_mut());
static FILES: AtomicUsize = AtomicUsize::new(0);
static MENUS: AtomicUsize = AtomicUsize::new(0);
unsafe extern "C" fn trace(number: u32, pointer: *mut (), size: usize) -> i64
{
    if number == mac_abi::traps::FILE
    {
        FILES.fetch_add(1, Ordering::Relaxed);
    }
    if number == mac_abi::traps::MENU
    {
        MENUS.fetch_add(1, Ordering::Relaxed);
    }
    // SAFETY: forwards each original synchronous borrowed request unchanged.
    unsafe { ((*SERVICES.load(Ordering::Acquire)).trap)(number, pointer, size) }
}
fn run(system: &System<'_>) -> Result<(), Status>
{
    menus::install(system)?;
    let mut editor = editor::Editor::default();
    editor.open(system)?;
    menus::update(system, &editor)?;
    system.log("DESKTOP_READY: actual TextEdit application typing profile\n")?;
    let mut edit_ticks = 0;
    let mut menu_ticks = 0;
    FILES.store(0, Ordering::Relaxed);
    MENUS.store(0, Ordering::Relaxed);
    for _ in 0..20
    {
        let start = system.events().uptime_ticks().map_err(|e| e.0)?;
        if !editor.event(system,
                         Event { kind: EventKind::KeyDown,
                                 message: u32::from(b'a'),
                                 ..Default::default() })?
        {
            return Err(BAD_ARGUMENT);
        }
        let edited = system.events().uptime_ticks().map_err(|e| e.0)?;
        menus::update(system, &editor)?;
        // Same consumed/released routing as the actual TextEdit application main loop.
        if editor.event(system,
                        Event { kind: EventKind::KeyUp,
                                message: u32::from(b'a'),
                                ..Default::default() })?
        {
            menus::update(system, &editor)?;
        }
        else
        {
            menus::update(system, &editor)?;
        }
        let end = system.events().uptime_ticks().map_err(|e| e.0)?;
        edit_ticks += edited - start;
        menu_ticks += end - edited;
    }
    let mut line = Text::<192>::default();
    let _ = writeln!(&mut line,
                     "TYPING_PROFILE chars=20 edit_ticks={} menu_ticks={} file_calls={} \
                      menu_calls={}",
                     edit_ticks,
                     menu_ticks,
                     FILES.load(Ordering::Relaxed),
                     MENUS.load(Ordering::Relaxed));
    system.log(line.as_str())?;
    // Getters and idempotent writes must not induce expensive stale frames.
    let start = system.events().uptime_ticks().map_err(|e| e.0)?;
    for _ in 0..50
    {
        let api = system.menus();
        let _ = api.layout(129).map_err(|e| e.0)?;
        let item = api.item(129, 3).map_err(|e| e.0)?;
        api.set_item_enabled(129, 3, item.enabled).map_err(|e| e.0)?;
        let _ = system.events().next_event(0).map_err(|e| e.0)?;
    }
    let elapsed = system.events().uptime_ticks().map_err(|e| e.0)? - start;
    let mut line = Text::<128>::default();
    let _ = writeln!(&mut line, "TYPING_QUERIES polls=50 elapsed_ticks={}", elapsed);
    system.log(line.as_str())?;
    system.log("TYPING_PROFILE_DONE\n")?;
    Ok(())
}
/// # Safety
/// The loader supplies a live resident service table throughout this application.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> Status
{
    SERVICES.store(services.cast_mut(), Ordering::Release);
    let original = unsafe { &*services };
    let profiled = SystemServices { header:       original.header,
                                    trap:         trace,
                                    capabilities: original.capabilities };
    let system = match unsafe { System::from_raw(&profiled) }
    {
        Ok(s) => s,
        Err(e) => return e
    };
    if let Err(e) = run(&system)
    {
        let _ = system.log("TYPING_PROFILE_FAIL\n");
        return e;
    }
    OK
}
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> !
{
    loop
    {
        core::hint::spin_loop();
    }
}

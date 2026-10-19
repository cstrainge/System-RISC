//! Exercise the actual Finder viewer source through the public SDK.
#![no_std]
#![no_main]
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use mac_toolbox::{Event, EventKind, Status, System, SystemServices, events::EVERY_EVENT};
static SERVICES: AtomicPtr<SystemServices> = AtomicPtr::new(core::ptr::null_mut());
static FILES: AtomicUsize = AtomicUsize::new(0);
static MENUS: AtomicUsize = AtomicUsize::new(0);
static COMMITS: AtomicUsize = AtomicUsize::new(0);
static TRACKS: AtomicUsize = AtomicUsize::new(0);
static VERTICAL_VALUE: AtomicUsize = AtomicUsize::new(0);

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
    if number == mac_abi::traps::WINDOW
       && size == core::mem::size_of::<mac_abi::traps::WindowCall>()
    {
        // SAFETY: the SDK lends this typed request synchronously to its gate.
        let call = unsafe { &*pointer.cast::<mac_abi::traps::WindowCall>() };
        if call.operation == mac_abi::END_UPDATE
        {
            COMMITS.fetch_add(1, Ordering::Relaxed);
        }
    }
    if number == mac_abi::traps::CONTROL
       && size == core::mem::size_of::<mac_abi::controls::ControlCall>()
    {
        // SAFETY: same synchronous typed SDK request contract as above.
        let call = unsafe { &*pointer.cast::<mac_abi::controls::ControlCall>() };
        if call.operation == mac_abi::controls::TRACK_SCROLLBAR
        {
            TRACKS.fetch_add(1, Ordering::Relaxed);
        }
        if call.operation == mac_abi::controls::DRAW_SCROLLBAR
           && call.scrollbar.bounds.bottom - call.scrollbar.bounds.top
              > call.scrollbar.bounds.right - call.scrollbar.bounds.left
        {
            VERTICAL_VALUE.store(call.scrollbar.value as usize, Ordering::Relaxed);
        }
    }
    // SAFETY: forward the original borrowed request unchanged to resident OS.
    unsafe { ((*SERVICES.load(Ordering::Acquire)).trap)(number, pointer, size) }
}

fn require(value: bool, system: &System<'_>, message: &str) -> Result<(), Status>
{
    if value { Ok(()) } else { system.log(message)?; Err(mac_toolbox::BAD_ARGUMENT) }
}
#[path = "../../../../../../finder/src/clipboard.rs"]
mod clipboard;
#[allow(dead_code)]
#[path = "../../../../../../finder/src/browser.rs"]
mod browser;
#[allow(dead_code)]
#[path = "../../../../../../finder/src/icon_view.rs"]
mod icon_view;
#[allow(dead_code)]
#[path = "../../../../../../finder/src/menus.rs"]
mod menus;
mod ui
{
    use mac_toolbox::{Status, System};
    pub fn text<'b>(system: &System<'_>, index: u32, output: &'b mut [u8])
                    -> Result<&'b str, Status>
    {
        system.resources().application().indexed_string(2000, index, output).map_err(|e| e.0)
    }
}
fn run(system: &System<'_>) -> Result<(), Status>
{
    system.log("CLIPBOARD_PROBE_START\n")?;
    system.quickdraw().cursors().hide().map_err(|e| e.0)?;
    system.log("CLIPBOARD_PROBE_HIDDEN\n")?;
    system.scrap().clear().map_err(|e| e.0)?;
    let mut viewer = clipboard::Viewer::default();
    system.log("CLIPBOARD_PROBE_CLEARED\n")?;
    viewer.open(system)?;
    system.log("DESKTOP_READY: Finder clipboard public SDK fixture\n")?;
    system.log("CLIPBOARD_PROBE_EMPTY\n")?;
    let mut scroll_mode = false;
    let mut stationary = false;
    let mut moved = false;
    loop
    {
        let event = system.next_event(EVERY_EVENT)?;
        let before = COMMITS.load(Ordering::Relaxed);
        if event.kind != EventKind::KeyDown
        {
            let handled = viewer.event(system, event)?;
            if scroll_mode && event.kind == EventKind::MouseDown && !stationary
            {
                require(system.events().button().map_err(|e| e.0)?,
                        system, "CLIPBOARD_SCROLL_FAIL physical button not held\n")?;
                let commits = COMMITS.load(Ordering::Relaxed);
                let tracks = TRACKS.load(Ordering::Relaxed);
                let files = FILES.load(Ordering::Relaxed);
                let menus = MENUS.load(Ordering::Relaxed);
                let browser = browser::Browser::default();
                for _ in 0..100
                {
                    // Real public event polling supplies the current held point.
                    let idle = system.next_event(0)?;
                    require(viewer.event(system, idle)?, system,
                            "CLIPBOARD_SCROLL_FAIL held idle was not consumed\n")?;
                    // Same shared policy call used by Finder's consumed-event
                    // branch; observe actual SDK IO and menu service traffic.
                    menus::update_after_event(system, &browser, idle)?;
                }
                require(TRACKS.load(Ordering::Relaxed) - tracks >= 100,
                        system, "CLIPBOARD_SCROLL_FAIL thumb was not tracking\n")?;
                require(COMMITS.load(Ordering::Relaxed) == commits,
                        system, "CLIPBOARD_SCROLL_FAIL stationary thumb repainted\n")?;
                require(FILES.load(Ordering::Relaxed) == files
                        && MENUS.load(Ordering::Relaxed) == menus,
                        system, "CLIPBOARD_SCROLL_FAIL idle policy queried services\n")?;
                system.log("CLIPBOARD_IDLE_POLICY_PASS nulls=100 file_calls=0 menu_calls=0\n")?;
                stationary = true;
                system.log("CLIPBOARD_SCROLL_STATIONARY_PASS nulls=100 commits=0\n")?;
            }
            if stationary && !moved && VERTICAL_VALUE.load(Ordering::Relaxed) > 0
            {
                moved = true;
                system.log("CLIPBOARD_SCROLL_MOVED_PASS\n")?;
            }
            if stationary && event.kind == EventKind::MouseUp
            {
                require(moved && COMMITS.load(Ordering::Relaxed) == before,
                        system, "CLIPBOARD_SCROLL_FAIL unchanged release repainted\n")?;
                system.log("CLIPBOARD_SCROLL_RELEASE_PASS commits=0\n")?;
                scroll_mode = false;
            }
            if handled
            {
                continue;
            }
        }
        if event.kind == EventKind::KeyDown
        {
            match event.character().to_ascii_lowercase()
            {
                b't' =>
                {
                    system.scrap().clear().map_err(|e| e.0)?;
                    system.scrap().put_text("abc").map_err(|e| e.0)?;
                    viewer.refresh(system)?;
                    system.log("CLIPBOARD_PROBE_TEXT\n")?;
                }
                b's' =>
                {
                    let mut text = [b'x'; 1200];
                    for (index, byte) in text.iter_mut().enumerate()
                    {
                        if index % 20 == 19 { *byte = b'\r'; }
                    }
                    system.scrap().clear().map_err(|e| e.0)?;
                    system.scrap().put_text(core::str::from_utf8(&text).map_err(|_| mac_toolbox::BAD_ARGUMENT)?)
                          .map_err(|e| e.0)?;
                    viewer.refresh(system)?;
                    let commits = COMMITS.load(Ordering::Relaxed);
                    viewer.event(system, Event { kind: EventKind::Activate,
                                                message: viewer.window + 100,
                                                modifiers: mac_toolbox::ACTIVE_FLAG,
                                                ..Default::default() })?;
                    require(COMMITS.load(Ordering::Relaxed) == commits,
                            system, "CLIPBOARD_SCROLL_FAIL unrelated activation repainted\n")?;
                    system.log("CLIPBOARD_UNRELATED_ACTIVATE_PASS commits=0\n")?;
                    scroll_mode = true;
                    stationary = false;
                    moved = false;
                    system.log("CLIPBOARD_PROBE_SCROLL\n")?;
                }
                b'e' =>
                {
                    system.scrap().clear().map_err(|e| e.0)?;
                    viewer.refresh(system)?;
                    system.log("CLIPBOARD_PROBE_EMPTY_AGAIN\n")?;
                }
                _ =>
                {}
            }
        }
    }
}
/// # Safety
/// The loader supplies a live service table for the application's lifetime.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> Status
{
    SERVICES.store(services.cast_mut(), Ordering::Release);
    // SAFETY: the loader validates this service table for the whole call.
    let original = unsafe { &*services };
    let profiled = SystemServices { header: original.header,
                                    trap: trace,
                                    capabilities: original.capabilities };
    let system = match unsafe { System::from_raw(&profiled) }
    {
        Ok(s) => s,
        Err(e) => return e
    };
    if let Err(error) = run(&system)
    {
        let _ = system.log("CLIPBOARD_PROBE_FAIL\n");
        return error;
    }
    0
}
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> !
{
    loop
    {
        core::hint::spin_loop();
    }
}

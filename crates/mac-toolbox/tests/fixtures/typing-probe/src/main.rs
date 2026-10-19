//! Measure the actual TextEdit application editor and menu policy through resident services.
#![no_std]
#![no_main]
#![allow(dead_code)]
use core::{fmt::Write,
           sync::atomic::{AtomicPtr, AtomicUsize, Ordering}};

use mac_toolbox::*;
#[path = "../../../../../../apps/textedit-exerciser/src/editor.rs"]
mod editor;
#[path = "../../../../../../apps/textedit-exerciser/src/batch.rs"]
mod batch;
#[path = "../../../../../../apps/textedit-exerciser/src/menus.rs"]
mod menus;
#[path = "../../../../../../apps/textedit-exerciser/src/ui.rs"]
mod ui;
use ui::{Text, text};
static SERVICES: AtomicPtr<SystemServices> = AtomicPtr::new(core::ptr::null_mut());
static FILES: AtomicUsize = AtomicUsize::new(0);
static MENUS: AtomicUsize = AtomicUsize::new(0);
static UPDATES: AtomicUsize = AtomicUsize::new(0);
static GROW_CLIPS: AtomicUsize = AtomicUsize::new(0);
static EDITOR: AtomicUsize = AtomicUsize::new(0);
static KEYS: AtomicUsize = AtomicUsize::new(0);
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
        // SAFETY: the public SDK supplied this synchronous typed window request.
        let call = unsafe { &*pointer.cast::<mac_abi::traps::WindowCall>() };
        if call.operation == mac_abi::END_UPDATE
        {
            UPDATES.fetch_add(1, Ordering::Relaxed);
        }
        if call.operation == mac_abi::SET_GROW_ICON_CLIP
        {
            GROW_CLIPS.fetch_add(1, Ordering::Relaxed);
        }
    }
    // SAFETY: forwards each original synchronous borrowed request unchanged.
    let status = unsafe { ((*SERVICES.load(Ordering::Acquire)).trap)(number, pointer, size) };
    if status == i64::from(OK)
       && number == mac_abi::traps::TEXTEDIT
       && size == core::mem::size_of::<mac_abi::textedit::TextEditCall>()
    {
        // The real resident service has completed this SDK-owned request.
        let call = unsafe { &*pointer.cast::<mac_abi::textedit::TextEditCall>() };
        if call.operation == mac_abi::textedit::CREATE
        {
            EDITOR.store(call.id.0 as usize, Ordering::Relaxed);
        }
        if call.operation == mac_abi::textedit::KEY
        {
            KEYS.fetch_add(1, Ordering::Relaxed);
        }
    }
    status
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
    UPDATES.store(0, Ordering::Relaxed);
    GROW_CLIPS.store(0, Ordering::Relaxed);
    for _ in 0..20
    {
        let start = system.events().uptime_ticks().map_err(|e| e.0)?;
        if !editor.event(system,
                         Event { kind: EventKind::KeyDown,
                                 message: u32::from(b'a'),
                                 when: start as u32,
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
        // Service the first idle iteration after every isolated edit. It must
        // not require a second scene commit merely to show the reset caret.
        editor.event(system,
                     Event { kind: EventKind::Null,
                             when: start as u32,
                             ..Default::default() })?;
        let end = system.events().uptime_ticks().map_err(|e| e.0)?;
        edit_ticks += edited - start;
        menu_ticks += end - edited;
    }
    let mut line = Text::<192>::default();
    let _ = writeln!(&mut line,
                     "TYPING_PROFILE chars=20 edit_ticks={} menu_ticks={} file_calls={} \
                      menu_calls={} updates={} grow_clip_calls={}",
                     edit_ticks,
                     menu_ticks,
                     FILES.load(Ordering::Relaxed),
                     MENUS.load(Ordering::Relaxed),
                     UPDATES.load(Ordering::Relaxed),
                     GROW_CLIPS.load(Ordering::Relaxed));
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
    system.log("TYPING_ISOLATED_DONE\n")?;
    keyboard_batch(system, &mut editor)?;
    system.log("TYPING_PROFILE_DONE\n")?;
    Ok(())
}

fn keyboard_batch(system: &System<'_>, editor: &mut editor::Editor) -> Result<(), Status>
{
    let id = textedit::EditorId(EDITOR.load(Ordering::Relaxed) as u32);
    let before = system.textedit().info(id).map_err(|error| error.0)?;
    system.events().flush(events::EVERY_EVENT, 0).map_err(|error| error.0)?;
    FILES.store(0, Ordering::Relaxed);
    MENUS.store(0, Ordering::Relaxed);
    UPDATES.store(0, Ordering::Relaxed);
    KEYS.store(0, Ordering::Relaxed);
    let when = system.events().ticks().map_err(|error| error.0)?;
    for character in b'a'..=b't'
    {
        for kind in [EventKind::KeyDown, EventKind::KeyUp]
        {
            system.events()
                  .post(Event { kind,
                                message: u32::from(character),
                                when,
                                ..Default::default() })
                  .map_err(|error| error.0)?;
        }
    }
    let boundary = Event { kind: EventKind::Application1,
                           message: 0x7abc,
                           ..Default::default() };
    let tail = Event { kind: EventKind::KeyDown,
                       message: u32::from(b'w'),
                       ..Default::default() };
    for event in [boundary, tail]
    {
        system.events().post(event).map_err(|error| error.0)?;
    }
    let first = system.next_event(events::EVERY_EVENT)?;
    batch::keyboard(system, editor, first)?;
    let updates = UPDATES.load(Ordering::Relaxed);
    let key_calls = KEYS.load(Ordering::Relaxed);
    let menu_calls = MENUS.load(Ordering::Relaxed);
    let files = FILES.load(Ordering::Relaxed);
    let info = system.textedit().info(id).map_err(|error| error.0)?;
    let mut bytes = [0; 512];
    let text = system.textedit().text(id, &mut bytes).map_err(|error| error.0)?;
    if info.text_length != before.text_length + 20
       || info.selection_start != info.text_length
       || info.selection_end != info.text_length
       || !text.ends_with("abcdefghijklmnopqrst")
       || updates != 1
       || key_calls != 20
       || menu_calls != 5
       || files != 0
       || system.events().peek(events::EVERY_EVENT).map_err(|error| error.0)? != Some(boundary)
       || system.next_event(events::EVERY_EVENT)? != boundary
       || system.next_event(events::EVERY_EVENT)? != tail
    {
        return Err(BAD_ARGUMENT);
    }
    system.log("TYPING_BATCH chars=20 text_match=1 updates=1 key_calls=20 menu_calls=5 \
                file_calls=0 ordered_boundary=1\n")?;

    // Even a queue consisting only of key releases cannot monopolize the loop.
    let release = Event { kind: EventKind::KeyUp,
                          ..Default::default() };
    for _ in 0..128
    {
        system.events().post(release).map_err(|error| error.0)?;
    }
    batch::keyboard(system, editor, release)?;
    if system.next_event(events::EVERY_EVENT)? != release
       || system.events().peek(events::EVERY_EVENT).map_err(|error| error.0)?.is_some()
       || UPDATES.load(Ordering::Relaxed) != updates
       || KEYS.load(Ordering::Relaxed) != key_calls
       || MENUS.load(Ordering::Relaxed) != menu_calls
    {
        return Err(BAD_ARGUMENT);
    }
    system.log("TYPING_BATCH_BOUND events=129 consumed=128 remaining=1 updates=0 key_calls=0\n")?;

    // Shortcuts are queued MenuCommand events. Commit the preceding text,
    // leave the command for normal dispatch, then use its resulting selection.
    for event in [Event { kind: EventKind::KeyDown,
                          message: u32::from(b'z'),
                          ..Default::default() },
                  Event { kind: EventKind::MenuCommand,
                          message: (129 << 16) | 6,
                          modifiers: events::CMD_KEY,
                          ..Default::default() },
                  Event { kind: EventKind::KeyDown,
                          message: u32::from(b'q'),
                          ..Default::default() }]
    {
        system.events().post(event).map_err(|error| error.0)?;
    }
    let first = system.next_event(events::EVERY_EVENT)?;
    batch::keyboard(system, editor, first)?;
    let command = system.next_event(events::EVERY_EVENT)?;
    let Some(selection) = system.menus().selection(&command).map_err(|error| error.0)?
    else
    {
        return Err(BAD_ARGUMENT);
    };
    if selection.command != menus::SELECT_ALL
       || !editor.command(system, selection.command, command.when)?
    {
        return Err(BAD_ARGUMENT);
    }
    menus::update(system, editor)?;
    let after = system.next_event(events::EVERY_EVENT)?;
    batch::keyboard(system, editor, after)?;
    let mut bytes = [0; 8];
    if system.textedit().text(id, &mut bytes).map_err(|error| error.0)? != "q"
    {
        return Err(BAD_ARGUMENT);
    }
    system.log("TYPING_BATCH_COMMAND ordered=1 selection_applied=1 final_text=q\n")
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

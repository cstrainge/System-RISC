//! Original TextEdit pixel cases rendered through actual resident RV64 traps.
#![no_std]
#![no_main]
use mac_toolbox::{events::EVERY_EVENT, textedit::EditorId, *};
const SAMPLE: &str = "one two three\rfour five six\rseven eight";

fn draw(system: &System<'_>, window: WindowId, editor: EditorId) -> ToolboxResult<()>
{
    system.windows().begin_update(window)?;
    let port = system.quickdraw().graphics(window);
    port.reset()?;
    port.colors(0xffffff, 0xffffff)?;
    port.paint_rect(Rect::new(0, 0, 128, 96))?;
    system.textedit().draw(editor, window)?;
    system.windows().end_update(window)
}
fn state(system: &System<'_>, editor: EditorId, name: u8) -> ToolboxResult<()>
{
    let edit = system.textedit();
    edit.set_rects(editor, Rect::new(6, 4, 86, 90), Rect::new(10, 8, 120, 80))?;
    edit.set_text(editor, SAMPLE)?;
    edit.activate(editor, false)?;
    match name
    {
        b's' =>
        {
            edit.select(editor, 2, 22)?;
            edit.activate(editor, true)?;
        }
        b'c' | b'i' | b'o' =>
        {
            edit.select(editor, 5, 5)?;
            edit.activate(editor, true)?;
            if name != b'c'
            {
                edit.idle(editor, 100)?;
            }
            if name == b'o'
            {
                edit.idle(editor, 130)?;
            }
        }
        b'u' =>
        {
            edit.select(editor, 2, 22)?;
            edit.activate(editor, true)?;
            edit.activate(editor, false)?;
        }
        b'd' =>
        {
            edit.scroll(editor, -5, -16)?;
        }
        _ =>
        {}
    }
    Ok(())
}
fn run(system: &System<'_>) -> ToolboxResult<()>
{
    let window=system.windows().create(windows::WindowSpec::new(Rect::new(100,100,228,196),"TextEdit probe").procedure(8).minimum(128,96))?;
    system.windows().set_grow_icon_clip(window, Some(Rect::ZERO))?;
    system.quickdraw().cursors().hide()?;
    let editor = system.textedit().create(Rect::new(6, 4, 86, 90), Rect::new(10, 8, 120, 80))?;
    state(system, editor, b't')?;
    draw(system, window, editor)?;
    system.log("DESKTOP_READY: resident TextEdit SDK fixture\n").map_err(Error)?;
    system.log("TEXTEDIT_PROBE_TEXT\n").map_err(Error)?;
    loop
    {
        let event = system.events().next_event(EVERY_EVENT)?;
        if event.kind == EventKind::Update && event.message == window.0
        {
            draw(system, window, editor)?;
        }
        if event.kind == EventKind::KeyDown
        {
            let key = event.character();
            if [b't', b's', b'c', b'd', b'i', b'o', b'u'].contains(&key)
            {
                state(system, editor, key)?;
                draw(system, window, editor)?;
                system.log(match key
                           {
                               b's' => "TEXTEDIT_PROBE_SELECTION\n",
                               b'c' => "TEXTEDIT_PROBE_CARET\n",
                               b'd' => "TEXTEDIT_PROBE_SCROLL\n",
                               b'i' => "TEXTEDIT_PROBE_CARET_IDLE_ON\n",
                               b'o' => "TEXTEDIT_PROBE_CARET_IDLE_OFF\n",
                               b'u' => "TEXTEDIT_PROBE_INACTIVE_SELECTION\n",
                               _ => "TEXTEDIT_PROBE_TEXT\n"
                           })
                      .map_err(Error)?;
            }
        }
    }
}
/// # Safety
/// The loader supplies a valid resident service table for this application.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> Status
{
    let system = match unsafe { System::from_raw(services) }
    {
        Ok(s) => s,
        Err(e) => return e
    };
    if let Err(e) = run(&system)
    {
        let _ = system.log("TEXTEDIT_PROBE_FAIL\n");
        return e.0;
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

//! Before/after measurements of the actual app editor over native services.
use super::*;

fn fence(system: &System<'_>, name: &str) -> Result<(), Error>
{
    let mut line = Text::<128>::default();
    writeln!(line, "TEXTEDIT_PERF_READY name={name}").map_err(|_| Error::NoMemory)?;
    system.log(line.as_str())?;
    loop
    {
        let event = system.wait_event(mac_toolbox::events::EventMask::ALL, 30)?;
        if matches!(event, mac_toolbox::Event::KeyDown { .. })
        {
            return Ok(());
        }
    }
}

pub fn run(system: &System<'_>) -> Result<(), Error>
{
    let _menus = menus::install(system)?;
    let mut editor = editor::Editor::with_font(quickdraw::fonts::FontSelection { family: 4,
                                                                                 size:   12,
                                                                                 face:   0 });
    editor.new_document(system, "Text Edit Performance")?;
    let id = textedit::EditorId(EDITOR.load(Ordering::Relaxed) as u64);
    system.quickdraw().cursors().hide()?;
    system.log("DESKTOP_READY: actual Text Edit performance fixture\n")?;
    let mut bytes = [b'x'; 32_000];
    for (index, byte) in bytes.iter_mut().enumerate()
    {
        if index % 64 == 63
        {
            *byte = b'\r';
        }
    }
    for (name, length, operation) in [("small-typing", 128, 0),
                                      ("large-typing", 32_000, 0),
                                      ("small-blink", 128, 1),
                                      ("large-blink", 32_000, 1),
                                      ("visible-cursor-blink", 32_000, 1),
                                      ("large-scrolling", 32_000, 2)]
    {
        system.textedit().set_text(id, core::str::from_utf8(&bytes[..length]).unwrap())?;
        system.textedit().select(id, 0, 0)?;
        system.textedit().scroll_to(id, 0)?;
        editor.draw(system)?;
        if name == "visible-cursor-blink"
        {
            system.quickdraw().cursors().init()?;
        }
        else if name == "large-scrolling"
        {
            system.quickdraw().cursors().hide()?;
        }
        // Flush any deliberate cursor transition before the measurement fence.
        system.wait_event(mac_toolbox::events::EventMask::from_bits_retain(0), 0)?;
        fence(system, name)?;
        UPDATES.store(0, Ordering::Relaxed);
        CONFIGURES.store(0, Ordering::Relaxed);
        LOGS.store(0, Ordering::Relaxed);
        let ticks = system.events().ticks()?;
        let started = system.events().monotonic_nanoseconds()?;
        for index in 0..20
        {
            if operation == 2
            {
                system.textedit().scroll_to(id, index * 16)?;
                editor.draw(system)?;
            }
            else
            {
                editor_event(system,
                             &mut editor,
                             Event { kind: if operation == 0
                                     {
                                         EventKind::KeyDown
                                     }
                                     else
                                     {
                                         EventKind::Null
                                     },
                                     message: if operation == 0 { u32::from(b'z') } else { 0 },
                                     when: ticks.wrapping_add((index + 1) * 30),
                                     ..Default::default() })?;
            }
        }
        let elapsed = system.events().monotonic_nanoseconds()?.saturating_sub(started);
        let logs = LOGS.load(Ordering::Relaxed);
        let mut line = Text::<256>::default();
        writeln!(line,
                 "TEXTEDIT_PERF_DONE name={name} bytes={length} iterations=20 ns={elapsed} \
                  updates={} scrollbar_configures={} logs={logs}",
                 UPDATES.load(Ordering::Relaxed),
                 CONFIGURES.load(Ordering::Relaxed)).map_err(|_| Error::NoMemory)?;
        system.log(line.as_str())?;
        fence(system, "phase-complete")?;
    }
    system.log("TEXTEDIT_PERF_PASS\n")?;
    loop
    {
        system.wait_event(mac_toolbox::events::EventMask::from_bits_retain(0), 30)?;
    }
}

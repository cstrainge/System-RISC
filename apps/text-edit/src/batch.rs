//! Drain consecutive keyboard input before committing the editor's next frame.
use core::fmt::Write;

use mac_toolbox::*;

use crate::{Text, editor};

pub fn is_keyboard(kind: &Event) -> bool
{
    matches!(kind, Event::KeyDown { .. } | Event::KeyUp { .. })
}

pub fn keyboard<'a>(system: &System<'a>, editor: &mut editor::Editor<'a>, first: Event)
                    -> Result<bool, Error>
{
    let mut pending: [Event; 128] = core::array::from_fn(|_| Event::default());
    pending[0] = first;
    let mut count = 1;
    // Consume input before applying edits: layout work for one key must not
    // delay delivering all the other keys already waiting in the OS queue.
    // The fixed capacity bounds work even with a continuously active producer.
    while count < pending.len()
    {
        // An enabled shortcut arrives as MenuCommand. If input was queued
        // before this edit enables its command, it is still KeyDown: leave that
        // boundary queued too. The main loop resolves it against the updated
        // menu policy after applying the preceding text edits.
        match system.events().peek(mac_toolbox::events::EventMask::ALL)?
        {
            Some(next)
                if next.is_keyboard()
                   && !next.input().modifiers.contains(events::Modifiers::COMMAND) =>
            {
                pending[count] = system.next_event(mac_toolbox::events::EventMask::ALL)?;
                count += 1;
            }
            _ => break
        }
    }
    editor.defer_drawing();
    let mut keys = 0;
    for event in pending[..count].iter()
    {
        // Releases only update the OS key map; they require no editor query,
        // menu query or repaint. Mutation order otherwise remains unchanged.
        if !matches!(event, Event::KeyUp { .. })
        {
            editor.event(system, event, PointerPosition::default())?;
            keys += 1;
        }
    }
    editor.flush_drawing(system)?;
    if keys != 0
    {
        let mut message = Text::<64>::default();
        let _ = writeln!(&mut message, "TEXTEDIT_BATCH keys={keys}");
        system.log(message.as_str())?;
    }
    Ok(keys != 0)
}

//! Drain consecutive keyboard input before committing the editor's next frame.
use core::fmt::Write;

use mac_toolbox::*;

use crate::{Text, editor, menus};

pub fn is_keyboard(kind: Event) -> bool
{
    matches!(kind, Event::KeyDown { .. } | Event::KeyUp { .. })
}

pub fn keyboard(system: &System<'_>, editor: &mut editor::Editor, first: Event)
                -> Result<(), Error>
{
    let mut pending = [Event::default(); 128];
    pending[0] = first;
    let mut count = 1;
    // Consume input before applying edits: layout work for one key must not
    // delay delivering all the other keys already waiting in the OS queue.
    // The fixed capacity bounds work even with a continuously active producer.
    while count < pending.len()
    {
        // Command shortcuts already arrive as MenuCommand records from the
        // Event Manager. Leave them queued for the normal main-loop dispatch.
        match system.events().peek(mac_toolbox::events::EventMask::ALL)?
        {
            Some(next) if is_keyboard(next) =>
            {
                pending[count] = system.next_event(mac_toolbox::events::EventMask::ALL)?;
                count += 1;
            }
            _ => break
        }
    }
    editor.defer_drawing();
    let mut keys = 0;
    for event in pending[..count].iter().copied()
    {
        // Releases only update the OS key map; they require no editor query,
        // menu query or repaint. Mutation order otherwise remains unchanged.
        if !matches!(event, Event::KeyUp { .. })
        {
            editor.event(system, event)?;
            keys += 1;
        }
    }
    if keys != 0
    {
        // Menu enabled state belongs to the same final frame as the selection.
        menus::update(system, editor)?;
    }
    editor.flush_drawing(system)?;
    if keys != 0
    {
        let mut message = Text::<64>::default();
        let _ = writeln!(&mut message, "TEXTEDIT_BATCH keys={keys}");
        system.log(message.as_str())?;
    }
    Ok(())
}

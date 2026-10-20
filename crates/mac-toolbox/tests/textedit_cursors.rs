//! Editor cursor policy is independent of keyboard focus and editor contents.
#![cfg(not(target_arch = "riscv64"))]
mod support;

use mac_toolbox::{Error, Rect};

#[test]
fn cursor_policy_does_not_change_keyboard_activation_or_document_state()
{
    support::with_system(|system| {
        let text = system.textedit();
        let bounds = Rect::new(4, 6, 120, 80);
        let editor = text.create(bounds, bounds).unwrap();
        text.set_text(editor, "editable text").unwrap();
        text.select(editor, 2, 8).unwrap();
        for active in [false, true]
        {
            let before = text.activate(editor, active).unwrap();
            for enabled in [false, true, true, false]
            {
                assert_eq!(text.set_cursor_enabled(editor, enabled).unwrap(), before);
                assert_eq!(text.info(editor).unwrap(), before);
                let mut bytes = [0; 32];
                assert_eq!(text.text(editor, &mut bytes).unwrap(), "editable text");
            }
        }
        text.set_cursor_enabled(editor, false).unwrap();
        text.key(editor, 'x').unwrap();
        let after = text.info(editor).unwrap();
        assert_eq!(text.set_cursor_enabled(editor, true).unwrap(), after);
        let mut bytes = [0; 32];
        assert_eq!(text.text(editor, &mut bytes).unwrap(), "edx text");
        text.dispose(editor).unwrap();
    });
}

#[test]
fn disposed_editor_cursor_configuration_cannot_reach_reused_slot()
{
    support::with_system(|system| {
        let text = system.textedit();
        let bounds = Rect::new(0, 0, 100, 60);
        let old = text.create(bounds, bounds).unwrap();
        text.set_cursor_enabled(old, false).unwrap();
        text.dispose(old).unwrap();
        for enabled in [false, true]
        {
            assert_eq!(text.set_cursor_enabled(old, enabled), Err(Error::NotFound));
        }
        let replacement = text.create(bounds, bounds).unwrap();
        assert_ne!(old, replacement);
        text.set_text(replacement, "replacement").unwrap();
        assert_eq!(text.set_cursor_enabled(old, true), Err(Error::NotFound));
        let before = text.info(replacement).unwrap();
        assert_eq!(text.set_cursor_enabled(replacement, true).unwrap(), before);
        text.dispose(replacement).unwrap();
    });
}

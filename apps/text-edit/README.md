# Text Edit

Native plain-text documents through `mac-toolbox`: owned document windows,
System-owned Open, Save As and save-before-closing dialogs, managed scrollbars,
and resource-based File, Edit, View and Windows menus. The Options dialog has a
managed font list, size and display controls, and a default OK button (Return).
The Windows menu selects open documents and marks the active one. New documents default to
Monaco 12. Files retain their original CR, LF or CRLF line endings on save.

Font choices are saved as versioned JSON in
`System Folder/Preferences/Text Edit Preferences`. Preference documents use
Finder type `JSON` and creator `TEDT`; native names need no extension. Text Edit
accepts both `TEXT` and `JSON`, including Finder document delivery, and retains
`JSON` when saving a JSON document. The initial `File Types` preference is also
an extensionless `JSON`/`TEDT` document.

The current document limit is 32,767 bytes and eight open windows. This is a
plain-text editor; font choices affect the view and do not add rich-text data.

Public-SDK coverage lives in `crates/mac-toolbox/tests/text_edit_documents.rs`.
Its QEMU case exercises Finder association, the System save dialog, JSON type
preservation and font preference persistence on a private disk.

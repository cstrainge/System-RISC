# Clipboard viewer test application

This private application compiles the actual `finder/src/clipboard.rs` and uses
only the aggregate Toolbox SDK. It packages the same WIND2002 and STR#2000
resources as Finder. T seeds ASCII `abc` through the real Scrap service; E clears
it. These keys belong only to the test application. The capture runner also boots
actual Finder separately and selects Edit > Show Clipboard through real input.

The fixture provides startup/trace transport. Canonical Rust acceptance lives in
`tests/clipboard.rs`; original pixels and provenance live in `tests/data/clipboard`.
This tests a read-only viewer and resident memory clipboard, not TextEdit, a
second application process or an OS scheduler.

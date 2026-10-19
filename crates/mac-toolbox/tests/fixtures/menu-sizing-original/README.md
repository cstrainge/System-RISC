# Original uncached menu sizing probe

Run from the enclosing RE workspace:

```sh
python3 scripts/retro68.py cmake -S /work/System7/crates/mac-toolbox/tests/fixtures/menu-sizing-original -B /work/build/menu-sizing-reference-68k -DCMAKE_TOOLCHAIN_FILE=/Retro68-build/toolchain/m68k-apple-macos/cmake/retro68.toolchain.cmake
python3 scripts/retro68.py cmake --build /work/build/menu-sizing-reference-68k --parallel 4
python3 System7/scripts/capture_original_menu_sizing.py
```

The collector boots a fresh private q800 clone and executes the installed
System7.6.1 CalcMenuSize. It records outputs and provenance under
`tests/data/menu-sizing`, and verifies that source media hashes are unchanged.
It never connects to the user's visible guest. Rust acceptance belongs in
`tests/menus.rs` and the shared no-std `tests/conformance/menu_sizing.rs`.

A keyless Close Window is115px wide; Command-W makes it139px wide. Native
MENU generation starts with no cached width, exposing the missing shortcut
space which cached original Finder MENU widths previously concealed.

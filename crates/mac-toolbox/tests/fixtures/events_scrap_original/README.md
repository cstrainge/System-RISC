# Original Event and Scrap Manager probes

These programs execute the installed original Mac OS7.6.1 managers. They collect
observations; Rust public Toolbox tests decide acceptance. They are not production
replacements. Never install them on the original disk or the user's active VM.

From the enclosing `rea-work` directory, the retained source can be built with:

```sh
python3 scripts/retro68.py cmake \
  -S /work/System7/crates/mac-toolbox/tests/fixtures/events_scrap_original \
  -B /work/build/events-scrap-retained-68k \
  -DCMAKE_TOOLCHAIN_FILE=/Retro68-build/toolchain/m68k-apple-macos/cmake/retro68.toolchain.cmake
python3 scripts/retro68.py cmake --build /work/build/events-scrap-retained-68k --parallel 4
```

The captures were built from byte-identical source files in the enclosing
`fixtures/events-scrap-reference`. Reproduction transports are:

```sh
python3 scripts/capture_events_scrap_reference.py
python3 scripts/capture_event_input_reference.py
python3 scripts/capture_clipboard_reference.py
python3 scripts/capture_mouse_queue_reference.py
```

Each script owns a fresh raw clone, private Unix QMP socket, and headless q800.
The main and input transports require the initial CMake build above with their
original parent build path `build/events-scrap-reference-68k`, or a corresponding
transport path adjustment. They verify the original disk/ROM hashes before and
after, and terminate their own QEMU. Source, compiler, binary, original System,
Finder, ROM, disk, capture and transport hashes are retained in adjacent test data
provenance JSON files and in the parent `provenance/` directory.

`EventsScrapProbe` captures Scrap operations followed by OS queue actions.
`PPostEvent` invokes A12F with A0=kind, D0=message and returns A0=queue pointer.
This explicit register glue is needed because the installed Retro68 headers
have a declaration without a linked PPostEvent implementation. The system event
mask is accessed at its documented low-memory location. The trap dispatcher
preserves A0 for A02F (`PostEvent`) and returns its changed value for A12F.

The `found` field is meaningful for `*_read`, `get_*`, `peek_*`, and null-read
rows; posting rows instead report `status` and `element`. Error-returned Scrap
offsets are raw observations, not valid offsets. Odd Scrap padding bytes are
not initialized by the manager and must not be compared as payload. Only the
observed change/unchanged property of scrapCount is a compatibility contract:
this installed7.6.1 changes it by8, and Inside Macintosh warns against depending
on the exact increment. Nonstandard posted kinds are investigation observations;
Apple only permits posting mouseDown, mouseUp, keyDown, keyUp, autoKey and diskEvt.

`InputProbe` samples every3ticks in a window whose content origin is(100,100).
The transport drives only its private VM. Guest ticks and host action timestamps
have approximate alignment; use observed states rather than exact sample index.
The tested q800 CapsLock follows the host key-down/up events; this does not prove
that every classic keyboard latches or releases CapsLock in the same way.

`ClipboardEmpty` and `ClipboardSeed` initialize empty scrap and ASCII `abc` TEXT,
then exit so the original Finder's Show Clipboard command can be captured.

`MouseQueueProbe` keeps the physical mouse released while synthesizing queued
mouseDown and mouseUp events. Button and StillDown remainFALSE for every measured
queue state, and neither query removes queued entries. This complements
InputProbe's held physicalmouse test with one synthetic mouseUp.

`captured-main-CMakeLists.txt` and `captured-main-InputProbe.cc.txt` retain the
exact helper-source revisions identified in the original maincapture manifest,
before the later MouseQueue and Clipboard targets and syntheticUp test were
added. The main EventsScrapProbe source and application.r are unchanged.

# Original mono TextEdit observations

`original.json` contains observations collected by the standalone 68k fixture
in `../../fixtures/textedit-original`. The target is the installed original
System 7.6.1, running under the pinned q800/Quadra ROM configuration. The capture
uses an owned headless clone, never the user's active native VM. Provenance pins
the source disk, extracted System and Finder, ROM, toolchain, fixture and capture
transport. Full media and generated binaries remain outside the repository.

Rebuild from the parent RE workspace:

```sh
python3 scripts/retro68.py cmake -S /work/System7/crates/mac-toolbox/tests/fixtures/textedit-original -B /work/build/textedit-original-68k -DCMAKE_TOOLCHAIN_FILE=/Retro68-build/toolchain/m68k-apple-macos/cmake/retro68.toolchain.cmake
python3 scripts/retro68.py cmake --build /work/build/textedit-original-68k --parallel 4
python3 System7/scripts/capture_original_textedit.py
```

These are captured facts, not replacement implementation outputs:

- Chicago 0, size 12, plain, has line height 16 and ascent 12. `TESetText`
  leaves the insertion point at the end. Empty text has zero counted lines;
  the line-start array still contains zero.
- CR ends a line. A final CR does not add a counted empty line. LF and tab
  remain ordinary stored characters in this mono Roman profile, with captured
  advances 10 and 8 respectively. Space wrapping retains whitespace, including
  trailing spaces whose positions can exceed the destination width.
- `TEInsert` inserts before selected text and advances both selection endpoints.
  `TEDelete` removes selected text and does nothing to an insertion point.
  Arrow-key operations on a selection collapse to an edge and then move one
  character. Passing character 127 to `TEKey` inserts that character.
- The recorded `TEGetPoint` position uses the caret's horizontal offset and
  line bottom, not the text drawing baseline. Thus point/offset conversions
  are not strict inverses at vertical line boundaries. Independent hit tests
  include horizontal midpoint ties, outside points and wrapping boundaries.
- Mono cut/copy/paste use the private TextEdit scrap. Replacing system `TEXT`
  does not change that private scrap, and copying text does not publish system
  `TEXT`. Both private and system contents are captured separately.

Each `pixels` record is a complete 128 by 96 one-bit GWorld image: 96 rows,
16 bytes per row, most significant bit first, **1 is black, 0 is white**.
The source pins destination/view rectangles and operation order. The visible
view deliberately clips text at its top and left. Selection, scrolling,
activation and two controlled caret idle transitions have separate records.
For the controlled blink, the fixture advances the edit record's `caretTime`
past the blink interval before calling the original `TEIdle`; it does not
replace the original drawing or idle routine.

The captures cover mono Roman text only. Styled records, script systems,
custom hooks, drag/double-click tracking and invalid pointers are unobserved.
Retro68 declares but does not supply linked `TEToScrap`/`TEFromScrap` glue;
the capture does not claim to have executed those helpers. Their documented
role and the blink/selection/point APIs are described in Apple's
[Inside Macintosh: Text, TextEdit](https://dev.os9.ca/techpubs/mac/pdf/Text/TextEdit.pdf),
pages 2-86, 2-88, 2-99 to 2-100 and 2-108. Native Unicode extensions and explicit
system-clipboard import/export need separate tests and scope statements.

Rust tests under `mac-toolbox/tests` own compatibility acceptance. The Python
script only prepares, captures, transports and checks capture completeness.

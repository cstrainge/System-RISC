# Progress Bar CDEF

`CDEF_PROGRESS_BAR` is System resource `CDEF 5`, named **Progress Bar**.
The independently relocatable resource owns the frame, fill, range validation
and changed-pixel geometry. `PROGRESS_BAR_PROCEDURE` is classic procID 80
(resource 5, variation 0). Monochrome presentation uses `ProgressStyle` and an
explicit native state flag; procID 81 is reserved for the Appearance relevance
bar and is not repurposed. The native UTF-8/state ABI remains separate from a
classic 68k ControlRecord.

Applications use `Controls::create_progress_bar` and the RAII `ProgressBar` owner.
`Progress::Preparing` shows an empty framed track until a denominator is known. `Determinate` accepts checked
64-bit completed/total counts, including both forks. Full-width counters cross the trap boundary without rounding; the CDEF
uses u128 arithmetic for one direct projection onto its actual pixel width. Zero completed work out of a zero total is
complete. A completed count above the total is rejected before changing state.

The resident Control Manager supplies shared generation handles and ownership.
The control never hits or captures input. A repeated SDK value does no trap
work; values mapping to the same CDEF fill edge cause no framebuffer damage.
Changed fill invalidates only the strips between the previous and next edges.
`Working` retains in-flight counts without counting them as completed. The
meter always has a fixed black frame, a pale-blue remainder and a dark-grey
completed prefix. Monochrome uses a white track and black fill. Zero progress
has no minimum marker. Explicit draw and bounds changes repaint only the
control's own rectangle.

The early System boot panel creates this same managed control and calls the
same loaded resource, before the desktop exists. `mac-boot-screen` draws the
panel/background/icon only; it contains no second progress renderer.

Original Finder7.6.1 renderer research is recorded in
`provenance/finder-copy-window.json`. Its private growing-outline/two-range
rendering has not been verified against a live copy operation. The native fixed
track follows the requested System RISC presentation; it is not claimed as a
pixel-identical reproduction of that private renderer.

The native API uses checked full-width counters and bounded geometry; it does
not reproduce the original's defensive250-pixel overflow fallback. Original-runtime
copy scheduling and interaction comparison remains a deferred acceptance gate.

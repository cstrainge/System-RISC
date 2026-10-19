# Original Finder Get Info reference frames

These five PNG captures come from the original Finder 7.5.6 on System Software
7.6, running in a private 640 by 480, eight-bit Q800 VM. They contain visual
reference evidence, not system disks, executables, or ROM images. `provenance.json`
records the original capture paths, source hashes, immutable baseline identity,
and observed window content geometry. The original capture directory was
`results/finder-get-info-reference` outside the repository.

The canonical `get_info_ui.rs` test compares the static field labels, their font
pixels and geometry, comment frame edges, and checkbox rendering for document,
locked document, stationery, empty folder, and populated folder views. Runtime
metadata values differ between the original and generated native fixtures and
are checked through the public File SDK instead. Native filename editing is an
intentional extension and is checked through physical input, stable catalog IDs,
title and browser caption changes, and persisted catalog data.

The capture script only transports input, stores frames and logs, and copies
these references into the report. Rust owns compatibility acceptance. APPL
memory controls, application-specific stationery composites, storage failure
injection in the graphical VM, and complete Get Info coverage remain outside
this test's scope.

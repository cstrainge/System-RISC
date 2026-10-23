# Finder copy window

`copy-window.bin` projects the original Finder7.6.1 `fvew8751` into the native
`cpUI8751` resource. Source hashes, exact child rectangles, item/class IDs, font
policy and all `STR#8750` messages are in
`provenance/finder-copy-window.json` at the repository root.

The big-endian `CpW1` payload is76 bytes: magic4; content width/height u16;
WDEF procedure i16; initial left i16; font family/size i16; face u16; initial top i16;
six top,left,bottom,right rectangles (count label, count, operation, filename,
progress, Stop); UTF-8 length u8 plus the Stop caption; remaining bytes zero.
This native projection is deliberately separate from the original custom view
format. Missing/invalid resources fail; the app has no hardcoded layout fallback.

The original content size is280x76, WDEF procedure5. The progress rectangle is
(10,55)-(198,66); the Stop button is(211,51)-(270,71). Font policy is Chicago12.
The private original runtime capture in results/finder-copy-live confirms a
Copy title, content origin (180,96) at 640x480, a black one-pixel bar frame,
#dadaff remaining pixels and #666666 completed pixels. Escape removes the
partial duplicate. Titles live in STR#8752; the native transfer retains its
resident undo ledger until cancellation acknowledges.

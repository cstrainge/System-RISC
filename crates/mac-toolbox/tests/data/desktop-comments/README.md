# Original System7.6.1 Desktop DB comment reference

`original-desktop-db.bin` is the98,304-byte Desktop DB data fork extracted read-only
from the private original System7.6.1 fixture volume after Finder comment edits.
SHA256: `eaa0213d68a4b0f57536bafb823efb891f8997ef8667674d8c070882e491aa6b`.

The associated JSON records the captured B-tree header, all2,318 walked leaf
records, two type3/CNID Pascal MacRoman comment records, source file identity and
limits. Original extraction and runtime screenshots are retained outside the
repository in `results/finder-get-info-reference`.

Canonical `file_mutations.rs` embeds this exact original fork and changes only
the two documented type3 key CNIDs460/463 to the matching real folder/file CNIDs
of its disposable generated HFS disk. Original key/value bytes for all2,316
unrelated leaf records and their relative order stay unchanged. After native
comment replace/insert/delete through the public SDK, Rust compares every opaque
record and the128-byte B-tree user record exactly. Independent remounts verify
both the generated HFS container and resulting comments.

This establishes the captured Desktop DB comment storage contract. It does not
claim support for every Desktop Manager selector, Desktop DF contents, the older
FCMT fallback, or arbitrary Desktop DB fork growth.

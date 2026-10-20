# Scrap Manager foundation

This is a shared, memory-backed clipboard service. `toolbox-scrap` contains only
safe no-std transport bindings and is re-exported by `mac-toolbox`; the resident
manager owns every payload. Kernel `TryState` serializes requests from future
application threads and reports `SERVICE_BUSY` on contention. No caller pointer
is retained. Host OS-thread tests exercise the same holder and manager; they do
not establish an RV64 scheduler or protected process address spaces.

## Captured compatibility subset

The pinned English/Roman System7.6.1 probe and hashes are retained under
`crates/mac-toolbox/tests/data/scrap/`. Canonical assertions in
`crates/mac-toolbox/tests/conformance/scrap.rs` use only the public SDK and run
unchanged on the host and native RV64 fixture. The capture includes19 ordered
observations;18 operations after the initial reference-only startup snapshot are
mapped into the native test.

- Clear removes all records and changes the clipboard generation.
- Put appends a record, including duplicate kinds and zero-length payloads.
- Reads select the first matching kind. An appended duplicate is still visible
  through native ordered enumeration, but never replaces that first occurrence.
- Logical record size includes an eight-byte header and an even-padded payload.
  Odd padding bytes are unspecified and are not part of the returned payload.
- A missing flavor reports the original `noTypeErr` (`-102`) and preserves the
  caller's output storage. An error does not give a meaningful original offset.
- Puts and reads leave the generation unchanged. The installed reference count
  advances by8 on clear, whereas this native profile advances a wrapping u16 by1.
  Inside Macintosh promises only that the count changes; tests compare changes
  and stability, never those undocumented increment values. The reference's
  positive scrap-state counter and master-pointer value are not native fields.

The native System starts with an initialized empty clipboard and generation0.
This is a bootstrap policy, not a claim about arbitrary original low-memory
startup state. The service has no public uninitialized or disk-backed state.

[Inside Macintosh: Scrap information record](https://dev.os9.ca/techpubs/mac/MoreToolbox/MoreToolbox-124.html),
[ZeroScrap](https://dev.os9.ca/techpubs/mac/MoreToolbox/MoreToolbox-130.html),
[PutScrap](https://dev.os9.ca/techpubs/mac/MoreToolbox/MoreToolbox-131.html), and
[GetScrap](https://dev.os9.ca/techpubs/mac/MoreToolbox/MoreToolbox-133.html).

## Native text and binary representations

`put_text(&str)` and `text(&mut [u8]) -> &str` preserve full native UTF-8,
including embeddedNUL and CR. The latter takes caller-owned scratch storage and
returns a checked Rust string; it fails instead of truncating a code point or
partial string. Native `TEXT` lengths and logical offsets count UTF-8 bytes.
ASCII compatibility fixtures therefore have identical native/original offsets;
non-ASCII original MacRoman byte counts are deliberately not claimed identical.

Generic binary `put` and `read` reject `TEXT`, so a Rust application cannot bypass
the string interface with a MacRoman buffer. Other four-byte kinds retain opaque
binary payloads, including NUL and invalid UTF-8. Native partial binary reads and
zero-based record enumeration are explicit extensions. Any future text-aware
format must receive a deliberate string representation before it is exposed as
human text; storing an opaque flavor does not implement its parser or semantics.

`PSTRING_SCRAP` is a paired convenience trap for the native text operations only.
The trap boundary converts Pascal/MacRoman input to resident UTF-8 and reverses
that conversion on output. Malformed input, unrepresentable characters and text
over255 MacRoman bytes fail without modifying output. Rust bindings expose only
native strings. This paired convenience trap is **not** the original A-line
`PutScrap(TEXT)` convention: that historical interface takes counted raw MacRoman
text, can exceed255bytes, and needs its own adapter. No original Scrap A-line
adapter is claimed in this slice.

## Bounds, failure and unfinished contracts

The native profile bounds the logical clipboard at1MiB and256 records. New bytes
and record capacity are reserved fallibly before the logical mutation commits;
allocation or limit failure preserves all previous records, generation and size.
Clear releases payload storage. No allocation, buffer or pointer escapes a call.
The service returns `NO_MEMORY` for native bound failures rather than pretending
to reproduce the original heap topology or disk error conditions.

`LoadScrap` and `UnloadScrap` return `NOT_SUPPORTED` and preserve memory state.
General file/fork writes and a recoverable persistent clipboard store are still
prerequisites; no simulated successful Clipboard/Scrap.dat file exists. Translation
Manager conversions, private/application scrap exchange, suspend/resume ownership,
TextEdit style-scrap conversion and dialog editing integration remain open. The
service does not translate arbitrary `styl` offsets alongside native Unicode.

## Planned host clipboard extension

[P18](../../../../docs/HOST-CLIPBOARD.md) adds optional bidirectional host sharing
after the HFS-B and initial storage-driver validation sequence. A separate kernel
extension negotiates a compatible host bridge and uses typed resident Scrap
subscriptions/snapshots; application APIs stay unchanged. The existing generation
is not a complete change notification because puts do not advance it. Plan an
internal mutation revision, consistent clear/put export batching and origin/session
tokens without changing captured compatibility semantics. Host I/O occurs outside
Scrap/desktop locks; absent or disconnected bridges preserve local operation.

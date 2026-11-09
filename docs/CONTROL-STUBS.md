# Native control stubs

The Control Suite application's **Control Stubs...** command creates all 23
remaining suite families as real resident controls and loaded CDEF resources.
Their default appearance is a theme-coloured rectangle and X. They advertise
no interactive hit; behavioral messages are deliberately ignored. They are
starting points for implementing control behavior, not finished widgets.

The existing Button, Scroll Bar, Progress Bar, Static Text, Table and Splitter
implementations remain real controls. Drop Menu uses classic CDEF 25/procedure
400; its fixed-width variation 401 is the functional location menu used by
Standard File Open and Save. CDEF 16 is the classic User Pane, not Popup Button.
Text Edit uses CDEF 17, variations 0 (text), 1 (view), and 2 (password).

## Properties and layout

`mac_control_model` contains the single field inventory, explicit defaults,
borrowed UTF-8/collection views, validation and codec. Defaults marked
`placeholder` are implementation choices, not prescribed product defaults.
`mac_control_state` owns fallibly allocated encoded values. The public
`controls::suite` constructors and named getters/setters access canonical CTRL
state; application wrappers do not maintain a second state cache. Compound
changes can be prepared with `Properties::from_view` and installed atomically.
Malformed updates preserve the previous record. Source and child keys in these
stubs are stored metadata, never authority to invoke a provider or use another
object. Implementing provider behavior requires real owner admission.

Bounds/enabled state use the existing Control Manager interface. Minimum and
optional maximum dimensions use the actual signed-16-bit reflow extent range.
AskSize reports the declared minimum and clamps creation preference to the
maximum. Parent sizing/scrolling policy is still the caller's layout policy.
All property records can be encoded independently for a future dialog-resource
format; this change does not add a general declarative-dialog interpreter.

## Requests, events and highlighting

`SuiteRef::send_message` encodes a borrowed typed request and invokes the same
retained CDEF through the canonical control owner/account and callback machinery.
The stubs accept these requests without changing properties. They do not perform
text replacement, undo, provider reads or highlighting.

Control notifications are typed `Event::Control` values in the ordinary public
event model. Text changes carry explicit transaction identity and ordered
UTF-8 byte-range deltas; selection changes carry anchor/caret byte offsets.
`TextDelta::validate_against` checks the actual original text boundaries.
Stubs emit **no** notifications. A real editor must reserve owned transaction
payload and queue admission before mutation; the existing fixed 128-record,
drop-oldest native queue cannot establish lossless text-change delivery. No
alternate queue, callback registry or pretend native producer is introduced.
The raw fixed-record conversion is now fallible rather than dropping a typed
notification's payload. Unknown existing native codes still decode as Other.

The public windowed `Highlighter`, `SpanQuery`, `Span` and `HighlightError`
interfaces define bounded borrowed inputs and typed answer validation. No
highlighter registry/module execution or full editor behavior is implemented
by these stubs. The Text Edit application migration and large-document editor
implementation remain separate work.

## Rebuild and evidence

The unreleased native ABI remains version 1. The control schema, copied CDEF
property/message payloads and callback argument contract change its layout
fingerprint, requiring a paired System/SDK/application/driver/definition rebuild
and VM restart. Do not mix packages from older builds.

Compilation and format checks establish only their stated static properties.
Native observations must identify the matching image, SDK, selected resource
forks and configuration; Linux/QEMU observations do not qualify Windows or
other hardware, SMP, pressure, arbitrary malformed DMA or future widget guts.

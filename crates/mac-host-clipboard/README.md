# Guest host-clipboard protocol engine

This `no_std`, safe Rust crate implements the guest protocol state for the
standard QEMU SPICE vdagent clipboard channel. It consumes borrowed frames from
`mac-vdagent-protocol` and produces owned messages using that codec. It has no
Scrap implementation, resident registration authority, worker, driver, host
clipboard implementation or preferences store. These remain integration gates.
It does provide validated immutable preference policy consumed by the engine.

The caller provides an immutable `Snapshot` pin with a monotonically increasing
Scrap content revision. The engine retains that exact owner rather than copying
its text to announce a guest offer. Native clear/put batching belongs to the
resident subscription boundary: call `observe` only with the stable snapshot
chosen by a bounded batch commit rule. A new session observes the initial local
snapshot without publishing it; subsequent local changes are offered on demand.
Applications continue to use the ordinary Scrap service.

Keep one engine instance for the lifetime of its channel callbacks. Its epochs
are nonreusable within that instance, not globally unique resident IDs. Replacing
the engine requires revoking/draining the old callback context through the
resident lifetime and grant machinery; labels from different engines are never
interchangeable authorization.

## Channel pump contract

1. Reset the real byte transport and replace its receive `Decoder`, then call
   `open_after_transport_reset`. An engine epoch alone cannot flush old device
   bytes. The engine sends exactly one capability announcement with request=1
   per connection. QEMU independently queues an initial host request=1; that
   crossed request is already answered by the immutable guest advertisement,
   so the engine does not send a second announcement. Clipboard traffic becomes
   ready only after the guest advertisement is fully accepted and the host's
   request=0 reply is received. `Effect::Negotiated` or `Sent::Negotiated` marks
   whichever event completes that exchange. It negotiates by-demand text,
   selection prefixes and grab serials. Host serial ties win. Decreasing serials
   are stale; guest serial exhaustion requires a reset rather than reuse.
2. For each complete codec frame, call `receive` with the current `Epoch`. Keep
   that frame and decoder storage pinned while retrying `QueueFull`,
   `ResponsePending` or allocation refusal (`Error::retry_frame`). Drain output and retry the same
   frame; do not feed later input past a refused frame. Codec framing errors
   call `malformed`, then require a real channel reset. Closed/stale-session
   callbacks never alter the local content pin.
3. An `Import` effect borrows decoded UTF-8. Outside engine/device locks, validate
   the real resident write grant and atomically compare its expected Scrap
   revision before replacing text. Call `complete_import` with the effect and
   the actual committed immutable pin, or the original typed failure. Completion
   verifies both a newer revision and exact committed text. A matching later
   `observe` does not echo the import as a guest copy. A new local revision
   invalidates a pending import. Protocol labels confer no write authority.
4. `output` returns three borrowed scatter slices and an opaque position token.
   Keep the engine owner pinned while writing them. Report exactly the accepted
   byte count with `accepted`; zero/partial acceptance retains all queued bytes.
   Stale or excessive acknowledgements fail without advancing. A device failure
   with ambiguous acceptance requires a real reset, never retransmission guessed
   from a status code. `Sent::Fetch` is the point to arm the caller's deadline.
5. Call `poll` after output drains or a stale response is consumed, and after
   retryable local-offer allocation refusal. It performs no work when unchanged.
   On disconnect, disable or cancellation, call `close` and revoke the separate
   resident registration. Reconnect with a new actual channel session.

This one-advertisement QEMU profile is deliberate. In
[QEMU v10.1.0's capability handler](https://github.com/qemu/qemu/blob/v10.1.0/ui/vdagent.c#L652-L679),
every received guest announcement resets clipboard serials. Once its clipboard
peer is registered, another announcement causes the
[serial-reset callback](https://github.com/qemu/qemu/blob/v10.1.0/ui/vdagent.c#L428-L435)
to close the channel. The same reset behavior was present in upstream master
when inspected on 2026-10-27. Unchanged subsequent peer requests/replies are
ignored without output, allocation or serial/fetch reset. A change in negotiated
capabilities, including between the crossed request and its reply, requires a
real channel reset and a fresh exchange; the engine never changes live layouts
or invents a fallback handshake. The caller must also bound the initial handshake
wait; an absent compatible request=0 reply leaves sharing inactive.

`observe` consumes a newer snapshot even when export fails: the real local copy
already happened, so its revision must invalidate an older host fetch. `poll`
retries a refused offer without taking another snapshot. The engine never edits
Scrap directly. A partial outbound text response or guest ownership message
invalidated by a newer copy/host offer forces a channel reset; unsent obsolete
guest output is removed. Use the same `Config::limits` for the receive decoder.

## Ordering and bounds

SPICE REQUEST and DATA carry no usable request ID. The engine permits exactly
one host text fetch at a time. A newer local copy, host offer or release marks an
outstanding fetch stale, but retains its tombstone until its DATA is drained.
Only then can the latest host offer start another request. A timed-out tombstone
requires a channel reset before any retry. Unsolicited or premature DATA also
requires reset. This relies on the standard peer returning at most one response
per request; the external format cannot distinguish malicious duplicate DATA
from a response to a later request.

Four inline queue slots own bounded messages, with at most one queued text
response. Other slots contain small announcements, requests, grabs or releases;
there is no dynamic queue allocation. Receive storage remains in the separately
bounded codec decoder. Local pins retain their resident owner. Text is copied
once into a queued wire response only when the host requests it. Partial writes
use cached headers and borrowed slices, without rebuilding/copying payloads.
Text policy admission is cached for each immutable local revision, so output
retry and repeated host demand do not rescan the snapshot for NUL/size policy.

The local text bound includes the codec's complete-message framing budget.
UTF-8 text of length zero is real text; NONE means unavailable and never clears
Scrap. Unsupported offers likewise preserve every local flavor. Embedded NUL
is rejected in both directions with its byte position because native clipboard
consumers may truncate at NUL. CR, LF and CRLF are preserved exactly: the engine
does not advertise line-conversion capabilities, normalize newlines, append a
terminator or substitute invalid Unicode. Actual host-native UTF-16 conversion
and QEMU's zero-byte clipboard behavior still need qualification on each claimed
host profile; these tests do not establish that interoperability.

## Preference policy

`Settings` accepts one bounded version 1 JSON document, for example:

```json
{"version":1,"enabled":true,"direction":"both"}
```

`version` is required and must be integer 1. `enabled` defaults to true and
`direction` defaults to `both`; other directions are `host-to-guest` and
`guest-to-host`. Unknown or duplicate fields, unsupported versions, malformed
types, trailing values and documents above `MAX_SETTINGS_BYTES` (256 bytes) are
refused with an actionable cause. A private serde wire type prevents bypassing
validation through a public `Deserialize` implementation. Decode and encode do
not allocate; a short encode buffer is left untouched for a real retry.

Select settings with `Config::with_settings` before creating an engine. There
is no live engine policy setter. Replacing settings requires closing the old
engine, revoking/disconnecting its real resident session and callbacks, resetting
the actual transport and opening a fresh authenticated session/engine. Creating
a new Settings value confers no resident permission and changes no existing
session. Numeric engine labels cannot substitute for that revocation.

| Policy | Host-to-guest behavior | Guest-to-host behavior |
| --- | --- | --- |
| Disabled | Open is refused before any allocation or traffic. | Local revisions are observed without inspecting text. |
| Both | Atomic import effects use the existing ordering and CAS rules. | Local text is offered and transferred on demand. |
| Host-to-guest | Imports remain enabled; local changes still invalidate stale imports. | No local text inspection or export; peer requests receive bounded NONE responses. |
| Guest-to-host | Offers update serial/ownership metadata but start no fetch; DATA is refused before text scanning. | Local text export remains enabled and uses the current host serial order. |

Direction refusal and unchanged idle work allocate nothing. A NONE response may
need a small bounded wire allocation; refusal preserves the receive frame for
retry and never falls through to reading private local text. Policy controls
bridge traffic, while the resident service separately authenticates every real
call and its session's read/write permission.

These methods encode policy data only. They do not load/save a Preferences file,
commit a transaction, claim writable storage, fabricate an application identity
or report persistence success. Native preference service/storage integration
must separately use the real authenticated writable transaction boundary.

## Checks

`cargo test --locked -p mac-host-clipboard` exercises the real codec with fragmented
input, crossed/coalesced capability announcements and partial write completion,
demand exchange, empty/NONE distinctions, limits, malformed Unicode,
stale offers/data/completions, serial ordering, queue bounds, partial writes,
timeouts, reconnect, generic CAS causes, strict preference JSON and applied
direction/enablement policy, and real allocation refusal/retry.
No test claims authenticated resident integration, scheduling, or a real host
clipboard round trip.

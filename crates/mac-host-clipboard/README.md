# Guest host-clipboard protocol engine

This `no_std`, safe Rust crate implements the guest protocol state for the
standard QEMU SPICE vdagent clipboard channel. It consumes borrowed frames from
`mac-vdagent-protocol` and produces owned messages using that codec. It has no
Scrap implementation, resident registration authority, worker, driver, host
clipboard implementation or preferences store. These remain integration gates.

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
   bytes. The engine sends a capability request and admits clipboard traffic
   only after a compatible peer announcement. It negotiates by-demand text,
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

## Checks

`cargo test --locked -p mac-host-clipboard` exercises the real codec with fragmented
input, demand exchange, empty/NONE distinctions, limits, malformed Unicode,
stale offers/data/completions, serial ordering, queue bounds, partial writes,
timeouts, reconnect, generic CAS causes and real allocation refusal/retry.
No test claims authenticated resident integration, scheduling, or a real host
clipboard round trip.

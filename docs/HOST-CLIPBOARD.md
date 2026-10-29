# Virtual host clipboard extension

Status: required System-extension proving milestone, tracked as
[P18](CORE-TOOLBOX-ROADMAP.md#p18--virtual-host-clipboard-extension). The shared
named-port adapter, VBYT byte-channel contract, SPICE vdagent codec, startup
outcome strip and authenticated resident SCEX Scrap observation/import service
are present.
They do not yet make an active clipboard bridge. Module-owned worker execution,
account/invocation authority, worker delivery/batching and complete guest bridge
activation remain gates. This is the next real consumer of the
[P6 extension SDK](SYSTEM-EXTENSIONS.md), proving typed hooks beyond storage.
The memory/model module-domain integration stage is incomplete with driver-only
or protocol-only evidence.

## Guest-only QEMU architecture

Implement Host Clipboard entirely in guest Rust, using QEMU's standard
`qemu-vdagent` chardev and its host clipboard integration. The guest uses the
standard SPICE vdagent protocol on `com.redhat.spice.0`. Do not introduce a custom
host companion, private clipboard encoding or filesystem bridge. Linux, native
Windows and macOS QEMU host profiles each need real interoperability evidence;
the presence of an option or a compiled guest package does not qualify a host.

The service chain is:

1. The resident VDEV service provides declared device inventory, claims and DMA
   lifetime management to an external driver.
2. `VDAgent Channel`, installed in `System Folder/Drivers`, uses
   `mac-virtio-console` to admit the exact named port. It exposes the VBYT v1
   byte-channel interface, with bounded status/read/write/error-text operations.
3. `Host Clipboard`, installed in `System Folder/Extensions`, resolves a VBYT v1
   provider through declared dependencies, negotiates the standard vdagent
   clipboard capabilities, and owns bridge policy and asynchronous transfers.
4. Resident typed Scrap services validate the extension's owner/session grant
   and commit clipboard mutations. Applications continue to use the public
   Toolbox Scrap APIs.

Neither System nor the extension selects a provider by its driver filename.
The current admitted transport profile requires a dedicated
`virtio-serial-device,max_ports=2` controller with `virtserialport,nr=1` for
`com.redhat.spice.0`. Host filesystem transport uses its own dedicated controller
and different port name. General multiport sharing is not implemented. A wrong
name/profile, absent endpoint, closed host port or incomplete guest-open
publication leaves byte I/O unavailable. See the
[transport contract](../crates/mac-virtio-console/README.md) and
[VBYT contract](../crates/mac-byte-channel/README.md).

The channel has a checked connection generation. Close, removal or generation
change invalidates every partial message, offered format, pending transfer and
origin token associated with the old session. Bounded discovery belongs to
scheduled work; boot initialization and input handling must never wait in a
loop for a host clipboard or capabilities response.

## Host detection and packaging

Build a separately packaged kernel extension with the native INIT/module entry,
generated contract metadata, relocatable resources and owned service/hook
registrations. Its `resources.json` declares file type `INIT` and
`extension_target: "system"`; the package contains matching `RV64/0` and
`EABI/0`, conventionally named `System`. Resource names are optional metadata;
part 0 selects resident execution. There is no Finder part unless a separate
Finder-owned feature and lifetime are implemented. The underlying host-channel
device provider uses the distinct DRVR/DABI package and entry contract.
Install the clipboard extension in `System Folder/Extensions` and any
separately packaged host-channel driver in `System Folder/Drivers`, both under
the selected blessed installation. Avoid compiling host clipboard policy into
each app or into the Scrap Manager. Keep the local Scrap implementation available
without the extension.

Supply the extension's own resource-based icon family and display it in the
System-owned classic startup icon strip. This is part of the extension proving
milestone, including the standard extension-icon fallback when artwork is absent;
see [extension icon presentation](SYSTEM-EXTENSIONS.md#extension-icons-and-startup-presentation).

Probe for an advertised virtual-host integration endpoint and negotiate its
clipboard protocol/version and supported formats. A virtualization indicator
alone is insufficient: sharing becomes active only with a working compatible
bridge. Physical hardware, an absent host endpoint, disabled sharing or an
unsupported version leaves the guest clipboard fully functional locally.
Discovery is bounded, with explicit unavailable/disconnected status and retry
on endpoint changes rather than on each input event.

Graphical desktop launches automatically supply the standard QEMU endpoint when
that QEMU advertises its required facilities. No enable flag is needed. Use
`python scripts/system7_vm.py start --no-clipboard` to omit the endpoint while
keeping the installed extension present: this exercises its actual unavailable
facility refusal and startup X. Headless launches supply no graphical clipboard
peer. Missing host facilities leave local Scrap functional and report the reason;
launcher configuration alone never reports sharing as active. The System JSON
enablement/direction preference remains the persistent guest policy.

For GTK, inspect the selected executable's QMP display schema before supplying
`clipboard=on` on both the GTK display and `qemu-vdagent` chardev. The GTK display
must declare a boolean clipboard member; a chardev-help listing alone does not
establish that GTK supplies a clipboard peer. The launcher performs this query
using a temporary CPU-less, display-less QEMU with an owned loopback monitor and
reaps it before launch. Probe failures retain their actual cause. Explicit opt-out
and headless launches skip clipboard probes. Cocoa uses its separate native peer
profile and still requires its own text qualification.

Qualify the standard QEMU configuration separately on each claimed host profile.
Detecting a VM, opening a port or passing mock-channel tests is not end-to-end
sharing. No network stack or filesystem container participates in the clipboard
data path.

## Worker admission and refusal

Bootstrap must obtain a real module-owned, OS-scheduled worker and its admitted
stack, invocation identity, allocation account and cancellation/wakeup authority.
The worker must be eligible for bounded asynchronous channel and Scrap work
outside the globally interrupt-masked application trap path. A boolean worker
flag, borrowed application identity or manually constructed account token does
not satisfy this gate. The current 96-byte module `Host` descriptor provides
the checked worker callbacks used by `mac-driver-alloc::Workers`. Native worker
execution, cancellation, original-Subject I/O and actual reclamation have the
bounded observations recorded in [THREADING.md](../THREADING.md). Obtain genuine
worker admission through that API; descriptor size or callback presence alone
is not admission or production clipboard readiness.
Do not publish an always-successful placeholder initializer or package inert
callbacks as a working bridge.

Register required hooks/services only after admission and compatible protocol
negotiation, with tracked rollback for every provisional lifetime. Missing VBYT,
unsupported protocol, worker/account refusal or inability to admit retained
transfer storage fails activation. System retains the package icon or theme
fallback, paints the refusal X and records a named cause with an actionable next
step. For example, an unavailable QEMU endpoint should identify
`com.redhat.spice.0` and the required controller profile; an unavailable worker
must identify the System worker prerequisite. Required hooks/services remain
unpublished and local Scrap stays usable. Transport/protocol readiness does not
stand in for worker/account admission.

Bridge activation is confirmed only after a scheduled readiness transition; a boot
initializer must not spin awaiting the host. The resident startup/lifecycle
service must retain the provisional outcome until that transition and distinguish
it from a confirmed active extension. Until that service is implemented, the
extension must refuse bridge activation rather than assert that sharing is
running. Current `MODULE_READY` reports synchronous initializer commitment and
armed workers becoming runnable; it does not report asynchronous negotiation
success. The worker foundation does not implement that later readiness surface.
A failed cleanup keeps its exact ticket and backing quarantined; report
recovery honestly instead of pretending rollback succeeded.

## Wire limits and text policy

`mac-vdagent-protocol` implements the standard little-endian chunk/message
layouts, bounded partial decoding and typed outbound messages. The chunk payload
ceiling is 2048 bytes. The local complete-message ceiling is 1 MiB, including
the 20-byte message header, format word and negotiated selection prefix; text
admission must also respect the resident Scrap payload limit. This is a local
limit, not a claim that MAX_CLIPBOARD was negotiated. See the
[codec contract and external provenance](../crates/mac-vdagent-protocol/README.md).

Begin with the clipboard selection and UTF-8 plain text. Negotiate by-demand
clipboard support and use selection/grab-serial fields only when supported.
GRAB serials do not appear on REQUEST or DATA and are not internal object IDs or
request tickets. Keep at most one fetch in flight per selection/type. If a
fetch becomes stale, drain its result before reusing that state; after an
ambiguous timeout, reset the connection before issuing another fetch. Merely
comparing a locally invented request counter cannot distinguish delayed DATA.

Preserve valid UTF-8 bytes and newline content in the guest codec. Host native
UTF-16 conversion and platform newline handling belong to QEMU's native
integration and need explicit per-host qualification. Do not advertise conversion
capabilities that were not negotiated. Empty UTF-8 text, an empty Scrap, an
unavailable text format and an unsupported offer remain distinct. In particular,
zero-byte codec success does not prove that the host's native clipboard adapter
preserves empty text. Images, rich text and file lists are separate future
format contracts.

## Resident Scrap integration

Applications keep using the public Toolbox clipboard APIs. Native Rust text is
UTF-8; legacy counted-MacRoman/Pascal conversions stay at their existing adapter
boundaries. Begin with bidirectional Unicode plain text, including empty text,
multiline text and emoji. Define host UTF-16/UTF-8 and newline conversion rules
explicitly; conversion/size errors preserve the current clipboard and report a
status. Images, styled text, file lists and other flavors need separate negotiated
contracts; do not claim lossless transfer of arbitrary Scrap records as text.

The resident SCEX v1 service provides typed observation sessions, retained
snapshot pins and compare/import publication. Each entry authenticates a real
Registry-issued module owner: it must be a Ready INIT extension whose declared
service requirements resolve SCEX v1 to System. An application ID, driver owner,
initializing module or unlisted service requirement cannot grant this access.
Quiescing closes the exact module ledger, revoking even previously obtained
resident authentication. No fabricated process or worker identity participates.

Sessions are explicit read-only or writable grants. The central registered
`SCRS`/`SCRB` stores own sessions and backing; `SRCP`/`SRPB` own snapshot pins and
backing. Every session operation checks module ownership. A pin additionally
belongs to exactly one session, so another connection of the same module cannot
reuse it. The shared object facility issues all identities and preserves
generation history; neither wire handles nor revision numbers grant authority.

Preserve ordinary native/legacy Scrap semantics. The resident manager has a
checked internal change revision for successful clear/put/replace mutations;
the public compatibility count still changes on clear and cannot detect puts.
Immutable text snapshots pin one committed payload without copying it, and
compare-and-replace rejects an outdated revision before allocating or mutating.
The service records one latest revision after commit and each session retains
its last acknowledged revision. This notification update is constant-time and
infallible: it walks no sessions, allocates no payload or queue entry, and invokes
no extension code. NOTICE reads the pending revision without consuming
it. Successful snapshot admission acknowledges it; allocation refusal retains
the notification. Concurrently retained pins keep their own immutable content
across subsequent Scrap commits and bounded partial reads.

SCEX uses checked little-endian requests for OPEN, NOTICE, SNAPSHOT, INFO, READ,
IMPORT, CLOSE_PIN and DISCONNECT. A reply has an eight-byte status/length header;
terminal failures include the informative UTF-8 resident cause. Callers provide
512–4096 reply bytes, validated before any effect, so a session or pin is never
published without room to return its identity. READ carries at most 4088 payload
bytes per wire reply. IMPORT permits at most `1 MiB - 32` text bytes and must also
fit the resident Scrap text limit. The
[wire contract](../crates/mac-scrap-extension-interface/src/lib.rs) defines the
exact layouts. Snapshot and metadata requests are separate; INFO or a later
read failure cannot hide an already minted pin from its caller.

This service supplies coalesced wake state, not a scheduled observer thread.
The worker must consume it at a documented bounded delivery boundary. Commits
before that boundary collapse to the latest snapshot. A clear/put producer has
no explicit end marker, so an observation made between its operations may see a
valid intermediate empty Scrap. Define and qualify final-empty deferral through
the real scheduler before exporting such a sequence; do not call that solved by
reading the revision twice in a tight loop. No snapshot may mix payload bytes
from different revisions.

Tag bridge transfers with origin/session and revision tokens to suppress echoes.
IMPORT requires a writable active session and its own live snapshot pin. It
compares the exact captured revision before replacing text. A delayed import
cannot overwrite a newer local commit. After successful commit, only the origin
session acknowledges that imported revision; other observers still receive the
change. An unsupported host offer must not call text IMPORT or clear unrelated
guest clipboard flavors. Disconnect removes the session and every pin it owns;
late reads/imports cannot reauthorize that connection, and reopening gets a new
central identity. Native transfer cancellation and stale-response draining must
complete before reusing the external protocol state.

Perform host I/O and conversion outside Scrap/desktop locks and UI/input paths.
Use bounded queues/payloads and asynchronous work with cancellation/timeouts;
take only the scheduler prerequisites needed for that context. Cache offers and
transfer on changes or demand, never per-event polling or whole-clipboard copies
on mouse motion. Notify after commit without calling extension code under Scrap
locks. Registration/request IDs use the shared generation-tagged facility with
separate ownership checks. Disconnect/reset revokes subscriptions and rejects
late replies from the previous connection before reuse.

The ordinary owner walk reclaims stopped extension sessions, pins and backing
through the same registered release catalog. A missing cleanup context retains
the unique invalidated ticket and typed cause for quarantine/retry; stopping an
owner does not bypass cleanup authentication or free a retained pin by decree.
The host service fixture verifies real INIT admission, public SDK mutations,
coalesced notifications, stable pins, CAS refusal, origin suppression,
disconnect/reopen and exact-ticket cleanup retry. It does not execute a guest
worker or establish native interrupt, account or arbitrary privileged-fault
containment.

`mac-driver-alloc::scrap` supplies non-copyable session and snapshot guards over
the original resolved SCEX provider. It reserves each retirement record before
OPEN or SNAPSHOT can have an effect. Guard destruction only marks that admitted
record; a module-lifetime controller performs bounded cleanup later. Child pins
keep their session connected until their releases are acknowledged. Each drain
visits at most its supplied record budget, with constant-time record removal;
Busy preserves the same provider and grant for retry. A malformed post-effect
creation reply retains its uncertain record rather than repeating creation.

Explicit shutdown returns the retained client when cleanup cannot complete.
Dropping the entire controller with outstanding records is last-resort quarantine,
not a release acknowledgement: neither resident owner cleanup nor dropping the
module frees that forgotten SDK allocation. A production worker must own and drain
the controller through its real lifecycle. This SDK stage covers guard ownership,
notification and snapshot metadata; immutable text capture, READ/IMPORT adapters
and automatic production lifecycle remain unimplemented.

Canonical host SDK tests obtain a Client through the real module entry binding
and route genuine Registry-issued providers and open operation scopes through the
same synchronous broker core as native System. They cover real contention,
closed roots, stale/foreign guards, preflight allocation refusal, corrupted replies
after actual creation, bounded cleanup and authoritative owner cleanup. Native
System keeps its interrupt/preemption gate across admission and the bounded
resident effect; the host adapter does not qualify that gate, SMP or a worker.

Store enablement/direction preferences in versioned System JSON. Support local
operation when preferences are read-only/unavailable, and extensions-disabled
startup. The module stays pinned for the boot session initially; disabling bridge
traffic must still cancel pending work and preserve the local clipboard.

The typed policy codec accepts version 1 JSON with optional `enabled` (default
`true`) and `direction` (`both`, `host-to-guest`, or `guest-to-host`; default
`both`). It rejects unknown fields, unsupported versions and documents exceeding
256 bytes. Policy is immutable for a transport epoch: changing it requires a new
epoch after draining or resetting owned transfers. Disabled and denied directions
avoid scanning or copying text, while protocol refusals retain bounded reply
state through allocation refusal. The codec and engine policy are implemented;
transactional System preference storage and live policy changes remain gates.

## Acceptance

Canonical `crates/mac-toolbox/tests` assertions use public Scrap calls through
real services. Test native Unicode copy/paste both ways, legacy conversion where
implemented, cross-app copying, clear/put batching, empty/large/unsupported offers,
conversion failures, no echo loops, concurrent changes and delayed replies.
Fake endpoints cover deterministic failures; private VM checks must additionally
copy real text between host editors and guest apps on each claimed host profile.

The native transport fixture has passed capability exchange, wrong-port refusal,
malformed-protocol rejection and absent-provider startup with a usable desktop.
A fresh private Xvfb and GTK QEMU have also passed real bidirectional UTF-8,
multiline and emoji text exchange through the guest policy engine. This fixture
uses bounded bootstrap execution and fixed private text: it does not admit the
production worker or connect application Scrap to that worker. Empty-text and
native Windows/macOS text exchange remain unqualified. Launcher tests pass on
Linux and native Windows; those tests do not qualify host clipboard text exchange.

Verify actual startup displays the package-owned extension icon, and a package
without artwork uses the standard extension icon. Compare the startup strip with
retained original Macintosh evidence. Canonical startup helper tests measure the
retained original 7.6.1 first-row geometry and bound pixel damage to the changed
32-pixel cell. Failure-X styling and row wrapping are native choices without an
original failed-extension capture; neither is a complete startup pixel-match
claim. Native discovery/boot captures now qualify successful package artwork,
theme fallback and a genuine initializer refusal using three dedicated INIT
probes, including exact public-SDK icon pixels, unchanged pixels outside the
cells and usable desktop input afterward. These probes qualify the resident
presentation machinery. Host Clipboard still needs its own real package/worker
activation and missing-prerequisite campaign.

Cover absent/disabled/unsupported bridges, disconnect/reconnect, timeout, malformed
messages, allocation limits, boot without host integration and stale/foreign
registration/request IDs. Verify extensions-disabled recovery. Record transfer
latency, idle work, queue bounds and Cursor Test/present responsiveness while
sharing; no new UI redraw or input-event storm is acceptable.

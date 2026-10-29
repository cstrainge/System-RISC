# Virtual host clipboard extension

Status: the external `Host Clipboard` INIT package now connects a genuine
module-owned scheduled worker, standard vdagent transport and resident Scrap.
A private Linux/X11 run of optimized RV64 System observed exact Unicode,
multiline and emoji copy/paste between Mousepad and Text Edit in both directions,
including a guest-edited suffix saved by both applications. This is bounded
evidence for that QEMU GTK profile, including four physical CPUs with
primary-only scheduling. Direction settings, disabled/invalid/read-only policy
and missing-preference creation have additional bounded native observations.
Windows, macOS, empty text, sustained pressure, live policy changes and complete
bridge shutdown remain
unverified; this is not production clipboard or SMP qualification.

This is the [P18](CORE-TOOLBOX-ROADMAP.md#p18--virtual-host-clipboard-extension)
consumer of the [P6 extension SDK](SYSTEM-EXTENSIONS.md). Module execution,
transport negotiation and a confirmed active bridge are separate states.

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
keeping the installed extension present. An absent transport device makes the
loader skip the channel driver and refuse the extension's missing dependency,
with the synchronous startup X. A present device with the wrong port name can
admit the worker, which retains the endpoint refusal asynchronously. The latter
does not repaint the already dismissed startup strip; see the readiness limit
below. Headless launches supply no graphical clipboard peer. Missing host facilities leave local Scrap functional and report the reason;
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

`mac_driver::lifecycle::Startup::defer` uses resident MLIF v1 before the
initializer returns. System commits the module and arms its actual workers,
but leaves offered interfaces unpublished and records pending startup. The
boot strip uses a hollow pending badge. Only the scheduled worker may confirm
readiness after protocol negotiation and Scrap session/storage admission;
MLIF then atomically publishes the declared interfaces. A refusal retains its
bounded status/cause and can be retried by the same admitted module. Confirming
an already active module is idempotent. Later connectivity/policy diagnostics
update status without claiming a new startup or changing module identity.
`MODULE_ADMITTED ... startup=pending`, `MODULE_ASYNC_READY` and
`MODULE_ASYNC_STATUS` distinguish these transitions in serial output.

The boot strip is dismissed before scheduled negotiation completes. Its existing
X covers synchronous initializer refusal; asynchronous refusal is currently
retained in the registry/serial status, without a visible post-desktop status
surface or repaint of that strip. That presentation requirement remains open.
A failed cleanup keeps its exact ticket and backing quarantined; quarantine is
not successful rollback or reclamation.

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

TextEdit's native clipboard boundary converts external CRLF/LF to its CR-based
editor model, and native Copy/Cut exports those breaks as LF. The shared Scrap
and vdagent payload stay byte-preserving. Earlier exact-byte round trips did not
establish line presentation: a user Windows report exposed LF drawn as a glyph.
A focused optimized native public-SDK scenario now observes normalized editor
text, actual logical lines/caret positions, Unicode, unchanged source Scrap and
oversize refusal/retry. That scenario does not qualify a Windows host round trip.

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
IMPORT, CLOSE_PIN, DISCONNECT and IMPORT_SNAPSHOT. A reply has an eight-byte status/length header;
terminal failures include the informative UTF-8 resident cause. Callers provide
512–4096 reply bytes, validated before any effect, so a session or pin is never
published without room to return its identity. READ carries at most 4088 payload
bytes per wire reply. IMPORT permits at most `1 MiB - 32` text bytes and must also
fit the resident Scrap text limit. The
[wire contract](../crates/mac-scrap-extension-interface/src/lib.rs) defines the
exact layouts. Snapshot and metadata requests are separate; INFO or a later
read failure cannot hide an already minted pin from its caller. IMPORT_SNAPSHOT
pre-admits a new pin before publication and returns the exact imported snapshot,
so a later application copy cannot be mistaken for the imported text. Revision-only
IMPORT remains available when the caller does not need that pin.

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
Earlier host-linked fixture results do not qualify native worker, interrupt,
account or arbitrary privileged-fault behavior.

`mac-driver-alloc::scrap` supplies non-copyable session and snapshot guards over
the original resolved SCEX provider. It reserves each retirement record before
OPEN, SNAPSHOT or IMPORT_SNAPSHOT can have an effect. Guard destruction only
marks that admitted record; a module-lifetime controller performs bounded cleanup
later. Child pins keep their session connected until their releases are
acknowledged. Each drain visits at most its supplied record budget, with
constant-time record removal. Busy preserves the original provider/grant for
retry. A malformed post-effect creation reply retains its uncertain record
rather than repeating creation.

The SDK now supplies bounded READ and IMPORT adapters, immutable UTF-8 capture,
and `capture_pin`, which returns its original unique pin on refusal. The bridge
retains an acknowledged import pin while retrying capture; it never repeats the
committed import. Explicit shutdown returns the retained client if cleanup
cannot complete. Dropping the controller with outstanding records is last-resort
quarantine, not a release acknowledgement or successful owner cleanup.

The pump checks the coalesced resident revision on a scheduled 16 ms cadence.
It defers a changed local revision until it has remained unchanged for 32 ms,
then captures one immutable payload. These are scheduling policy intervals,
not measured maximum transfer latency. A producer that waits longer than that
between clear and put can still expose an intermediate empty Scrap; the public
Scrap protocol has no transaction-end marker. The bridge reads unchanged text
zero times, performs no clipboard work on input events, and retains decoded
frames/output/correlated pins across bounded retries. Actual worker timed park
uses `core::time::Duration`; host I/O never runs under a module mutex or Scrap
lock. Negotiation and fetch deadlines are five seconds. Ambiguous transfer
failure requires acknowledged VBYT RESET before a new protocol epoch.

Enablement/direction preferences use resident MPRE v1, with the admitted stable
package identity obtained from the registry. Display names cannot redirect the
storage scope. The 256-byte versioned JSON document lives at
`System Folder/Preferences/7731a000000000000000000000000080.json`.
The image builder derives this same key from the actual packaged EABI identity.
LOAD and exact-byte
compare/exchange use the real Files service; a successful write requires writable
media. Comparison, writer admission and commit share its guard. Stale writes
refuse, abort failures retain their cleanup ticket, and no failure is reported
as successful persistence. The SDK exposes immutable `Document` bytes and
`Preferences::{load,save,revision}`; it supplies no request-controlled path.

The typed clipboard policy accepts version 1 with optional `enabled` (default
`true`) and `direction` (`both`, `host-to-guest`, `guest-to-host`; default `both`).
Unknown fields, unsupported versions and oversized documents refuse. The image
builder installs an explicit default; a missing document is created through real
compare/exchange. Unavailable/invalid preferences leave sharing local. The worker
checks the startup catalog revision once per second and reloads only on change;
a policy change retires the old session and resets the actual transport before
opening another epoch. Denied export observes revision metadata without reading
local text. Disabled policy performs no text observation or transfer.
There is no control-panel application yet. Live changes, concurrent preference
writers and failure recovery still need native qualification.

## Acceptance

The current bounded native observations use real optimized no_std RV64/RVA23
System and 8 GiB, with input/text traces disabled. The editor round trip uses
the standard four physical CPUs with primary-only admission; policy and worker
diagnostics explicitly select one hart. Extra physical CPUs do not establish
concurrent supervisor execution or SMP safety. The installed INIT
worker reached readiness after genuine vdagent negotiation and SCEX admission.
A fresh private Xvfb with upstream QEMU 11.1.50 GTK (`gl=off`, clipboard schema
present) and Mousepad 0.6.3 exchanged `Host café 雪 🦀`, a second line and a guest
typed suffix with Text Edit. Both editors saved exactly the expected UTF-8 bytes;
the guest file was read offline only after VM exit. The collector allowed GTK's
asynchronous paste to finish before Save. This establishes that scenario, not a
transfer-time bound, other QEMU binaries or other host profiles.

Separate existing native worker diagnostic modules exercised zero duration,
checked conversion/deadline overflow, coalesced wake, a 30 ms deadline (32,180
observed guest microseconds), early wake and cancellation of timed parks, plus
clock/park refusal inside a critical section. Real canonical publication and
acknowledgement records correlated all four normal and all three rollback worker
reclamations. This uses the actual native scheduler; it does not qualify complete
clipboard-controller shutdown, pressure or SMP.

Direction cases used actual Mousepad/Text Edit copies and exact saved bytes.
Host-to-guest preserved the host clipboard after a guest edit/copy;
guest-to-host preserved the guest's prior local Scrap after a host copy, then
exported the guest edit. Disabled/invalid policy retained local copy/paste and
saved guest text. Wrong-name endpoint refusal also left local text usable.
A missing document was created through actual MPRE/File Manager transactions and
read back after VM exit. Read-only creation refused with READ_ONLY, retained
local operation, issued no active confirmation and left no preference file;
its local text was observed in the editor screenshot, not a saved guest file.
These are boot-time policy scenarios, not concurrent-writer or live-change
qualification.

The private QEMU build omits HMP; its standard QMP `memsave` reads the existing
System UI observer for pointer acknowledgements. That debugger observation and
paced GUI actions have overhead and do not qualify timing or sustained stress.
Some rapid Finder navigation attempts stopped after opening Applications before
its layout completed, including an absent-endpoint case where the clipboard
worker never ran. A separate earlier Text Edit initialization returned only a
generic error. Error-only stage/status/cause logging is now present; later runs
did not reproduce that initialization error. Neither issue has an established
cause or fix. File → Open/gesture-completion scenarios have narrower evidence;
they do not establish rapid double-click reliability or general OS stability.

Exercise public Scrap calls through actual guest services. Remaining campaigns
include legacy conversion, cross-app copying, clear/put batching, empty/large/
unsupported offers, conversion failures, concurrent changes, delayed replies,
policy transitions and recovery. Private VM checks must copy real text between
host editors and guest apps on each claimed host profile. Codec results,
launcher probes and historical host Rust pass counts cannot substitute for this.

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
presentation machinery. Those earlier probe observations are limited to their artifacts and scenarios;
the actual Host Clipboard package's missing-prerequisite and artwork campaign
remains separate from the successful Linux editor round trip.

Cover absent/disabled/unsupported bridges, disconnect/reconnect, timeout, malformed
messages, allocation limits, boot without host integration and stale/foreign
registration/request IDs. Verify extensions-disabled recovery. Record transfer
latency, idle work, queue bounds and Cursor Test/present responsiveness while
sharing; no new UI redraw or input-event storm is acceptable.

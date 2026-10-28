# Native application panic reports

Every installed Rust application, including Finder, installs the loader's
application-lifetime service table with `mac_toolbox::panic::install` before
entering its application code and defines `mac_toolbox::panic_handler!()`.
The shared handler formats the complete `PanicInfo` (message, source file, line
and column), together with the process name and canonical process/thread IDs.
It streams UTF-8 through a 512-byte stack buffer without allocating in the
application. The complete formatted stream also goes to the serial log.

`System::crashes()` is the public client for beginning, appending, showing,
reading, scrolling and dismissing a report. Its `CRASH = 0x1001f` trap is outside
the classic 16-bit trap range. The public `toolbox-crashes` crate transports
requests; resident System services own storage, resources, windows, managed
controls, input, drawing and the clipboard operation.

System boot preparation admits one full report buffer, with the existing 1 MiB
UTF-8 retention limit and enough line-index capacity for every retained byte.
The backing belongs to System and retains no dying application account.
The fixed report layout is prepared from the renderer's geometry and the actual
System Monaco font metrics after the font manager is ready. BEGIN applies that
layout to the empty buffer; APPEND builds its index incrementally. SHOW checks
the prepared layout and never rebuilds the index by scanning the full report.

`MAX_APPEND_BYTES` is 512. The native boundary rejects a larger input before
creating its byte slice or validating UTF-8. The safe `append` facade splits
larger strings at UTF-8 boundaries, returning the final accepted reply without
an extra INFO trap. Empty append still makes one APPEND request. An individual
argument larger than `MAX_REPORT_BYTES` is refused before any effect; if a later
chunk refuses, earlier accepted chunks remain retained. System marks retention
or formatting failures explicitly. This transfer bound does not reduce the
1 MiB total report capacity. READ still returns exact requested bytes, and a
read chunk may end inside a UTF-8 scalar. Copy Report copies the complete text
when it fits the clipboard's existing limit; otherwise its button is disabled.
READ and clipboard copying can still copy a large report in one resident call;
their latency needs target measurements.

After submitting a report, the failed application waits with an empty event
mask. It runs no application handlers or drawing callbacks. System services
scrolling, Home/End, Page Up/Down, Copy Report and Close Application. Dismissal
wakes the parked application so it can request ordinary process exit. Finder's
report instead offers shell replacement through the normal Finder image and
scheduler path. This panic-abort runtime has no stack unwinder, so the report
contains the available panic diagnostics rather than a promised backtrace.

The recovery implementation uses one reusable System modal session. A report
from another recipient receives a typed busy refusal while that session is in
use. If the reporter must first cancel its own Save or core alert, SHOW can be
accepted pending with `window = 0` and `dismissed = false`. System retains the
genuine process/resource binding, flags and full immutable report snapshot;
actual prior-child and I/O acknowledgments wake deferred publication. It uses
the same report renderer and makes no application callback. Refused cleanup
retains its original ticket and capabilities. An unresponsive lower protocol
can therefore keep publication pending or quarantined; acceptance is not a
promise of a new timeout or immediate display. A refused dismissal retains the
actual resource binding, modal activity, Window identity and full snapshot.
Canonical ACK progress wakes its independent completion pump; INFO becomes
dismissed only after Window Manager proves actual disposal.

A visible report or handoff snapshot pins the actual immutable data. After
acknowledged dismissal, the saved manager releases that strong pin and retains
only its accepted capture's canonical process/thread origin and scalar state.
INFO and READ temporarily pin the latest System archive for that exact stored
origin, release the archive lock, then inspect or copy its bytes. There is no
public arbitrary archive lookup. The latest full report remains readable after
dismissal until a later accepted BEGIN reuses the sole stock. After replacement,
an old manager reports no retained bytes and READ returns not found. Refused
BEGIN preserves the previous bytes and origin; successful BEGIN withdraws the
old archive before mutation.

This origin check relies on the current sealed lifecycle: one saved report
manager per process; snapshots retain the real data Arc and block stock reuse;
a manager's next capture replaces its own metadata; and Finder handoff stops
the old process irreversibly before its replacement executes. Deferred SHOW
restores into the same validated process. Future arbitrary snapshot transfer,
executor revival or independent managers for one process must preserve those
rules or use the shared registered identity facility for capture identity.
A separate private counter must not substitute for that facility.

Finder replacement keeps the full diagnostic independently of its old process.
The old System-owned report window must acknowledge normal disposal before the
modal recipient transfers to a genuine new process owner. New UI IDs come from
the normal renderer. The healthy replacement closes the retained old report
without exiting or restarting again. Retry Finder is a separate action which
validates an existing retained preparation and its actual owner/image binding;
old report origin metadata cannot authorize a fresh restart. A boot-held, zeroed 4 MiB stack and actual platform-sized
saved CPU backing are lent uniquely to the replacement, with its original
Application/Kernel budget receipts. Backing returns only after actual scheduler
quiescence; maintenance scrubs at most one page per checkpoint before reuse.
No buddy token or physical ownership is fabricated for protected backing.
The recovered Finder keeps an internal protected allocation capability for its
minimum desktop/menu/report operation under continuing pressure. Ordinary
application launch explicitly suppresses that capability and modal activity;
launch requests can return an informative memory refusal when ordinary child
resources are unavailable. This does not guarantee arbitrary application launch
success at zero ordinary memory, or restoration of all previous folder windows.

The independent terminal diagnostic floor remains separate from this reusable
storage. Neither the report service nor shell replacement promises recovery
from arbitrary kernel corruption, exceptions or failed foreign cleanup.
System's report WIND, STR# and ICON resources use ID -16400. The 32-by-32 bomb
is the original System 7.6.1 DSAT 0 internal item -16397; extraction hashes and
reference provenance are retained in
`system/assets/crashes/classic-bomb.provenance.json`.

Selected normal-memory service paths have run in an optimized native RV64
QEMU image: physical short/long report scroll, copy and dismissal; recipient-only
metadata with denied direct window mutation; paint-refusal preservation; reset;
and exact UTF-8 READ. This does not qualify the full protected recovery graph.
Source review, formatting and compilation cannot establish its physical admission,
interrupt latency, pressure scheduling or complete modal drain behavior.
Full qualification requires the exact optimized native OS image in a private RV64
guest, including full-size Unicode capture, deferred publication after real
cancellation, actual Finder stop/restart under ordinary-memory pressure,
subsequent physical menu/input and launch refusal, full READ/copy behavior,
repeated recovery and refusal/ACK cleanup. Record artifact hashes, configuration,
production paths, observations and remaining limits. Existing host-linked Rust
service suites do not provide OS acceptance. The current source does not claim
that all transitive UI allocations are preadmitted or that the complete recovery
kit has met those gates.

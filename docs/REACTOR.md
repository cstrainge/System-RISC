# Reactor Room and native data controls

> All good systems have inspection ports, and using them should never be a
> privileged or 'magic' act.

Reactor Room is the ordinary native application that makes the memory/lifetime-model
migration observable. The stages below describe its public service contracts
and remaining integration work; the application ships incrementally with those
services.

## Stage integration

| Model stage | Resident service and public SDK | Reactor Room customer |
|---|---|---|
| 2: facility | Versioned snapshots of actual domains, owner ledgers and kind registrations; cumulative counters; stated coverage and staleness | Domains and Kinds; display migrated data and explicit unavailable facets |
| 3: teardown walk | Ephemeral bounded journal, coalesced change subscriptions, capability-checked termination, completed-walk results | Journal and Kill/Restart; actual released/failed/pending/leaked counts |
| 5–6: pages/heaps | Buddy orders, real heap/account charges, reserve/emergency-kit status and control-zone usage | Memory strata and meters; no invented budgets or armed-kit claims |
| 7: modules | Registered services/chains, outcomes, image references/extents and module domains | Registered view |
| 8: view protocol | App-owned bounded-slice sources and registered source lifetimes | Virtual Data Lists and Graph sources; journal acceptance at 10,000 rows |

The inspector ships incrementally with its services. Existing managed plain
lists may be used before the view protocol lands, as the unified design permits;
do not implement app-local stand-ins for the new controls. Unmigrated managers
must not disappear into a misleading total of zero handles or zero leaks.

The first installed slice is `apps/reactor`: a resizable owned window, managed
domain/detail scroll boxes, a managed status text, resource menus/icon/layout,
System panic reporting and the public `System::inspection()` API. Process
membership changes coalesce advisory notices; foreground activation/Refresh
obtains a fresh snapshot. No sampling timer runs in this slice. Selection changes
only the old/new visible rows and detail pane; resizing configures the same owned
objects. The palette comes from resident theme roles.

Current coverage is deliberately partial: process and module domains are enumerated,
and their ledgers contain migrated controls, scroll boxes, Standard File dialogs,
offscreen QuickDraw worlds, windows, menus, memory blocks/zones, TextEdit,
Alerts, Drag sessions/receivers, directory watches, open forks, staged data writes and their
backing/saved state. Pool details include readable release providers from the public kind
registry. Module rows come directly from their owner directory and carry their real
incarnation, label and ledger; driver requests/interfaces/devices are still
unmigrated and are not fabricated as ledger counts. System/bootstrap domains, individual object detail,
CPU/account/page/service data, Kill/Restart,
journal, preferences and the four new native controls remain staged work. There
is no claim of zero total handles, zero leaks or an armed emergency reserve.

Live evaluation note (2026-10-23): one visible Windows run showed a selected
domain/detail disagreement after pointer-placement automation timed out. The
extended scrolled-selection/reselection/refresh assertions pass on traced and
normal Linux QEMU and normal Windows headless QEMU. The live observation remains
unexplained; those isolated passes do not establish that it is fixed. Inspection
membership notices now use a distinct tag from window geometry notifications;
public SDK decoding tests pin that distinction, without claiming it explains the
observed selection/detail disagreement.

All reads go through a thin public `mac-toolbox` service. Exec chains (handlers in
order, owners, claimed evidence and revisions), handler claims and kind mappings
are ordinary public registry queries for every application; Reactor Room's Registered
view is their first customer. Their existence is not inspector-only information.
Chain entries carry the source extension/bundle's user-visible name and read in
execution order. Reactor Room renders those names as the chain; identities, source
resource, generation, evidence and revisions belong in its details. The final
resident entry has the visible name “System Native Handler”. Missing/invalid
provider labels are informative registration refusals, not raw-ID UI fallbacks.
The displayed order is the actual exec precedence: dependents before their
providers, while initialization runs providers first. Shelly precedes its CLAP
dependency; the resident native handler stays last. Show missing/cyclic dependency
paths and useful corrective actions with the relevant extension names. Boot and
dispatch order derive from declared contracts and stable identities, independently
of file/display names and directory discovery order.
The System authenticates the caller; ordinary callers can inspect themselves.
Cross-domain detail and termination require System-issued inspector authority through a general policy,
not an application-name exception, private debug trap or raw memory channel.
Reactor Room is installed under Applications, with its own resources, icon, owned
window and system-owned panic reporting. Geometry/view/sort preferences are
versioned JSON. It remains no_std until the real std platform milestone is ready.

The modular promise is that if the System cannot do something now, an extension
should be able to make it happen. The fixed core stays small; capability gaps are
extension-shaped. A feature requiring a core change to add is a design flag to
raise, rather than the default plan.

## Controls and appearance

Graph, Meter, Data List and Segmented are native, capability-queried Toolbox
controls with independently loaded, descriptively named CDEF resources. Classic
IDs and behavior stay intact. Definitions draw through ports; System owns
tracking/cancellation and owner/zone accounting. No literal palettes in Reactor Room
or the new definitions: the shared theme surface owns fills, patterns, strokes,
alpha and text roles. Series 1 shares the Meter/progress fill role.

Native additions are introduced with real customers and disposal/damage tests.
Graph modes share one bounded source contract, explicit gaps and a describe
query. Meter owns scalar view state and marked regions. Data List owns only
visible rows/view state, with sorting supplied by its source. Segmented owns
exclusive selection and keyboard tracking. Do not retain unused control stubs.

## Observation and performance

CPU time, charges and handle births/deaths are model mechanism, independent of
scheduler policy. History belongs to Reactor Room: 300 samples at one-second cadence
while frontmost. Part II §9 extends the earlier Memory-only timer rule to visible
performance graphs. Backgrounding pauses sampling/subscriptions and records a
visible gap; it never polls the whole model or interpolates missing history.

Snapshots have explicit revisions; stale reads restart like Finder scans.
Journal bursts coalesce once per event-loop turn. Row text/layout and graph
strokes are prepared on snapshot/sample change; drawing does no enumeration or
measurement. Damage is limited to affected rows, details or graph slivers.
Journal history never jumps to its end unless already there.

Kill confirms with actual ledger facts, uses the ordinary stop/walk/barrier
service, and shows its outcome honestly. Finder uses Restart; System/bootstrap
cannot be killed. Cleanup failures remain visible with their kind and pending
resources; finishing a walk attempt is not proof of successful reclamation.

The journal is System-owned, in-memory and never persisted. Its configurable
initial capacity is 10,000 events (user ruling, 2026-10-23). Host export is deferred; it will
project the same snapshots. An outline is deferred until Registered proves it
needs one. Canonical tests stay in `crates/mac-toolbox/tests`, using the public
SDK and real resident services; the app is also the visible acceptance surface.

The new control definitions follow the accepted [definition author
interface](DEFINITION-SDK.md): safe native author handlers, exhaustive messages,
platform-owned panic handling, theme roles and transparent static text. Their
unique implementation remains in each relocatable code resource.

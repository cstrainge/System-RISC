# Reactor and native data controls

Reactor is the ordinary native application that makes the memory/lifetime-model
migration observable. Its source contract is the workspace document
`reactor-and-toolbox-controls-unified.md` (2026-10-23), which supersedes its
three earlier design notes. This document records integration, not completion.

## Stage integration

| Model stage | Resident service and public SDK | Reactor customer |
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

All reads go through a thin public `mac-toolbox` service. The System authenticates
the caller; ordinary callers can inspect themselves. Cross-domain detail and
termination require System-issued inspector authority through a general policy,
not an application-name exception, private debug trap or raw memory channel.
Reactor is installed under Applications, with its own resources, icon, owned
window and system-owned panic reporting. Geometry/view/sort preferences are
versioned JSON. It remains no_std until the real std platform milestone is ready.

## Controls and appearance

Graph, Meter, Data List and Segmented are native, capability-queried Toolbox
controls with independently loaded, descriptively named CDEF resources. Classic
IDs and behavior stay intact. Definitions draw through ports; System owns
tracking/cancellation and owner/zone accounting. No literal palettes in Reactor
or the new definitions: the shared theme surface owns fills, patterns, strokes,
alpha and text roles. Series 1 shares the Meter/progress fill role.

Native additions are introduced with real customers and disposal/damage tests.
Graph modes share one bounded source contract, explicit gaps and a describe
query. Meter owns scalar view state and marked regions. Data List owns only
visible rows/view state, with sorting supplied by its source. Segmented owns
exclusive selection and keyboard tracking. Do not retain unused control stubs.

## Observation and performance

CPU time, charges and handle births/deaths are model mechanism, independent of
scheduler policy. History belongs to Reactor: 300 samples at one-second cadence
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
initial capacity is awaiting the user's choice. Host export is deferred; it will
project the same snapshots. An outline is deferred until Registered proves it
needs one. Canonical tests stay in `crates/mac-toolbox/tests`, using the public
SDK and real resident services; the app is also the visible acceptance surface.

This standalone guest application is a conformance fixture, not a Finder feature.
Its sole runtime dependency is the public `mac-toolbox` SDK. It creates the
original tracking probe's window geometry/title/procedure, hides the cursor via
the SDK and paints white retained content. The capture helper builds and packages
it as Finder only on a private disk clone. `src/package.rs` is a host packaging
tool and is never linked into the guest. Behavioral and pixel assertions remain
in the aggregate `window_interactions.rs` integration test.

With the `menus` feature, the public SDK loop also observes disabled Command-O
and Command-W as native KeyDown events. `tests/menus.rs` requires each to arrive
exactly once with the Command modifier and no menu selection, while retaining the
two expected enabled menu commands and all existing pixel comparisons.

With `deferred-close`, a separate loop uses `Windows::set_close_deferred` and
reports public SDK window liveness, updates and close notifications. The capture
helper drives real close-box cancellation/acceptance, explicit application close,
and a second window retaining default automatic disposal. Canonical assertions
in `tests/deferred_close.rs` compare retained content pixels and require accepted
deferred closes to produce exactly one request and no extra Update. The helper
removes its obsolete private boot disks after capture and retains logs/frames.

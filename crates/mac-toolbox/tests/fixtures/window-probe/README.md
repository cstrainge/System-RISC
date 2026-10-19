This standalone guest application is a conformance fixture, not a Finder feature.
Its sole runtime dependency is the public `mac-toolbox` SDK. It creates the
original tracking probe's window geometry/title/procedure, hides the cursor via
the SDK and paints white retained content. The capture helper builds and packages
it as Finder only on a private disk clone. `src/package.rs` is a host packaging
tool and is never linked into the guest. Behavioral and pixel assertions remain
in the aggregate `window_interactions.rs` integration test.

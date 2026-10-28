# Managed controls

`System::controls()` creates owned `Control` objects through the resident manager.
`create_standard(window, kind, state)` supports buttons, checkboxes and radios;
`create_scrollbar(window, state, step, page)` supports horizontal/vertical CDEF1
scrollbars. All apps use these objects or the existing managed compound
`ScrollBox`; drawing a CDEF snapshot alone does not create an interactive control.

`Control::control()` borrows a `ControlRef`; Drop disposes the object. `into_id()`
explicitly transfers its lifetime to the parent window, which retires all child
controls on close. System also cleans up controls on app exit. IDs share the
central generation facility, with a separate namespace and owner checks, so
stale Drop, reused slots and foreign-manager IDs cannot affect a new control.

Use `update`/`update_scrollbar` during window repaints to configure and include
the control exactly once. Outside a repaint, `configure` commits only changes. Later control changes
replace that object's retained drawing and damage its own bounds. No-change
configuration/polls do not invalidate content. CDEF0/CDEF1 retain ownership of
their unique pixels, validation and hit/thumb geometry.

`ControlRef::track(anchor, callback)` provides ordinary modal tracking while other
apps remain runnable and queued app events remain intact. System owns press,
leave/re-enter, release and cancellation; the callback runs outside manager locks
only for accepted actions. Push/check/radio actions occur on release inside.
The application decides checkbox/radio values after accepting the action.
Scrollbar values update with arrow/page repetition or CDEF-owned thumb mapping.
Callback errors cancel capture through an SDK guard. Disable, geometry changes
and deactivation cancel without activating the control.

For an event-driven controller, use `begin_tracking`/`poll_tracking`/`cancel` and
consume the returned action/value, never replicate hit geometry or tracking math.
Finder's Clipboard viewer uses this form; Panic Test, QuickDraw Test, ResEdit and
other Finder controls use shared SDK tracking. TextEdit selections and Finder icon dragging
are content interactions rather than standard controls.

Canonical tracking/ownership assertions live in
`crates/mac-toolbox/tests/conformance/managed_controls.rs`, executed through disk-booted RV64 ECALL tests. Host-linked control validation
is retired. Original CDEF pixel/hit fixtures remain the appearance
oracle. Custom controls, focus traversal and arbitrary action callbacks remain
separate work; this is not full classic Control Manager parity.

Push buttons may set `StandardState::flags = DEFAULT_BUTTON`. Their declared
bounds include a four-pixel margin containing the classic default outline;
CDEF0 draws the outline and excludes it from activation hit testing. The dialog
controller routes Return to its default action. Other control kinds reject the
flag. `ScrollBoxRef::row_at` resolves current resident row geometry; blank space,
partial bottom rows and the scrollbar return `None`, without painting.

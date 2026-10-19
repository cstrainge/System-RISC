# Native application panic reports

Every installed Rust application, including Finder, installs the loader's
application-lifetime service table with `unsafe { mac_toolbox::panic::install(services) }`
before entering its application code and defines `mac_toolbox::panic_handler!()`.
The shared handler formats the complete `PanicInfo` (message, source file, line,
and column), plus the process name, application ID and OS thread ID. It streams
UTF-8 through a512-byte stack buffer without allocating in the application.

`System::crashes()` is the native client API for beginning, appending, showing,
reading, scrolling and dismissing a report. Its trap is `CRASH = 0x1001f`, outside
the entire classic16-bit trap range. It neither replaces nor aliases a classic
Macintosh system-error trap. Public `toolbox-crashes` remains a thin transport;
System's internal `toolbox::crashes` owns the diagnostics, window, resources,
drawing, controls, physical input and clipboard operation.

After sending the report, the application waits with an empty event mask. It
never runs its failed event loop or draws the report. System services scrolling,
Home/End and Page Up/Down, Copy Report (Ctrl-C) and Close Application (Ctrl-W).
The application thread wakes on dismissal and asks the existing process service
to exit; other application threads continue running. Finder's report disables
Close Application because Finder termination is not supported yet; restart the
machine after a Finder panic. This is not exception recovery or unwinding.

System retains up to1MiB per report and displays an explicit warning on a
retention or formatting failure. The entire formatted stream also goes to the
serial log, including text beyond the resident retention limit. Copy Report
copies the exact UTF-8 text when it fits the clipboard's existing limit; its
button is disabled when it does not. The report shows all available `PanicInfo`,
not a backtrace: this no_std panic-abort runtime has no stack unwinder. If the
service table or resident viewer is unavailable, the handler cannot promise a
graphical display; a valid table still provides serial diagnostics. Kernel
panics and hardware traps retain their independent emergency paths.

System's report WIND, STR# and ICON resources use ID-16400. The32×32 bomb is the
unaltered bitmap from System7.6.1 DSAT0 internal item-16397, with extraction
hashes and REA evidence in `system/assets/crashes/classic-bomb.provenance.json`.
Panic Test has its own application resources and icon. Open it from Applications;
its buttons or File menu deliberately trigger a short formatted panic (Ctrl-P)
or a27KiB Unicode report (Ctrl-L).

Host acceptance lives in `crates/mac-toolbox/tests/crashes.rs` through the public
Toolbox and live resident services. `panic_graphical.rs` boots an isolated actual
OS image, invokes the installed test app, compares the original bomb's pixels,
checks scrolling, copies the long report into a different application and checks
application-only termination. Run the graphical gate after `cargo xtask image`:

```sh
cargo test -p mac-toolbox --test panic_graphical -- --ignored
SYSTEM7_PANIC_WINDOWS=1 cargo test -p mac-toolbox --test panic_graphical -- --ignored
```

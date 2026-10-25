# Real process acceptance fixtures

This private replacement Finder runs the canonical public-SDK assertions in
`tests/process_cases.rs`, including the original-observation-backed application
menu snapshot assertions in `tests/conformance/application_menu.rs`.

The transport packages three separate RV64 ELF/resource applications:

| Application | Linked reservation | Purpose |
| --- | --- | --- |
| Finder observer | 0x81000000..0x82000000 | Native service assertions and background progress |
| ProcessWorker | 0x85000000..0x86000000 | Private resources/memory/events, owned window, physical input, return |
| ProcessSpinner | 0x86000000..0x86c56000 | Three-second pure hardware-counter loop without service calls/yields |

The spinner's `rdtime` instruction is contained in its loader/architecture shim.
The canonical spinner body calls no Toolbox service between START and FINISHED.
The observer must execute between those markers on a single QEMU hart, observe
multiple independent scheduler switches, and retain its own trusted identity and
memory. A successful cooperative event loop cannot satisfy that evidence.

The observer launches the worker twice, checking independent/reissued AppId and
ThreadId, duplicate-launch activation, three invalid-launch rejections, distinct
STR2000 resources and private handle payloads across switches, separate private
Application1 queue events, foreign-window access denial, hide/restore/focus,
protected original-layout application menus, routed physical input, return and
relaunch. The worker deliberately leaves its window and handle live at return;
process teardown must reclaim them. Input is supplied only after guest readiness.

Run `cargo test --locked -p mac-toolbox --test processes -- --ignored --nocapture`
after a fresh `cargo xtask image`. Python only packages a private disk, boots it,
sends keys and records artifacts. Canonical acceptance stays in Rust. The visible
VM and original DIST artifacts are never input targets or modified disks.

Scope: one OS thread per app, one hart, bounded fixed nonoverlapping image slots;
this does not establish MMU address isolation, arbitrary relocation, additional
worker threads, multi-hart safety or classic 68k application execution.

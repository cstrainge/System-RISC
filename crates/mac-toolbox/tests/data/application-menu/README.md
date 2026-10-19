# Original System 7.6.1 application menu

These are runtime observations from an owned headless q800 raw clone, the original
Quadra650 ROM, and the installed original System7.6.1/Finder. `provenance.json`
records source identities, transport/actions, original media unchanged checks,
and owned VM exit. No original media or visible VM was modified.

Two minimal cooperative Retro68 probe apps were launched by the original Finder:
ZuluProbe first, then AlphaProbe. They have different windows and ordinary File >
Quit commands. They exercise the original Process Manager and system app menu;
they do not implement a replacement app menu.

`original.json` contains ten decoded original MenuHandle snapshots, read from the
current app's real MenuList at documented low-memory address 0xA1C. Raw RAM and
full screenshots remain under `working/application-menu-reference` and
`results/application-menu-reference`, with digests in provenance. The compact PNGs
are literal crops of screenshots, except the eight full-screen state snapshots.
The crop bounds are global half-open image coordinates. The varying clock and
most mouse-over-title pixels are excluded from popup crops. The literal pointer
tail remains at global x619..625/y20..22; mask [619,20,626,23] for renderer-only
comparison. The row-highlight crop also retains its pointer over the row label.

Observed contracts:

- System-owned menu id -16489; title is an icon token, not an application name.
- At640x480, menu title starts x601; popup interior right edge is633 exclusive.
  The interior begins y20. Frame extends one pixel outside, shadow another pixel.
- First three commands are `Hide <foreground application name>`, `Hide Others`,
  `Show All`, followed by a separator and the alphabetically ordered running apps.
  `AlphaProbe`, `Finder`, `ZuluProbe` establishes sorting independently of launch.
- Command and separator rows are16px; application rows are18px. Thus heights82,
  100 and118 correspond to1,2 and3 running apps. Labels appear14px after the
  popup interior's left edge for commands,34px for apps, with a16px small-icon
  slot at+16 and checkmark near+2. Raw item mark18 identifies the foreground app.
- Hide and Hide Others are enabled only while another visible app exists.
  Show All is enabled iff at least one app is hidden.
- Hiding Alpha returns to the immediately preceding foreground Finder and removes
  Alpha's window. Hidden apps remain in the menu and selectable, with dimmed small
  icons; raw menu text style remains0. Hiding others from Zulu also hides Finder's
  desktop icons, not only its windows.
- Selecting hidden Zulu restores and activates it while Alpha stays hidden.
- Show All restores hidden apps without changing Zulu's foreground/checkmark.
- Quitting foreground Zulu removes its menu row and returns to preceding Finder;
  Alpha remains running. Activating and quitting Alpha leaves Finder alone again.
- Highlighting an app row inverts its full18px row, icon and text.

Scope limits: three apps, ASCII names with distinct initial letters, one window
per probe; no collation/case ties, name truncation, overflow/scroll menus,
Desk Accessories, modal/suspend failure, crashed apps, or exhaustive MRU fallback
permutations. The screenshots establish the listed state changes; there is no
claim of timer preemption in the original cooperative system. Native Rust apps
will use the requested actual OS-thread model instead.

Reproduce: compile `../../fixtures/application-menu-original` using the pinned
`scripts/retro68.py` runner, then use
`System7/scripts/capture_original_application_menu.py start` and its explicit
point/down/up/key/capture/menus stages, ending with `stop`. The full action trace
is hashed by provenance. This is an investigation transport, not a replacement
for canonical Rust public-SDK acceptance tests.
